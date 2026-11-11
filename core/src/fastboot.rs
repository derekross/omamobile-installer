// SPDX-License-Identifier: MIT
//! The fastboot protocol, and the only way this crate writes to a phone.
//!
//! Protocol: AOSP `system/core/fastboot/README.md` (Apache-2.0). The host
//! sends an ASCII command; the phone answers packets of at most 256 bytes
//! starting `OKAY`, `FAIL`, `DATA<8 hex>`, `INFO` or `TEXT`. A download is
//! `download:<8 hex size>`, the phone's `DATA<8 hex size>`, the bytes, then
//! `OKAY`; `flash:<partition>` writes what was downloaded.
//!
//! The command layer ([`Client`]) is private. [`Session`] is the public face,
//! and it is a fixed sequence, not a toolbox:
//!
//! ```text
//! connect (read-only checks) -> flash_boot x N, in the manifest's order
//!   -> begin_userdata -> flash_userdata_segment x M -> finish_userdata
//!   -> activate_and_reboot
//! ```
//!
//! What it can send: `getvar:`, `download:`, `flash:<p>` where `<p>` is a
//! [`Partition`] of the verified release (slot B's boot images, `userdata`),
//! `set_active:b` and `reboot`. There is no code for `erase`, `format`,
//! `oem`, `flashing`, another slot or a caller-chosen partition name. Bytes
//! to write come only as [`VerifiedImage`] or under a [`VerifiedRootImage`].
//! Before every write the phone's serial number and product are read again
//! and must still be the ones pinned at `connect`.
use crate::bundle::{VerifiedImage, VerifiedRelease, VerifiedRootImage};
use crate::hash::sha256_hex;
use crate::manifest::{Manifest, Partition};
use crate::sparse;
use crate::{Error, Result};

/// The fastboot interface: vendor class 0xff, subclass 0x42, protocol 0x03.
pub const USB_CLASS: u8 = 0xff;
pub const USB_SUBCLASS: u8 = 0x42;
pub const USB_PROTOCOL: u8 = 0x03;
/// Google's USB vendor ID, for device filters.
pub const USB_VENDOR_GOOGLE: u16 = 0x18d1;

/// Bytes handed to the transport at a time during a download. A multiple of
/// every bulk packet size (64, 512, 1024), so no short packet ends the
/// transfer early.
pub const SEND_CHUNK: usize = 1 << 20;
const MAX_INFO_PACKETS: usize = 100_000;

/// A USB (or fake) link to a phone in fastboot mode: one bulk OUT and one
/// bulk IN endpoint. Futures need not be `Send`, so a browser's can be used.
#[allow(async_fn_in_trait)]
pub trait Transport {
    /// Write `data`; the number of bytes the phone took.
    async fn send(&mut self, data: &[u8]) -> Result<usize>;
    /// One packet from the phone.
    async fn recv(&mut self) -> Result<Vec<u8>>;
}

/// A transport lent for a few read-only questions ([`probe`]).
impl<T: Transport + ?Sized> Transport for &mut T {
    async fn send(&mut self, data: &[u8]) -> Result<usize> {
        (**self).send(data).await
    }
    async fn recv(&mut self) -> Result<Vec<u8>> {
        (**self).recv().await
    }
}

/// Which phone: the SHA-256 of the serial number fastboot reports. The
/// install is pinned to one, and every later step compares against it. It
/// is a digest so that nothing holding it prints a serial number by
/// accident; it still identifies the phone, so treat it as private.
#[derive(Clone, PartialEq, Eq)]
pub struct PhoneId(String);

impl PhoneId {
    /// `None` for an empty serial: a phone that tells none can't be pinned.
    pub fn of_serial(serial: &str) -> Option<Self> {
        (!serial.is_empty()).then(|| PhoneId(sha256_hex(serial.as_bytes())))
    }
}

impl core::fmt::Debug for PhoneId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PhoneId(..)")
    }
}

/// What a phone in fastboot mode says about itself, locked or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneReport {
    pub id: PhoneId,
    pub product: String,
    pub unlocked: bool,
    /// `a` or `b`.
    pub current_slot: String,
    /// The current slot's bootloader version.
    pub bootloader: String,
    pub baseband: Option<String>,
    /// Both slots' bootloader versions (a, b), when the phone reports them.
    pub slot_bootloaders: Option<(String, String)>,
}

