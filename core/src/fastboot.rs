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
//! That is the order of Omarchy Mobile's `tools/install-phone`: slot B's
//! boot images in the descriptor's order, `userdata`, `set_active:b` (what
//! `fastboot --set-active=b` sends), `reboot`. Nothing else is done about
//! the slots: no other slot is made active, and no slot is marked.
//!
//! What it can send: `getvar:`, `download:`, `flash:<p>` where `<p>` is a
//! [`Partition`] of the verified release (slot B's boot images, `userdata`),
//! `set_active:b` and `reboot`; and one question outside the session,
//! [`unlock_ability`], which sends `flashing get_unlock_ability` and changes
//! nothing on the phone. That list is closed, and checked again at the one
//! place a command leaves (`Client::allowed`): a `flash:` leaves only for a
//! partition on the session's own list, which `connect` builds from the
//! verified release (the descriptor's `BOOT_IMAGES` with `_b`, and
//! `userdata`), and the questions outside a session have an empty list.
//! There is no code for `erase`, `format`, `oem`, any other `flashing`
//! command (`flashing unlock` among them), another slot or a caller-chosen
//! partition name. Bytes to write come only as [`VerifiedImage`] or under a
//! [`VerifiedRootImage`]. Before every write the phone's serial number and
//! product are read again and must still be the ones pinned at `connect`.
//!
//! The gate does not lean on the layer above it. A partition must also be
//! on the gate's own fixed list (slot B's boot images and `userdata`), so a
//! name the [`Partition`] type should never have let through is refused
//! there too. A client with an empty list (the questions outside a session,
//! and every dry-run session) can send nothing that writes or stages a
//! write: no `flash:`, no `download:`, no `set_active:b`, no `reboot`. And
//! a `download:` carries exactly eight hex digits.
//!
//! A session that stops partway says how far it got ([`Session::reach`]),
//! from what the phone confirmed: the flow's stop is worded from that.
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

/// The one command of the `flashing` family this crate sends, and it only
/// reads: whether the phone would take an unlock command ("OEM unlocking" in
/// Android's Developer options). Sent by [`unlock_ability`] and nothing else.
pub const GET_UNLOCK_ABILITY: &str = "flashing get_unlock_ability";

/// The gate's own list of what may ever be flashed, apart from any
/// session's: slot B's boot images and `userdata`, by name. It does not rest
/// on [`Partition`]: a name that got past the manifest's rules is refused
/// here all the same.
fn on_the_fixed_list(partition: &str) -> bool {
    partition == "userdata"
        || partition
            .strip_suffix("_b")
            .is_some_and(|image| crate::manifest::SLOT_B_IMAGES.contains(&image))
}