/// `version-bootloader` for one slot among `getvar:all`'s lines, under any
/// of the spellings a bootloader may use.
fn slot_bootloader(all: &[String], slot: char) -> Option<String> {
    all.iter().find_map(|line| {
        let rest = line.strip_prefix("version-bootloader")?;
        let mut c = rest.chars();
        let sep = c.next()?;
        (matches!(sep, '-' | '_' | ':') && c.next()? == slot)
            .then(|| c.as_str().strip_prefix(':').map(|v| v.trim().to_string()))
            .flatten()
    })
}

/// Ask a phone in fastboot mode what it is. Sends `getvar:` commands and
/// nothing else, so it is safe on a locked phone and before any decision.
pub async fn probe<T: Transport>(transport: &mut T) -> Result<PhoneReport> {
    let mut client = Client {
        transport,
        log: Vec::new(),
    };
    let id = PhoneId::of_serial(&client.getvar("serialno").await?).ok_or_else(|| {
        Error::Phone("the phone tells no serial number, so an install can't be tied to it".into())
    })?;
    let product = client.getvar("product").await?;
    let unlocked = client.getvar("unlocked").await? == "yes";
    let current_slot = client
        .getvar("current-slot")
        .await?
        .trim_start_matches('_')
        .to_string();
    if current_slot != "a" && current_slot != "b" {
        return Err(Error::Phone(format!(
            "the phone reports current slot '{current_slot}'"
        )));
    }
    let bootloader = client.getvar("version-bootloader").await?;
    if bootloader.is_empty() {
        return Err(Error::Phone(
            "the phone reports no version-bootloader".into(),
        ));
    }
    let baseband = client
        .getvar("version-baseband")
        .await
        .ok()
        .filter(|v| !v.is_empty());
    let all = client.getvar_all().await?;
    let slot_bootloaders = slot_bootloader(&all, 'a').zip(slot_bootloader(&all, 'b'));
    Ok(PhoneReport {
        id,
        product,
        unlocked,
        current_slot,
        bootloader,
        baseband,
        slot_bootloaders,
    })
}

enum Reply {
    Okay(String),
    Data(u64),
}

struct Client<T> {
    transport: T,
    log: Vec<String>,
}

impl<T: Transport> Client<T> {
    async fn read_reply(&mut self, command: &str, info: &mut Vec<String>) -> Result<Reply> {
        for _ in 0..MAX_INFO_PACKETS {
            let packet = self.transport.recv().await?;
            if packet.len() < 4 || packet.len() > 256 {
                return Err(Error::Protocol(format!(
                    "a {}-byte answer to '{command}'",
                    packet.len()
                )));
            }
            let text = String::from_utf8_lossy(&packet[4..]).into_owned();
            match &packet[..4] {
                b"OKAY" => return Ok(Reply::Okay(text)),
                b"FAIL" => {
                    return Err(Error::DeviceFail {
                        command: command.to_string(),
                        message: text,
                    })
                }
                b"INFO" | b"TEXT" => info.push(text),
                b"DATA" => {
                    let size = (text.len() == 8)
                        .then(|| u64::from_str_radix(&text, 16).ok())
                        .flatten();
                    return size.map(Reply::Data).ok_or_else(|| {
                        Error::Protocol(format!("a malformed DATA answer to '{command}'"))
                    });
                }
                _ => return Err(Error::Protocol(format!("an unknown answer to '{command}'"))),
            }
        }
        Err(Error::Protocol(format!("'{command}' never finished")))
    }

    async fn command(&mut self, command: &str, info: &mut Vec<String>) -> Result<Reply> {
        debug_assert!(command.len() <= 64, "the original protocol's command limit");
        self.log.push(command.to_string());
        let sent = self.transport.send(command.as_bytes()).await?;
        if sent != command.len() {
            return Err(Error::Transport(format!("'{command}' was not sent whole")));
        }
        self.read_reply(command, info).await
    }

    async fn simple(&mut self, command: &str) -> Result<String> {
        match self.command(command, &mut Vec::new()).await? {
            Reply::Okay(text) => Ok(text),
            Reply::Data(_) => Err(Error::Protocol(format!("DATA in answer to '{command}'"))),
        }
    }

    async fn getvar(&mut self, name: &str) -> Result<String> {
        Ok(self
            .simple(&format!("getvar:{name}"))
            .await?
            .trim()
            .to_string())
    }

    /// `getvar:all`: the `key:value` INFO lines.
    async fn getvar_all(&mut self) -> Result<Vec<String>> {
        let mut info = Vec::new();
        match self.command("getvar:all", &mut info).await? {
            Reply::Okay(_) => Ok(info),
            Reply::Data(_) => Err(Error::Protocol("DATA in answer to 'getvar:all'".into())),
        }
    }

    async fn download(&mut self, data: &[u8]) -> Result<()> {
        let command = format!("download:{:08x}", data.len());
        match self.command(&command, &mut Vec::new()).await? {
            Reply::Data(n) if n == data.len() as u64 => {}
            Reply::Data(n) => {
                return Err(Error::ShortDownload {
                    wanted: data.len() as u64,
                    accepted: n,
                })
            }
            Reply::Okay(_) => {
                return Err(Error::Protocol(format!(
                    "OKAY instead of DATA in answer to '{command}'"
                )))
            }
        }
        let mut done = 0u64;
        for chunk in data.chunks(SEND_CHUNK) {
            let sent = self.transport.send(chunk).await?;
            done += sent as u64;
            if sent != chunk.len() {
                return Err(Error::ShortDownload {
                    wanted: data.len() as u64,
                    accepted: done,
                });
            }
        }
        match self.read_reply(&command, &mut Vec::new()).await? {
            Reply::Okay(_) => Ok(()),
            Reply::Data(_) => Err(Error::Protocol("DATA after a download".into())),
        }
    }
}

fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => t.parse().ok(),
    }
}

/// What the person running the install has said about the phone.
#[derive(Debug, Clone, Default)]
pub struct PhoneOptions {
    /// Slot B is the current slot and holds Omarchy Mobile. Without it a
    /// phone running from slot B is refused: stock Android runs there after
    /// a system update.
    pub reinstall: bool,
    /// When the bootloader reports no per-slot versions: the version the
    /// owner checked on the other slot. It must equal the current slot's.
    pub bootloader_slots_confirmed: Option<String>,
    /// Do every check, send no write: each is recorded in the log as
    /// `would: ...` instead.
    pub dry_run: bool,
}

/// What the read-only checks found, for the confirmation screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneFacts {
    pub product: String,
    pub current_slot: String,
    pub bootloader: String,
    pub baseband: Option<String>,
    pub max_download_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Checks passed; the next boot image to write.
    Boot(usize),
    Userdata {
        image_len: u64,
        segments: u32,
    },
    UserdataDone,
    Rebooted,
    /// A write failed. Nothing more is sent on this session.
    Failed,
}

pub struct Session<T: Transport> {
    client: Client<T>,
    release: VerifiedRelease,
    serial: String,
    facts: PhoneFacts,
    state: State,
    dry_run: bool,
}