/// `download:` and the size as exactly eight hex digits, as this crate
/// writes it.
fn is_download(command: &str) -> bool {
    command
        .strip_prefix("download:")
        .is_some_and(|size| size.len() == 8 && size.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Every command this crate may put on the wire, and nothing more. The
/// `flashing` entry is the whole command, compared exactly: `flashing unlock`,
/// `flashing lock`, `flashing unlock_critical` and every `oem` command are
/// not on the list, and neither is anything else that isn't named here.
/// `flash:` is allowed only for a partition in `flashable`, compared whole.
///
/// `flashable` is the list of the client the command leaves through. It is
/// empty for a client that only asks (the look at the phone, the unlock
/// question) and for a dry-run session's, and for those nothing that writes
/// or stages a write passes at all: no `flash:`, no `download:`, no
/// `set_active:b`, no `reboot`. For a session that may write, a `flash:`
/// passes only for a partition that is on the session's list and on the
/// gate's own fixed one ([`on_the_fixed_list`]), so the gate and the
/// [`Partition`] type each hold without the other.
fn allowed(command: &str, flashable: &[String]) -> bool {
    if let Some(partition) = command.strip_prefix("flash:") {
        return flashable.iter().any(|p| p == partition) && on_the_fixed_list(partition);
    }
    if command == GET_UNLOCK_ABILITY || command.starts_with("getvar:") {
        return true;
    }
    // What stages a write or changes the phone: only where a flash can leave
    !flashable.is_empty()
        && (command == "set_active:b" || command == "reboot" || is_download(command))
}

/// How far a session's writes got, from what the phone confirmed. A
/// `download:` alone changes nothing on a phone (it is held in memory until
/// a `flash:`), so only `flash:`, `set_active:b` and `reboot` count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteReach {
    /// No command that changes the phone was sent.
    Nothing,
    /// Slot B's boot images: `done` of `of` were written and confirmed.
    /// `in_doubt`: the next one's `flash:` was sent and not confirmed, so
    /// that partition may be partly written. `userdata` is untouched.
    BootImages { done: u32, of: u32, in_doubt: bool },
    /// Every boot image is written, and `userdata` was begun: `pieces`
    /// sparse images were confirmed, and one more may be partly written.
    Userdata { pieces: u32 },
    /// Every image is written and confirmed. Slot B was not made active.
    Written,
    /// As `Written`; `set_active:b` was sent and not confirmed.
    SlotInDoubt,
    /// Slot B is the active slot. `reboot` was not confirmed.
    SlotSet,
    /// Everything, and the phone confirmed the restart.
    Rebooted,
}

/// What a walk of the root image through a session found: the raw image's
/// size, `userdata`'s size on the phone, and how many sparse images it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserdataWalk {
    pub image_len: u64,
    pub partition_size: u64,
    pub pieces: u32,
}

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
///
/// A line with an empty value is a slot the phone did not report, as in
/// Omarchy Mobile's `tools/install-phone` (`[[ -n $bl_a && -n $bl_b ]]`):
/// two empty answers are not "both reported and equal". This is the one
/// place a per-slot version is read, for the flow's judge ([`probe`]) and
/// for every session (`connect`).
fn slot_bootloader(all: &[String], slot: char) -> Option<String> {
    all.iter().find_map(|line| {
        let rest = line.strip_prefix("version-bootloader")?;
        let mut c = rest.chars();
        let sep = c.next()?;
        (matches!(sep, '-' | '_' | ':') && c.next()? == slot)
            .then(|| c.as_str().strip_prefix(':').map(|v| v.trim().to_string()))
            .flatten()
            .filter(|version| !version.is_empty())
    })
}

/// Whether a locked phone would take the unlock command: the answer to
/// [`GET_UNLOCK_ABILITY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockAbility {
    /// "OEM unlocking" is switched on: the unlock command can work.
    Allowed,
    /// "OEM unlocking" is not switched on: the phone refuses the unlock
    /// command until it is.
    NotAllowed,
    /// The phone did not answer the question, or not in a form this crate
    /// knows. Nothing is concluded from that.
    Unknown,
}

/// `get_unlock_ability: 0` or `get_unlock_ability: 1`, and nothing else.
fn unlock_ability_line(line: &str) -> Option<bool> {
    match line.trim().strip_prefix("get_unlock_ability:")?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Ask a phone in fastboot mode whether it would take an unlock command.
/// Sends [`GET_UNLOCK_ABILITY`], once, and nothing else: it reads, and
/// changes nothing on the phone. It never sends the unlock command.
///
/// There is no error to handle: a phone that answers FAIL, something else
/// than a plain yes or no, both, or nothing gives [`UnlockAbility::Unknown`],
/// and the caller goes on without the answer. Because a phone that never
/// answers could answer late, send this as the last question on a transport.
pub async fn unlock_ability<T: Transport>(transport: &mut T) -> UnlockAbility {
    let mut client = Client::reading(transport);
    let mut lines = Vec::new();
    match client.command(GET_UNLOCK_ABILITY, &mut lines).await {
        Ok(Reply::Okay(text)) => lines.push(text),
        Ok(Reply::Data(_)) | Err(_) => return UnlockAbility::Unknown,
    }
    let mut answers = lines.iter().filter_map(|l| unlock_ability_line(l));
    match (answers.next(), answers.next()) {
        (Some(true), None) => UnlockAbility::Allowed,
        (Some(false), None) => UnlockAbility::NotAllowed,
        _ => UnlockAbility::Unknown,
    }
}

/// Ask a phone in fastboot mode what it is. Sends `getvar:` commands and
/// nothing else, so it is safe on a locked phone and before any decision.
pub async fn probe<T: Transport>(transport: &mut T) -> Result<PhoneReport> {
    let mut client = Client::reading(transport);
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
    /// The partitions a `flash:` may name. Empty outside a [`Session`].
    flashable: Vec<String>,
}

impl<T> Client<T> {
    /// A client that can ask and can't flash.
    fn reading(transport: T) -> Self {
        Client {
            transport,
            log: Vec::new(),
            flashable: Vec::new(),
        }
    }

    /// A session's client. One that holds every write back (a dry run)
    /// gets no list, like a client that only asks: the session never forms
    /// a write, and the gate would not pass one.
    fn for_session(transport: T, flashable: Vec<String>, dry_run: bool) -> Self {
        Client {
            transport,
            log: Vec::new(),
            flashable: if dry_run { Vec::new() } else { flashable },
        }
    }
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
            // What a phone says ends up in sentences a front end prints:
            // nothing a terminal acts on is kept of it, from here on
            let text = crate::text::plain(&String::from_utf8_lossy(&packet[4..]));
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
        // The one place a command leaves: nothing off the list is sent
        if !allowed(command, &self.flashable) {
            return Err(Error::Refused(format!(
                "'{command}' is not a command this installer sends"
            )));
        }
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
///
/// The default is a dry run ([`PhoneOptions::default`]): a session that
/// writes is one whose maker wrote `dry_run: false`.
#[derive(Debug, Clone)]
pub struct PhoneOptions {
    /// Slot B is the current slot and holds Omarchy Mobile. Without it a
    /// phone running from slot B is refused: stock Android runs there after
    /// a system update.
    pub reinstall: bool,
    /// When the bootloader reports no per-slot versions: the version the
    /// owner checked on the other slot. It must equal the current slot's.
    pub bootloader_slots_confirmed: Option<String>,
    /// Do every check, send no write: each is recorded in the log as
    /// `would: ...` instead. On by default.
    pub dry_run: bool,
    /// The maintainer's override, for a dry run only: go on when the
    /// bootloader reports only the current slot's version, without knowing
    /// the other. It never passes two versions that are known to differ.
    /// Without `dry_run` the session is refused before anything is sent
    /// ([`Error::OverrideOutsideDryRun`]).
    pub maintainer_skip_bootloader_slot_check: bool,
}

impl Default for PhoneOptions {
    /// A dry run, with nothing said about the phone. Options that write are
    /// never what a caller gets without asking for them.
    fn default() -> Self {
        PhoneOptions {
            reinstall: false,
            bootloader_slots_confirmed: None,
            dry_run: true,
            maintainer_skip_bootloader_slot_check: false,
        }
    }
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
    /// The bootloader-slot check was skipped by the maintainer's override.
    /// Only ever true together with `dry_run`.
    slot_check_skipped: bool,
    /// The phone's size for each boot image's partition, in the plan's order.
    boot_partition_sizes: Vec<u64>,
    /// The root image's size, when it was checked against `userdata` before
    /// the first write ([`Self::preflight_userdata`]).
    userdata_preflight: Option<u64>,
    userdata_walk: Option<UserdataWalk>,
    reach: WriteReach,
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
        // The override exists for dry runs. A session that could write
        // refuses it whole, before a question is asked of the phone.
        if options.maintainer_skip_bootloader_slot_check && !options.dry_run {
            return Err(Error::OverrideOutsideDryRun);
        }
        let m: &Manifest = release.manifest();
        // The closed list of what this session may flash: the verified
        // release's writes, which are the descriptor's BOOT_IMAGES with _b,
        // and userdata. Nothing a person typed or a phone said is on it.
        let mut flashable: Vec<String> = m
            .boot_writes
            .iter()
            .map(|w| w.partition.name().to_string())
            .collect();
        flashable.push(m.root_image.partition.name().to_string());
        let mut client = Client::for_session(transport, flashable, options.dry_run);
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
        let mut slot_check_skipped = false;
        match (per_slot('a'), per_slot('b'), &options.bootloader_slots_confirmed) {
            (Some(a), Some(b), _) if a == b => {}
            (Some(a), Some(b), _) => return Err(Error::Phone(format!("the bootloader slots differ (a: {a}, b: {b})"))),
            (_, _, Some(confirmed)) if *confirmed == bootloader => {}
            (_, _, Some(confirmed)) => return Err(Error::Phone(format!("the other slot was confirmed as {confirmed}, but slot {current_slot} has {bootloader}"))),
            // Only the unknown case, and only in a dry run (checked above too)
            _ if options.maintainer_skip_bootloader_slot_check && options.dry_run => {
                slot_check_skipped = true;
                client.log.push("skipped: the bootloader-slot check (maintainer override, dry run only)".into());
            }
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
        let mut boot_partition_sizes = Vec::with_capacity(m.boot_writes.len());
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
            boot_partition_sizes.push(psize);
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
            slot_check_skipped,
            boot_partition_sizes,
            userdata_preflight: None,
            userdata_walk: None,
            reach: WriteReach::Nothing,
        })
    }

    /// The phone's size for each boot image's partition, in the plan's
    /// order, as read by the checks.
    pub fn boot_partition_sizes(&self) -> &[u64] {
        &self.boot_partition_sizes
    }
    /// What the walk of the root image found, once it was begun.
    pub fn userdata_walk(&self) -> Option<UserdataWalk> {
        self.userdata_walk
    }
    /// The partitions the gate lets this session's `flash:` name: the
    /// verified release's (its boot images with `_b`, and `userdata`) in
    /// the session that writes, and none at all in a dry run, where the
    /// gate passes no command that writes or stages a write.
    pub fn may_flash(&self) -> &[String] {
        &self.client.flashable
    }
    /// How far this session's writes got. In a dry run: nothing, always.
    pub fn reach(&self) -> WriteReach {
        self.reach
    }

    async fn userdata_size(&mut self) -> Result<u64> {
        parse_size(&self.client.getvar("partition-size:userdata").await?)
            .ok_or_else(|| Error::Phone("the phone reports no partition-size for userdata".into()))
    }

    /// Before the first write: the root image, `image_len` bytes unpacked
    /// (as a dry run of the same release found), must fit `userdata`. The
    /// boot images were checked at `connect`; with this every image is
    /// checked against its partition before anything is written. The root
    /// image that is then streamed must be that size.
    pub async fn preflight_userdata(&mut self, image_len: u64) -> Result<()> {
        if self.state != State::Boot(0) {
            return Err(Error::Refused(
                "userdata's size is checked before the first write".into(),
            ));
        }
        let size = self.userdata_size().await?;
        if image_len > size {
            return Err(Error::Phone(format!(
                "the root image is {image_len} bytes, larger than userdata ({size} bytes)"
            )));
        }
        self.userdata_preflight = Some(image_len);
        Ok(())
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
    /// Whether the maintainer's override skipped the bootloader-slot check.
    /// Such a session is a dry run, and says nothing about a real install.
    pub fn slot_check_skipped(&self) -> bool {
        self.slot_check_skipped
    }
    /// The last gate before anything that changes the phone is sent: a
    /// session whose bootloader-slot check was skipped never writes.
    fn may_write(&self) -> Result<()> {
        if self.slot_check_skipped {
            return Err(Error::OverrideOutsideDryRun);
        }
        Ok(())
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
        self.may_write()?;
        self.client.download(data).await?;
        // From here the partition may be partly written until the phone
        // confirms: recorded before the command leaves
        let boots = self.release.manifest().boot_writes.len() as u32;
        let before = self.reach;
        self.reach = match before {
            WriteReach::Nothing => WriteReach::BootImages {
                done: 0,
                of: boots,
                in_doubt: true,
            },
            WriteReach::BootImages { done, of, .. } if done < of => WriteReach::BootImages {
                done,
                of,
                in_doubt: true,
            },
            WriteReach::BootImages { .. } => WriteReach::Userdata { pieces: 0 },
            other => other,
        };
        self.client
            .simple(&format!("flash:{}", partition.name()))
            .await?;
        self.reach = match self.reach {
            WriteReach::BootImages { done, of, .. } => WriteReach::BootImages {
                done: done + 1,
                of,
                in_doubt: false,
            },
            WriteReach::Userdata { pieces } => WriteReach::Userdata { pieces: pieces + 1 },
            other => other,
        };
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
        if self
            .userdata_preflight
            .is_some_and(|checked| checked != image_len)
        {
            return Err(Error::Refused(format!(
                "the root image is {image_len} bytes, not the size that was checked against userdata before the first write"
            )));
        }
        let size = self.userdata_size().await?;
        if image_len > size {
            return Err(Error::Phone(format!(
                "the root image is {image_len} bytes, larger than userdata ({size} bytes)"
            )));
        }
        self.userdata_walk = Some(UserdataWalk {
            image_len,
            partition_size: size,
            pieces: 0,
        });
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
                if let Some(walk) = self.userdata_walk.as_mut() {
                    walk.pieces = segments + 1;
                }
                Ok(())
            }
            Err(e) => self.fail(e),
        }
    }

    pub async fn finish_userdata(&mut self) -> Result<()> {
        match self.state {
            State::Userdata { segments, .. } if segments > 0 => {
                self.state = State::UserdataDone;
                if !self.dry_run {
                    self.reach = WriteReach::Written;
                }
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
        if let Err(e) = self.may_write() {
            return self.fail(e);
        }
        self.reach = WriteReach::SlotInDoubt;
        if let Err(e) = self.client.simple("set_active:b").await {
            // FAIL is the phone's answer: the slot was not changed
            if matches!(e, Error::DeviceFail { .. }) {
                self.reach = WriteReach::Written;
            }
            return self.fail(e);
        }
        self.reach = WriteReach::SlotSet;
        match self.client.simple("reboot").await {
            Ok(_) => {
                self.state = State::Rebooted;
                self.reach = WriteReach::Rebooted;
                Ok(())
            }
            Err(e) => self.fail(e),
        }
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Rebooted
    }
}

#[cfg(test)]
mod tests {
    //! The private command layer, where the list of commands is enforced.
    //! No USB: a transport that records what it is handed.
    use super::*;
    use pollster::block_on;

    #[derive(Default)]
    struct Recorder {
        sent: Vec<String>,
    }
    impl Transport for Recorder {
        async fn send(&mut self, data: &[u8]) -> Result<usize> {
            self.sent.push(String::from_utf8_lossy(data).into_owned());
            Ok(data.len())
        }
        async fn recv(&mut self) -> Result<Vec<u8>> {
            Ok(b"OKAY".to_vec())
        }
    }

    /// A session's list for the Pixel 7 Pro's descriptor.
    const OURS: [&str; 6] = [
        "boot_b",
        "init_boot_b",
        "vendor_boot_b",
        "vendor_kernel_boot_b",
        "dtbo_b",
        "userdata",
    ];

    fn try_command(command: &str) -> (Result<String>, Vec<String>) {
        let mut client = Client {
            transport: Recorder::default(),
            log: Vec::new(),
            flashable: OURS.iter().map(|p| p.to_string()).collect(),
        };
        let result = block_on(client.simple(command));
        assert_eq!(client.log, client.transport.sent);
        (result, client.transport.sent)
    }

    #[test]
    fn of_the_flashing_and_oem_commands_only_the_one_question_is_sent() {
        assert_eq!(GET_UNLOCK_ABILITY, "flashing get_unlock_ability");
        let (result, sent) = try_command(GET_UNLOCK_ABILITY);
        assert!(result.is_ok());
        assert_eq!(sent, [GET_UNLOCK_ABILITY]);
        for refused in [
            "flashing unlock",
            "flashing lock",
            "flashing unlock_critical",
            "flashing lock_critical",
            "flashing get_unlock_bootloader_nonce",
            "flashing",
            "flashing ",
            // Not the question itself with something after or before it
            "flashing get_unlock_ability ",
            "flashing get_unlock_ability unlock",
            "flashing get_unlock_ability;flashing unlock",
            " flashing get_unlock_ability",
            "flashing  get_unlock_ability",
            "flashing:get_unlock_ability",
            "FLASHING GET_UNLOCK_ABILITY",
            "flashing get_unlock_ability\0flashing unlock",
            "oem unlock",
            "oem lock",
            "oem device-info",
            "oem",
            "oem get_unlock_ability",
        ] {
            let (result, sent) = try_command(refused);
            assert!(
                matches!(&result, Err(Error::Refused(why)) if why.contains("not a command this installer sends")),
                "{refused:?}: {result:?}"
            );
            assert!(sent.is_empty(), "{refused:?} reached the transport");
        }
    }

    #[test]
    fn nothing_off_the_list_is_sent() {
        for (command, ok) in [
            ("getvar:product", true),
            ("download:00000010", true),
            ("flash:boot_b", true),
            ("set_active:b", true),
            ("reboot", true),
            ("set_active:a", false),
            ("set_active:b ", false),
            ("reboot-bootloader", false),
            ("reboot bootloader", false),
            ("erase:userdata", false),
            ("format:userdata", false),
            ("boot", false),
            ("continue", false),
            ("upload", false),
            ("", false),
            // A download says its size as eight hex digits, and nothing else
            ("download:", false),
            ("download:zzzzzzzz", false),
            ("download:0000001", false),
            ("download:000000100", false),
            ("download:ffffffffff", false),
            ("download:00000010 ", false),
            ("download:0000001G", false),
            ("download:+0000010", false),
            ("DOWNLOAD:00000010", false),
            ("download:0000ABCD", true),
            ("reboot ", false),
            ("REBOOT", false),
        ] {
            let (result, sent) = try_command(command);
            assert_eq!(result.is_ok(), ok, "{command:?}");
            assert_eq!(sent.len(), usize::from(ok), "{command:?}");
        }
    }

    #[test]
    fn a_flash_leaves_only_for_a_partition_on_the_sessions_list() {
        for ours in OURS {
            let (result, sent) = try_command(&format!("flash:{ours}"));
            assert!(result.is_ok(), "{ours}");
            assert_eq!(sent, [format!("flash:{ours}")]);
        }
        for refused in [
            // Slot A, and the same names without a slot
            "boot_a",
            "init_boot_a",
            "vendor_boot_a",
            "vendor_kernel_boot_a",
            "dtbo_a",
            "boot",
            "init_boot",
            "vendor_boot",
            "vendor_kernel_boot",
            "dtbo",
            // What an install never touches, with and without a slot
            "bootloader",
            "bootloader_a",
            "bootloader_b",
            "radio",
            "radio_a",
            "radio_b",
            "vbmeta",
            "vbmeta_a",
            "vbmeta_b",
            "vbmeta_system_b",
            "vbmeta_vendor_b",
            "devinfo",
            "persist",
            "efs",
            "efs_backup",
            "modem",
            "modem_a",
            "modem_b",
            "modem_userdata",
            "misc",
            "super",
            "abl_b",
            "bl1_b",
            "bl2_b",
            "bl31_b",
            "pbl_b",
            // Made up, and near misses of our own names
            "omamobile_made_up",
            "made_up_b",
            "userdata_b",
            "userdata_a",
            "boot_b ",
            " boot_b",
            "boot_b\0",
            "boot_b:raw",
            "BOOT_B",
            "boot_b,boot_a",
            "",
        ] {
            let (result, sent) = try_command(&format!("flash:{refused}"));
            assert!(
                matches!(&result, Err(Error::Refused(why)) if why.contains("not a command this installer sends")),
                "{refused:?}: {result:?}"
            );
            assert!(sent.is_empty(), "flash:{refused:?} reached the transport");
        }
        // A client that only asks (the look at the phone, the unlock
        // question) has no list: it can flash nothing at all
        for ours in OURS {
            let mut client = Client::reading(Recorder::default());
            assert!(block_on(client.simple(&format!("flash:{ours}"))).is_err());
            assert!(client.transport.sent.is_empty());
        }
    }

    /// Every command that changes a phone or stages a change, for [`OURS`].
    fn every_write() -> Vec<String> {
        let mut writes: Vec<String> = OURS.iter().map(|p| format!("flash:{p}")).collect();
        writes.extend(["download:00000010", "set_active:b", "reboot"].map(String::from));
        writes
    }

    #[test]
    fn the_gate_holds_without_the_partition_type() {
        // A list that names what no session may write, as it would look if
        // the manifest's rules or the `Partition` type had let it through:
        // the gate has its own fixed list, and refuses each all the same
        let wrong = [
            "persist",
            "efs",
            "efs_backup",
            "devinfo",
            "modem_a",
            "modem_b",
            "modem_userdata",
            "boot_a",
            "vbmeta_b",
            "bootloader_b",
            "radio_b",
            "userdata_b",
            "",
        ];
        for name in wrong {
            assert!(!on_the_fixed_list(name), "{name:?}");
            let mut client = Client {
                transport: Recorder::default(),
                log: Vec::new(),
                flashable: wrong.iter().map(|p| p.to_string()).collect(),
            };
            let result = block_on(client.simple(&format!("flash:{name}")));
            assert!(matches!(result, Err(Error::Refused(_))), "{name:?}");
            assert!(client.transport.sent.is_empty(), "flash:{name:?} was sent");
        }
        // The fixed list is slot B's boot images and userdata, whole
        for ours in OURS {
            assert!(on_the_fixed_list(ours), "{ours}");
        }
        assert_eq!(
            crate::manifest::SLOT_B_IMAGES.len() + 1,
            OURS.len(),
            "a new image name must be added to this test's list too"
        );
    }

    #[test]
    fn a_client_that_only_asks_sends_no_command_that_writes() {
        // The look at the phone and the unlock question
        for command in every_write() {
            let mut client = Client::reading(Recorder::default());
            let result = block_on(client.simple(&command));
            assert!(
                matches!(&result, Err(Error::Refused(why)) if why.contains("not a command this installer sends")),
                "{command:?}: {result:?}"
            );
            assert!(client.transport.sent.is_empty(), "{command:?} was sent");
            assert!(client.log.is_empty());
        }
        let mut client = Client::reading(Recorder::default());
        assert!(block_on(client.simple("getvar:product")).is_ok());
        assert!(block_on(client.simple(GET_UNLOCK_ABILITY)).is_ok());
        assert_eq!(
            client.transport.sent,
            ["getvar:product", GET_UNLOCK_ABILITY]
        );
    }

    #[test]
    fn a_dry_run_sessions_client_sends_no_command_that_writes() {
        let list = || OURS.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        // Held back by the session before they are ever formed; and if one
        // were formed all the same, the gate refuses it
        for command in every_write() {
            let mut client = Client::for_session(Recorder::default(), list(), true);
            assert!(client.flashable.is_empty());
            let result = block_on(client.simple(&command));
            assert!(matches!(result, Err(Error::Refused(_))), "{command:?}");
            assert!(client.transport.sent.is_empty(), "{command:?} was sent");
        }
        // The same list in a session that may write
        for command in every_write() {
            let mut client = Client::for_session(Recorder::default(), list(), false);
            assert!(block_on(client.simple(&command)).is_ok(), "{command:?}");
            assert_eq!(client.transport.sent, [command]);
        }
        // Either can ask
        let mut client = Client::for_session(Recorder::default(), list(), true);
        assert!(block_on(client.simple("getvar:all")).is_ok());
    }

    #[test]
    fn a_slot_whose_bootloader_version_is_empty_is_a_slot_not_reported() {
        let lines = |a: &str, b: &str| {
            vec![
                "version-bootloader:fake-1.0".to_string(),
                format!("version-bootloader{a}"),
                format!("version-bootloader{b}"),
            ]
        };
        // Each spelling, with a value: reported
        for (a, b) in [("-a:", "-b:"), ("_a:", "_b:"), (":a:", ":b:")] {
            let all = lines(&format!("{a}fake-1.0"), &format!("{b} fake-1.1 "));
            assert_eq!(slot_bootloader(&all, 'a').as_deref(), Some("fake-1.0"));
            assert_eq!(slot_bootloader(&all, 'b').as_deref(), Some("fake-1.1"));
            // The same lines with nothing after the colon, or only spaces:
            // not reported, so never "both reported and equal"
            for empty in ["", " ", "\t"] {
                let all = lines(&format!("{a}{empty}"), &format!("{b}{empty}"));
                assert_eq!(slot_bootloader(&all, 'a'), None, "{a:?} {empty:?}");
                assert_eq!(slot_bootloader(&all, 'b'), None, "{b:?} {empty:?}");
            }
        }
    }

    #[test]
    fn the_default_options_are_a_dry_run() {
        let options = PhoneOptions::default();
        assert!(options.dry_run);
        assert!(!options.reinstall && !options.maintainer_skip_bootloader_slot_check);
        assert_eq!(options.bootloader_slots_confirmed, None);
    }

    /// A phone that answers every command with the packets it was given.
    struct Says(Vec<Vec<u8>>);
    impl Transport for Says {
        async fn send(&mut self, data: &[u8]) -> Result<usize> {
            Ok(data.len())
        }
        async fn recv(&mut self) -> Result<Vec<u8>> {
            Ok(self.0.remove(0))
        }
    }

    #[test]
    fn every_kind_of_answer_is_made_fit_to_print_where_it_is_read() {
        // ESC sequences, a carriage return, a NUL, and a byte that is not UTF-8
        let hostile = b"\x1b[2J\x1b]0;t\x07\rok\0\xff";
        let shown = "\\u{1b}[2J\\u{1b}]0;t\\u{7} ok\\u{0}\u{fffd}";
        let packet = |kind: &[u8]| [kind, &hostile[..]].concat();
        // OKAY: a getvar's value
        let mut client = Client::reading(Says(vec![packet(b"OKAY")]));
        assert_eq!(block_on(client.getvar("product")).unwrap(), shown);
        // FAIL: the phone's message, in the error a front end prints
        let mut client = Client::reading(Says(vec![packet(b"FAIL")]));
        let err = block_on(client.getvar("product")).unwrap_err();
        assert!(
            matches!(&err, Error::DeviceFail { message, .. } if message == shown),
            "{err:?}"
        );
        assert!(!err.to_string().chars().any(char::is_control));
        // INFO and TEXT: the lines of getvar:all
        let mut client = Client::reading(Says(vec![
            packet(b"INFO"),
            packet(b"TEXT"),
            b"OKAY".to_vec(),
        ]));
        assert_eq!(block_on(client.getvar_all()).unwrap(), [shown, shown]);
    }

    #[test]
    fn the_answer_is_a_plain_yes_or_no_or_nothing_is_concluded() {
        assert_eq!(unlock_ability_line("get_unlock_ability: 1"), Some(true));
        assert_eq!(unlock_ability_line("get_unlock_ability: 0"), Some(false));
        assert_eq!(unlock_ability_line(" get_unlock_ability:1 "), Some(true));
        for odd in [
            "",
            "1",
            "0",
            "yes",
            "get_unlock_ability:",
            "get_unlock_ability: 2",
            "get_unlock_ability: 10",
            "get_unlock_ability: yes",
            "unlock_ability: 1",
            "unlocked: 1",
        ] {
            assert_eq!(unlock_ability_line(odd), None, "{odd:?}");
        }
    }
}