impl<T: Transport> core::fmt::Debug for Session<T> {
    // No serial number in debug output
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Session<T> {
    /// Open a session on one phone and run every read-only check. Nothing is
    /// written. `expected_serial` is the serial number of the USB device the
    /// person chose; the phone must report the same one.
    pub async fn connect(
        transport: T,
        release: &VerifiedRelease,
        expected_serial: &str,
        options: PhoneOptions,
    ) -> Result<Self> {
        let expected = PhoneId::of_serial(expected_serial)
            .ok_or_else(|| Error::Refused("no serial number to pin the phone to".into()))?;
        Self::connect_pinned(transport, release, &expected, options).await
    }

    /// As [`Self::connect`], for a phone already known by its [`PhoneId`]
    /// (the one an earlier step looked at).
    pub async fn connect_pinned(
        transport: T,
        release: &VerifiedRelease,
        expected: &PhoneId,
        options: PhoneOptions,
    ) -> Result<Self> {
        let m: &Manifest = release.manifest();
        let mut client = Client {
            transport,
            log: Vec::new(),
        };
        let serial = client.getvar("serialno").await?;
        if PhoneId::of_serial(&serial).as_ref() != Some(expected) {
            return Err(Error::Refused(
                "the phone that answered is not the one that was chosen (serial numbers differ)"
                    .into(),
            ));
        }
        let product = client.getvar("product").await?;
        if product != m.codename {
            let name = m.device_name.as_deref().unwrap_or("");
            let shown = if product.is_empty() {
                "unknown"
            } else {
                product.as_str()
            };
            return Err(Error::Phone(format!(
                "the phone is '{shown}', not {} ({name}): wrong release or wrong phone",
                m.codename
            )));
        }
        if client.getvar("unlocked").await? != "yes" {
            return Err(Error::Phone(
                "the bootloader is locked: unlock it first (this erases the phone)".into(),
            ));
        }
        let current_slot = client
            .getvar("current-slot")
            .await?
            .trim_start_matches('_')
            .to_string();
        if current_slot != "a" && current_slot != "b" {
            return Err(Error::Phone(format!(
                "the phone reports current slot '{current_slot}'"
            )));
        }
        let bootloader = client.getvar("version-bootloader").await?;
        if bootloader.is_empty() {
            return Err(Error::Phone(
                "the phone reports no version-bootloader".into(),
            ));
        }
        // Setting slot B active runs slot B's bootloader: both must match.
        let all = client.getvar_all().await?;
        let per_slot = |slot: char| slot_bootloader(&all, slot);
        match (per_slot('a'), per_slot('b'), &options.bootloader_slots_confirmed) {
            (Some(a), Some(b), _) if a == b => {}
            (Some(a), Some(b), _) => return Err(Error::Phone(format!("the bootloader slots differ (a: {a}, b: {b})"))),
            (_, _, Some(confirmed)) if *confirmed == bootloader => {}
            (_, _, Some(confirmed)) => return Err(Error::Phone(format!("the other slot was confirmed as {confirmed}, but slot {current_slot} has {bootloader}"))),
            _ => return Err(Error::Phone(format!("the phone only reports slot {current_slot}'s bootloader ({bootloader}); the other slot must be checked and confirmed"))),
        }
        if current_slot == "b" && !options.reinstall {
            return Err(Error::Phone("the phone runs from slot b. If stock Android is there, installing would overwrite it; if it is Omarchy Mobile, say so (reinstall)".into()));
        }
        let max_download_size = parse_size(&client.getvar("max-download-size").await?)
            .ok_or_else(|| Error::Phone("the phone reports no max-download-size".into()))?;
        if max_download_size < sparse::MIN_SEGMENT {
            return Err(Error::Phone(format!(
                "max-download-size {max_download_size} is too small"
            )));
        }
        // Each image must fit its slot B partition, a footer image exactly.
        for w in &m.boot_writes {
            let name = w.partition.name();
            let psize = parse_size(&client.getvar(&format!("partition-size:{name}")).await?)
                .ok_or_else(|| {
                    Error::Phone(format!("the phone reports no partition-size for {name}"))
                })?;
            if w.size > psize {
                return Err(Error::Phone(format!(
                    "{} is {} bytes, larger than {name} ({psize} bytes)",
                    w.source, w.size
                )));
            }
            if w.partition_size.is_some_and(|want| want != psize) {
                return Err(Error::Phone(format!(
                    "{name} is {psize} bytes, not the {} the image's AVB footer was built for",
                    w.partition_size.unwrap_or(0)
                )));
            }
            if w.size > max_download_size {
                return Err(Error::Phone(format!(
                    "{} is larger than the phone's max-download-size",
                    w.source
                )));
            }
        }
        let baseband = client
            .getvar("version-baseband")
            .await
            .ok()
            .filter(|v| !v.is_empty());
        let facts = PhoneFacts {
            product,
            current_slot,
            bootloader,
            baseband,
            max_download_size,
        };
        Ok(Self {
            client,
            release: release.clone(),
            serial,
            facts,
            state: State::Boot(0),
            dry_run: options.dry_run,
        })
    }

    pub fn facts(&self) -> &PhoneFacts {
        &self.facts
    }
    pub fn max_download_size(&self) -> u64 {
        self.facts.max_download_size
    }
    /// Every command sent so far, and in a dry run every write held back.
    pub fn log(&self) -> &[String] {
        &self.client.log
    }
    /// The plan in words, for the confirmation screen.
    pub fn plan(&self) -> Vec<String> {
        let m = self.release.manifest();
        let mut out: Vec<String> = m
            .boot_writes
            .iter()
            .map(|w| {
                format!(
                    "flash {} {} ({} bytes)",
                    w.partition.name(),
                    w.source,
                    w.size
                )
            })
            .collect();
        out.push(format!(
            "flash {} {} (erases userdata)",
            m.root_image.partition.name(),
            m.root_image.name
        ));
        out.push("set_active b".into());
        out.push("reboot".into());
        out
    }
    pub fn into_transport(self) -> T {
        self.client.transport
    }
    /// Whether this session holds every write back.
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
    pub(crate) fn release_id(&self) -> &str {
        self.release.id()
    }

    fn fail<V>(&mut self, e: Error) -> Result<V> {
        self.state = State::Failed;
        Err(e)
    }

    /// The phone on the other end is still the one the checks passed on.
    async fn still_pinned(&mut self) -> Result<()> {
        let codename = self.release.manifest().codename.clone();
        if self.client.getvar("serialno").await? != self.serial
            || self.client.getvar("product").await? != codename
        {
            return Err(Error::Refused(
                "the phone changed during the install; nothing more is written".into(),
            ));
        }
        Ok(())
    }

    /// The one place a write is sent: `partition` can only be a name the
    /// verified manifest's rules produced.
    async fn write(&mut self, partition: &Partition, data: &[u8]) -> Result<()> {
        if data.len() as u64 > self.facts.max_download_size {
            return Err(Error::Refused(format!(
                "{} bytes is over the phone's max-download-size",
                data.len()
            )));
        }
        self.still_pinned().await?;
        if self.dry_run {
            self.client.log.push(format!(
                "would: download {} bytes, flash:{}",
                data.len(),
                partition.name()
            ));
            return Ok(());
        }
        self.client.download(data).await?;
        self.client
            .simple(&format!("flash:{}", partition.name()))
            .await?;
        Ok(())
    }

    /// Write the next boot image of the plan to its slot B partition.
    pub async fn flash_boot(&mut self, image: &VerifiedImage<'_>) -> Result<()> {
        let State::Boot(next) = self.state else {
            return Err(Error::Refused(
                "boot images are written first, once each".into(),
            ));
        };
        if image.release != self.release.id() {
            return Err(Error::Refused(
                "the image was verified against another release".into(),
            ));
        }
        if image.index != next {
            return Err(Error::Refused(
                "boot images are written in the manifest's order".into(),
            ));
        }
        let partition = self.release.manifest().boot_writes[next].partition.clone();
        match self.write(&partition, image.bytes).await {
            Ok(()) => {
                self.state = State::Boot(next + 1);
                Ok(())
            }
            Err(e) => self.fail(e),
        }
    }

    /// After the last boot image: `userdata` is next. `image_len` is the raw
    /// root image's size; it must fit the partition.
    pub async fn begin_userdata(
        &mut self,
        proof: &VerifiedRootImage,
        image_len: u64,
    ) -> Result<()> {
        let boots = self.release.manifest().boot_writes.len();
        if self.state != State::Boot(boots) {
            return Err(Error::Refused(
                "userdata is written after every boot image, once".into(),
            ));
        }
        if proof.release != self.release.id() {
            return Err(Error::Refused(
                "the root image was verified against another release".into(),
            ));
        }
        let size =
            parse_size(&self.client.getvar("partition-size:userdata").await?).ok_or_else(|| {
                Error::Phone("the phone reports no partition-size for userdata".into())
            })?;
        if image_len > size {
            return Err(Error::Phone(format!(
                "the root image is {image_len} bytes, larger than userdata ({size} bytes)"
            )));
        }
        self.state = State::Userdata {
            image_len,
            segments: 0,
        };
        Ok(())
    }

    /// One sparse image of the root image, as [`crate::install::UserdataStream`] cuts them.
    pub async fn flash_userdata_segment(&mut self, segment: &[u8]) -> Result<()> {
        let State::Userdata {
            image_len,
            segments,
        } = self.state
        else {
            return Err(Error::Refused("no userdata write is open".into()));
        };
        // A sparse image of exactly this root image, nothing else
        let header_ok = segment.len() >= 28
            && segment[0..4] == 0xed26_ff3a_u32.to_le_bytes()
            && u32::from_le_bytes(segment[12..16].try_into().unwrap()) as u64
                * u32::from_le_bytes(segment[16..20].try_into().unwrap()) as u64
                == image_len;
        if !header_ok {
            return self.fail(Error::Refused(
                "not a sparse image of the root image".into(),
            ));
        }
        let partition = self.release.manifest().root_image.partition.clone();
        match self.write(&partition, segment).await {
            Ok(()) => {
                self.state = State::Userdata {
                    image_len,
                    segments: segments + 1,
                };
                Ok(())
            }
            Err(e) => self.fail(e),
        }
    }

    pub async fn finish_userdata(&mut self) -> Result<()> {
        match self.state {
            State::Userdata { segments, .. } if segments > 0 => {
                self.state = State::UserdataDone;
                Ok(())
            }
            _ => Err(Error::Refused("userdata has not been written".into())),
        }
    }

    /// Make slot B active and restart, only after every write succeeded.
    pub async fn activate_and_reboot(&mut self) -> Result<()> {
        if self.state != State::UserdataDone {
            return Err(Error::Refused(
                "slot B is made active only after every write succeeded".into(),
            ));
        }
        if let Err(e) = self.still_pinned().await {
            return self.fail(e);
        }
        if self.dry_run {
            self.client.log.push("would: set_active:b".into());
            self.client.log.push("would: reboot".into());
            self.state = State::Rebooted;
            return Ok(());
        }
        if let Err(e) = self.client.simple("set_active:b").await {
            return self.fail(e);
        }
        match self.client.simple("reboot").await {
            Ok(_) => {
                self.state = State::Rebooted;
                Ok(())
            }
            Err(e) => self.fail(e),
        }
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Rebooted
    }
}
