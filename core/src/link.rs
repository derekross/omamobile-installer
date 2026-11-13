// SPDX-License-Identifier: MIT
//! After the write: the install link, from the phone's first start to a
//! phone that is ready (Omarchy Mobile's INSTALL-LINK-PLAN, stage L6).
//!
//! The flow ([`crate::flow::Flow`]) ends its write with the phone restarting
//! into Omarchy Mobile. A release whose root image offers the install link
//! (`manifest.json`'s `install_link`) goes on here: [`crate::flow::Flow::link`]
//! hands over what the write pinned, and a [`LinkFlow`] takes the install
//! through the link's states, as screens, the same for every front end:
//!
//! ```text
//! first-start -> pair -> setup -> radio-backup -> factory-send -> provision -> restart -> check -> ready
//! ```
//!
//! What is said on the wire is the protocol crate's host machine
//! ([`omamobile_link::host::Host`]): the closed list of commands, the nonce of
//! every `hello`, the serial proof against the serial the write pinned, the
//! token, the image's window, the backup's files before `backup-commit`, and
//! a lost link taken up again where the phone says it is. This file decides
//! the order of the steps, every stop and every sentence a person reads about
//! them, and carries out nothing itself: the front end lends a
//! [`LinkTransport`] (the USB function, or a pretend phone) and a
//! [`LinkShell`] (the clock, randomness, the backup directory and the factory
//! image's file), and draws the screens.
//!
//! The owner's factory image (decided on 2026-11-13): the file the flow
//! checked against the release's signed build index, which the owner
//! downloaded from Google (or the installer downloaded for them from Google's
//! own address, after Google's terms), is read from where it is, a piece at a
//! time, and sent to the phone. The phone checks it again, whole, against the
//! same SHA-256 from the index before anything opens it, and extracts and
//! installs its firmware itself. Nothing of Google's is hosted, extracted on
//! the computer or kept by the installer beyond the owner's own file.
use crate::flow::{AnswerField, OwnerAnswers, Retry, Screen, ScreenStep, ISSUES};
use crate::{Error, Result};
use omamobile_link::answers::{Answers, Field, Problem, ValidAnswers};
use omamobile_link::digest::{from_hex32, to_hex};
use omamobile_link::host::{
    Command, Event, Host, HostConfig, HostEffect, HostInput, HostOutcome, HostOutput, Stop,
};
use omamobile_link::msg::{
    BackupPart, BackupState, Code, ErrorCode, ErrorMsg, FactoryBegin, FactoryState, JobState,
    Progress, Stage, Status, Token, What,
};
use omamobile_link::Entropy;
use serde::{Deserialize, Serialize};

#[cfg(feature = "pretend-link")]
pub mod pretend;

pub use omamobile_link::PROTOCOL;

/// How long the phone may take to show the link after the write, and after
/// the planned restart, before the screen says what to check (§4: the first
/// start grows the root to fill the phone's storage). The flow keeps waiting.
pub const LINK_WAIT_MS: u64 = 10 * 60_000;
/// How often a front end looks for the link while it is not on the bus.
pub const LOOK_MS: u64 = 500;
/// The longest a read waits before the machine's clock is looked at again.
const READ_MS: u64 = 500;
/// A data payload no larger than this is asked for on the computer side: the
/// phone answers with the smaller of this and its own (1 MiB by default).
pub const MAX_DATA: u32 = omamobile_link::frame::DEFAULT_MAX_DATA;
/// The link's USB interface (§4): vendor class, subclass 0x6f, protocol
/// 0x01, and this string. Found by the interface, never by vendor and
/// product: `1d6b:0104` is also what the phone shows without the link.
pub const USB_CLASS: u8 = 0xff;
pub const USB_SUBCLASS: u8 = 0x6f;
pub const USB_PROTOCOL: u8 = 0x01;
pub const USB_INTERFACE: &str = "Omarchy Mobile install link";
/// A write the phone has not taken in this long is a lost link (§5.7).
pub const WRITE_WAIT_MS: u64 = 30_000;
/// The read buffer for one frame at [`MAX_DATA`] (§5.1, D6).
pub fn read_buffer_len() -> usize {
    omamobile_link::frame::read_buffer_len(MAX_DATA)
}
/// The installer's name in `hello`.
pub const INSTALLER: &str = "omamobile-install";

/// Moves the link's frames: the phone's link interface (§4) found and
/// claimed, one frame per bulk transfer each way. The front end's: USB on a
/// computer, WebUSB in the page, a pretend phone in a test.
///
/// What the plan's §5.7 asks of it: one read is always pending on the IN
/// endpoint while the link is up, also while a frame is being written (a
/// reader thread on a computer, a `transferIn` always outstanding in a
/// browser), so `recv` hands over what that reader completed; and a write
/// the phone has not taken within 30 s is a lost link, said as an error.
#[allow(async_fn_in_trait)]
pub trait LinkTransport {
    /// Look for the link and claim it. `Ok(true)`: up; `Ok(false)`: not on
    /// the bus now. Called only while not connected.
    async fn connect(&mut self) -> Result<bool>;
    /// One frame, one bulk transfer. An error is a lost link.
    async fn send(&mut self, frame: &[u8]) -> Result<()>;
    /// The next frame the pending read completed, waiting at most
    /// `wait_ms`. `Ok(None)`: nothing came. An error is a lost link.
    async fn recv(&mut self, wait_ms: u64) -> Result<Option<Vec<u8>>>;
    /// Let the link go (it was lost, or the run is over).
    fn disconnect(&mut self);
}

/// What the front end does for the link besides moving frames. The core
/// has no clock, no randomness and no files of its own.
#[allow(async_fn_in_trait)]
pub trait LinkShell {
    /// Milliseconds on a clock that does not go back.
    fn now_ms(&mut self) -> u64;
    /// Seconds since 1970 (UTC), for the date in the backup's `NV-BACKUP`.
    fn wall_clock_s(&mut self) -> u64;
    async fn sleep(&mut self, ms: u64);
    /// The operating system's source: each `hello`'s nonce.
    fn random(&mut self, buf: &mut [u8]);
    /// False: the front end is leaving (the window closed, the input ended).
    /// The run then returns, and the record says where it was.
    fn go_on(&mut self) -> bool;
    /// A new screen while the phone works by itself (`action` `wait`).
    fn screen(&mut self, screen: &Screen);
    /// How far a step is: `what` is `link-wait`, `firstboot`, `backup`,
    /// `factory-send`, `factory-check` or `provision`; `total` 0 is unknown.
    fn progress(&mut self, what: &'static str, done: u64, total: u64);
    /// Keep the record in the work directory (mode 600), so that a second
    /// run goes on ([`LinkRecord`]). Never in a report.
    fn keep(&mut self, record: &LinkRecord) -> Result<()>;
    /// How many bytes of `NAME.img` a try before this one left (0: none).
    async fn backup_open(&mut self, name: &str, size: u64) -> Result<u64>;
    /// Cut `NAME.img` at `offset` and write `data` there.
    async fn backup_write(&mut self, name: &str, offset: u64, data: &[u8]) -> Result<()>;
    /// SHA-256 of the whole `NAME.img`, read back from where it is stored.
    async fn backup_digest(&mut self, name: &str) -> Result<[u8; 32]>;
    /// Delete `NAME.img`: it is not the partition.
    async fn backup_discard(&mut self, name: &str) -> Result<()>;
    /// Write `SHA256SUMS`, `partitions.txt` and `NV-BACKUP` as given, each
    /// mode 600, and sync them and the directory (mode 700). Only then is
    /// `backup-commit` sent (§5.5).
    async fn backup_finish(&mut self, files: &BackupFiles) -> Result<()>;
    /// `len` bytes of the owner's factory image at `offset`, read from the
    /// file: never the whole image in memory.
    async fn factory_read(&mut self, offset: u64, len: u32) -> Result<Vec<u8>>;
}

/// The backup's last three files, in `tools/nv-backup`'s format, so that
/// `tools/nv-backup --check DIR` reads the directory later.
#[derive(Clone, PartialEq, Eq)]
pub struct BackupFiles {
    /// `<sha256>  NAME.img` per partition, in the phone's order.
    pub sha256sums: String,
    /// `NAME DEVICE ro=1 mounted=0 SIZE` per partition. The link does not
    /// name the phone's kernel device, so DEVICE is `-`.
    pub partitions_txt: String,
    pub nv_backup: String,
}

impl core::fmt::Debug for BackupFiles {
    // The digests identify the partitions' contents: not for a log (§9)
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("BackupFiles { .. }")
    }
}

/// What the write hands the link: who the phone is, what goes to it.
#[derive(Clone)]
pub struct LinkStart {
    /// The serial number the writing session pinned in fastboot. It never
    /// crosses the link: the phone's serial proof is compared with it.
    pub serial: String,
    /// The release's codename and label, for the screens and `NV-BACKUP`.
    pub device: String,
    pub device_name: String,
    pub release: String,
    /// The owner's factory image, as the signed build index describes it.
    pub factory: FactoryBegin,
    /// The owner's answers, checked by the link's rules. `None` in a run
    /// that goes on from a record: they were in memory only, and are asked
    /// again if the phone does not have them yet.
    pub answers: Option<ValidAnswers>,
    /// A token a run before this one kept.
    pub token: Option<Token>,
    /// The front end already knows where the radio backup goes (the command
    /// line's `--backup-dir`).
    pub backup_place: bool,
    /// The installer's version, for `hello`.
    pub version: String,
}

impl core::fmt::Debug for LinkStart {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "LinkStart {{ {} {}, serial, answers and token withheld }}",
            self.device, self.release
        )
    }
}

/// The record a front end keeps in its work directory, so that a second run
/// (the window was closed, the computer restarted) goes on with no new code
/// and no new write. It holds the pinned serial (for the proof) and the
/// token: the work directory is mode 700 and its files 600, and the record
/// is never part of a report or a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkRecord {
    /// 1.
    pub format: u32,
    pub serial: String,
    pub device: String,
    pub device_name: String,
    pub release: String,
    pub factory_size: u64,
    pub factory_sha256: String,
    pub factory_build: String,
    pub factory_name: String,
    /// The session token, hex; `None` before pairing and after `finish`.
    pub token: Option<String>,
    /// The phone's stage, as last heard.
    pub stage: String,
    /// The link was finished: nothing is left to go on with.
    pub finished: bool,
}

impl core::fmt::Debug for LinkRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "LinkRecord {{ {} {} at {}, serial and token withheld }}",
            self.device, self.release, self.stage
        )
    }
}

impl LinkRecord {
    pub const FORMAT: u32 = 1;

    /// Read a record a run before this one kept. `None`: not a record.
    pub fn parse(bytes: &[u8]) -> Option<LinkRecord> {
        let r: LinkRecord = serde_json::from_slice(bytes).ok()?;
        (r.format == Self::FORMAT
            && !r.serial.is_empty()
            && from_hex32(&r.factory_sha256).is_some()
            && r.token.as_deref().is_none_or(|t| from_hex32(t).is_some()))
        .then_some(r)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// The start of a run that goes on from this record. The answers are
    /// not in it: they are asked again if the phone has none yet.
    pub fn start(&self, version: &str, backup_place: bool) -> Option<LinkStart> {
        Some(LinkStart {
            serial: self.serial.clone(),
            device: self.device.clone(),
            device_name: self.device_name.clone(),
            release: self.release.clone(),
            factory: FactoryBegin {
                size: self.factory_size,
                sha256: from_hex32(&self.factory_sha256)?,
                build: self.factory_build.clone(),
                name: self.factory_name.clone(),
            },
            answers: None,
            token: match &self.token {
                Some(t) => Some(Token(from_hex32(t)?)),
                None => None,
            },
            backup_place,
            version: version.to_string(),
        })
    }
}

/// The owner's answers as the link carries them: the password hashed as
/// `openssl passwd -6` does (with `salt`, 16 characters from the operating
/// system's randomness: [`crate::firstboot::salt_from`]), a PIN when it is
/// digits only, and what install-phone asks besides. The link's own rules
/// (first boot's) are applied here; their problems name the flow's fields.
pub fn link_answers(
    answers: &OwnerAnswers,
    extras: &crate::firstboot::Extras<'_>,
    salt: &str,
) -> Result<ValidAnswers> {
    let pin = answers.password.bytes().all(|b| b.is_ascii_digit());
    let a = Answers {
        username: answers.username.clone(),
        password_hash: crate::firstboot::sha512_crypt(answers.password.as_bytes(), salt)?,
        credential: Some(if pin { "pin" } else { "password" }.into()),
        hostname: Some(answers.hostname.clone()),
        timezone: Some(answers.timezone.clone()),
        wifi_ssid: answers.wifi.as_ref().map(|w| w.0.clone()),
        wifi_psk: answers.wifi.as_ref().and_then(|w| w.1.clone()),
        ssh_keys: extras
            .ssh_key
            .map(|k| vec![k.to_string()])
            .unwrap_or_default(),
        full_name: extras.full_name.map(str::to_string),
        email: extras.email.map(str::to_string),
    };
    a.validate().map_err(|problems| {
        let named: Vec<String> = problems
            .iter()
            .map(|p| format!("{} ({})", p.field.as_str(), p.reason))
            .collect();
        Error::Refused(format!(
            "the answers break the phone's rules: {}",
            named.join(", ")
        ))
    })
}

/// The phone's word for what was wrong with an answer, in the flow's field
/// and in words a person reads under it.
fn problem_words(p: &Problem) -> (AnswerField, String) {
    let field = match p.field {
        Field::Username | Field::FullName | Field::Email | Field::SshKeys => AnswerField::Username,
        Field::PasswordHash | Field::Credential => AnswerField::Password,
        Field::Hostname => AnswerField::Hostname,
        Field::Timezone => AnswerField::Timezone,
        Field::WifiSsid => AnswerField::WifiSsid,
        Field::WifiPsk => AnswerField::WifiPsk,
    };
    let what = match p.field {
        Field::FullName => "the full name",
        Field::Email => "the email address",
        Field::SshKeys => "the SSH key",
        _ => "this answer",
    };
    let words = match p.reason.as_str() {
        "required" => format!("The phone needs {what}."),
        "reserved" => "The phone keeps that name for the system; pick another.".into(),
        "system-account" => {
            "That is the name of a system account on the phone; pick another.".into()
        }
        "exists" => "An account of that name exists on the phone already; pick another.".into(),
        "no-such-zone" => "The phone does not know that time zone.".into(),
        "control-character" => format!("The phone refused {what}: it holds a control character."),
        "too-long" | "length" => format!("The phone refused {what}: it is too long or too short."),
        "without-ssid" => "A Wi-Fi password needs the network's name.".into(),
        "too-many" => "The phone takes at most 8 SSH keys.".into(),
        _ => format!("The phone refused {what} as it is written."),
    };
    (field, words)
}

/// A step of the link's flow that needs the owner, or the end.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wait {
    /// The phone works by itself, or the link is looked for: nothing to do.
    None,
    /// The code the phone shows.
    Code,
    /// The answers, again: a run that goes on from a record lost them.
    Answers(Vec<(AnswerField, String)>),
    /// Where the radio backup goes.
    BackupPlace,
    /// First-boot setup on the phone failed at these steps.
    FirstbootFailed(Vec<String>),
    /// Provisioning failed at this step.
    ProvisionFailed(String),
    /// The phone's battery is too low to provision.
    Battery,
}

/// Why the link's flow stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStop {
    pub id: &'static str,
    pub title: String,
    pub message: Vec<String>,
    pub retry: Retry,
}

/// Where the link's flow is, as a person would name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    FirstStart,
    Pair,
    Setup,
    RadioBackup,
    FactorySend,
    Provision,
    Restart,
    Check,
    Ready,
}

/// The install after the write, as one state machine for every front end.
pub struct LinkFlow {
    start: LinkStart,
    host: Host,
    connected: bool,
    /// When the link was last looked for and not there, from when.
    missing_since: Option<u64>,
    /// The phone's stage and boot, as last heard.
    stage: Option<Stage>,
    boot: u8,
    pair: PairWait,
    wrong_left: Option<u32>,
    backup_place: bool,
    backup_done: bool,
    factory_verified: bool,
    factory_mismatches: u32,
    firstboot_done: bool,
    restart_asked: bool,
    smoke: Option<omamobile_link::json::Value>,
    optional_failed: Vec<String>,
    /// `status` is asked first after each pairing: where the phone is.
    look_first: bool,
    running: Option<&'static str>,
    wait: Wait,
    stop: Option<LinkStop>,
    finished: bool,
    /// What the step's progress last said, for the screen.
    detail: Option<String>,
    /// The connection was lost and is being taken up again.
    lost: bool,
    /// The last screen the shell was given while the phone worked.
    shown: Option<(String, String, Option<String>)>,
    /// The front end's last failure, in its own words, for a stop.
    shell_error: Option<String>,
    /// What the owner's last input asks for, sent when the machine can.
    pending: Option<Command>,
    /// The machine waits for a code (its `NeedCode`).
    code_asked: bool,
    paired_once: bool,
    /// The record as the shell last kept it.
    kept: Option<LinkRecord>,
    /// The shell's clock at the last turn of the run.
    last_look: Option<u64>,
}

/// How pairing stands, when the machine is not paired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairWait {
    Hello,
    Tap,
    Session,
    Locked(u32),
}

struct Rng<'a, S: LinkShell>(&'a mut S);

impl<S: LinkShell> Entropy for Rng<'_, S> {
    fn fill(&mut self, buf: &mut [u8]) {
        self.0.random(buf);
    }
}

impl LinkFlow {
    pub fn new(start: LinkStart) -> LinkFlow {
        let mut cfg = HostConfig::new(INSTALLER, &start.version, start.serial.as_bytes());
        cfg.token = start.token;
        cfg.max_data = MAX_DATA;
        LinkFlow {
            backup_place: start.backup_place,
            host: Host::new(cfg),
            start,
            connected: false,
            missing_since: None,
            stage: None,
            boot: 1,
            pair: PairWait::Hello,
            wrong_left: None,
            backup_done: false,
            factory_verified: false,
            factory_mismatches: 0,
            firstboot_done: false,
            restart_asked: false,
            smoke: None,
            optional_failed: Vec::new(),
            look_first: true,
            running: None,
            wait: Wait::None,
            stop: None,
            finished: false,
            detail: None,
            lost: false,
            shown: None,
            shell_error: None,
            pending: None,
            code_asked: false,
            paired_once: false,
            kept: None,
            last_look: None,
        }
    }

    /// The flow stopped, and nothing more is done.
    pub fn stopped(&self) -> Option<&LinkStop> {
        self.stop.as_ref()
    }
    /// The phone is ready and the link was ended.
    pub fn finished(&self) -> bool {
        self.finished
    }
    /// The optional steps that failed (`camera`): "Cameras: not set up".
    pub fn optional_failed(&self) -> &[String] {
        &self.optional_failed
    }
    /// The smoke report the phone sent, as it wrote it.
    pub fn smoke_report(&self) -> Option<&omamobile_link::json::Value> {
        self.smoke.as_ref()
    }

    /// What the record holds now.
    pub fn record(&self) -> LinkRecord {
        LinkRecord {
            format: LinkRecord::FORMAT,
            serial: self.start.serial.clone(),
            device: self.start.device.clone(),
            device_name: self.start.device_name.clone(),
            release: self.start.release.clone(),
            factory_size: self.start.factory.size,
            factory_sha256: to_hex(&self.start.factory.sha256),
            factory_build: self.start.factory.build.clone(),
            factory_name: self.start.factory.name.clone(),
            token: self.host.token().map(|t| to_hex(&t.0)),
            stage: self.stage.map_or("unknown", Stage::as_str).to_string(),
            finished: self.finished,
        }
    }

    // ---- What the owner hands in ---------------------------------------------

    /// The code the owner typed, as typed. Only while the screen asks for it.
    pub fn pair(&mut self, typed: &str) -> Result<()> {
        if self.wait != Wait::Code || self.stop.is_some() {
            return Err(Error::Refused("the phone is not asking for a code".into()));
        }
        let digits: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
        let Some(code) = Code::parse(&digits) else {
            self.wrong_left = None;
            self.detail = Some("The code is six digits, as the phone shows it.".into());
            return Ok(());
        };
        self.detail = None;
        self.wait = Wait::None;
        self.pending = Some(Command::Pair(code));
        Ok(())
    }

    /// The owner's answers, when a run that goes on from a record asks for
    /// them again.
    pub fn answers(&mut self, answers: ValidAnswers) -> Result<()> {
        if !matches!(self.wait, Wait::Answers(_)) || self.stop.is_some() {
            return Err(Error::Refused(
                "the phone is not asking for the answers".into(),
            ));
        }
        self.start.answers = Some(answers);
        self.wait = Wait::None;
        Ok(())
    }

    /// The front end has a place for the radio backup: a new directory,
    /// mode 700 (the command line's `--backup-dir`, the app's chooser).
    pub fn backup_place_chosen(&mut self) -> Result<()> {
        if self.wait != Wait::BackupPlace && self.backup_place {
            return Err(Error::Refused(
                "the backup's place is chosen already".into(),
            ));
        }
        self.backup_place = true;
        if self.wait == Wait::BackupPlace {
            self.wait = Wait::None;
        }
        Ok(())
    }

    /// Try again: first-boot setup, provisioning, or provisioning after
    /// the phone was charged.
    pub fn retry(&mut self) -> Result<()> {
        let cmd = match &self.wait {
            Wait::FirstbootFailed(_) => Command::FirstbootRetry,
            Wait::ProvisionFailed(_) | Wait::Battery => Command::Provision,
            _ => return Err(Error::Refused("there is nothing to try again".into())),
        };
        if self.stop.is_some() {
            return Err(Error::Refused("the install has stopped".into()));
        }
        self.wait = Wait::None;
        self.detail = None;
        self.pending = Some(cmd);
        Ok(())
    }
}

// ---- The run: the machine, the transport and the shell together ---------------

impl LinkFlow {
    /// Nothing more happens until the owner hands something in, or ever.
    pub fn waits_for_owner(&self) -> bool {
        self.wait != Wait::None || self.stop.is_some() || self.finished
    }

    /// Drive the link until the owner is needed ([`Self::screen`]'s action
    /// is not `wait`), the flow stopped or finished, or the shell says to
    /// leave. While the phone works by itself the shell is given each new
    /// screen and the steps' progress. A run that returned is taken up by
    /// the next call; a run that the front end left is taken up by a new
    /// flow from the record, with no new code and no replug.
    pub async fn run<T: LinkTransport, S: LinkShell>(
        &mut self,
        link: &mut T,
        shell: &mut S,
    ) -> Result<()> {
        loop {
            if self.waits_for_owner() || !shell.go_on() {
                break;
            }
            let now = shell.now_ms();
            self.last_look = Some(now);
            self.show(shell);
            // What the owner's last input asks for
            let pending_fits = match &self.pending {
                Some(Command::Pair(_)) => self.code_asked,
                Some(_) => self.host.ready() && !self.host.busy(),
                None => false,
            };
            if pending_fits {
                if let Some(cmd) = self.pending.take() {
                    self.command(now, cmd, link, shell).await;
                    continue;
                }
            }
            // The next step, when the machine is free for one
            if self.pending.is_none() && self.host.ready() && !self.host.busy() {
                if let Some(cmd) = self.next_step() {
                    self.command(now, cmd, link, shell).await;
                }
                continue;
            }
            if !self.connected {
                match link.connect().await {
                    Ok(true) => {
                        self.connected = true;
                        self.missing_since = None;
                        let out = self.host.handle(now, HostInput::LinkUp, &mut Rng(shell));
                        self.carry(now, out, link, shell).await;
                    }
                    Ok(false) | Err(_) => {
                        let since = *self.missing_since.get_or_insert(now);
                        shell.progress("link-wait", now - since, LINK_WAIT_MS);
                        shell.sleep(LOOK_MS).await;
                    }
                }
                continue;
            }
            let deadline = self.host.deadline();
            let wait = deadline.map_or(READ_MS, |d| d.saturating_sub(now).min(READ_MS));
            match link.recv(wait).await {
                Ok(Some(frame)) => {
                    let now = shell.now_ms();
                    let out = self
                        .host
                        .handle(now, HostInput::Frame(&frame), &mut Rng(shell));
                    self.carry(now, out, link, shell).await;
                }
                Ok(None) => {
                    let now = shell.now_ms();
                    if deadline.is_some_and(|d| d <= now) {
                        let out = self.host.handle(now, HostInput::Tick, &mut Rng(shell));
                        self.carry(now, out, link, shell).await;
                    }
                }
                Err(_) => {
                    let out = self.lose(link, shell);
                    let now = shell.now_ms();
                    self.carry(now, out, link, shell).await;
                }
            }
        }
        self.keep(shell);
        Ok(())
    }

    async fn command<T: LinkTransport, S: LinkShell>(
        &mut self,
        now: u64,
        cmd: Command,
        link: &mut T,
        shell: &mut S,
    ) {
        self.running = Some(cmd.name());
        let out = self
            .host
            .handle(now, HostInput::Command(cmd), &mut Rng(shell));
        self.carry(now, out, link, shell).await;
    }

    /// The link went away: the cable, the phone restarting, a write the
    /// phone did not take in time.
    fn lose<T: LinkTransport, S: LinkShell>(
        &mut self,
        link: &mut T,
        shell: &mut S,
    ) -> Vec<HostOutput> {
        link.disconnect();
        self.connected = false;
        self.code_asked = false;
        let now = shell.now_ms();
        self.host.handle(now, HostInput::LinkDown, &mut Rng(shell))
    }

    /// Do what the machine asks, in order; the answers to its effects go
    /// back in, and what they ask for is done after what was asked before.
    async fn carry<T: LinkTransport, S: LinkShell>(
        &mut self,
        now: u64,
        out: Vec<HostOutput>,
        link: &mut T,
        shell: &mut S,
    ) {
        let mut queue: std::collections::VecDeque<HostOutput> = out.into();
        while let Some(o) = queue.pop_front() {
            let answer = match o {
                HostOutput::Send(frame) => {
                    if !self.connected {
                        continue;
                    }
                    if link.send(&frame).await.is_err() {
                        // What is left of the list belongs to a session
                        // that is over: the machine takes the step up again
                        queue.clear();
                        queue.extend(self.lose(link, shell));
                        continue;
                    }
                    None
                }
                HostOutput::Event(e) => {
                    self.on_event(e, shell);
                    None
                }
                HostOutput::Do(effect) => self.effect(effect, shell).await,
            };
            if let Some(input) = answer {
                let more = self.host.handle(now, input, &mut Rng(shell));
                queue.extend(more);
            }
        }
        self.keep(shell);
    }

    /// One effect, and what to tell the machine about it.
    async fn effect<S: LinkShell>(
        &mut self,
        effect: HostEffect,
        shell: &mut S,
    ) -> Option<HostInput<'static>> {
        let failed = |me: &mut LinkFlow, e: Error| {
            me.shell_error = Some(e.to_string());
            HostOutcome::Failed
        };
        Some(match effect {
            // The machine holds the token; the record is kept after the list
            HostEffect::RecordToken(_) | HostEffect::ForgetToken => return None,
            HostEffect::BackupOpen { ticket, name, size } => HostInput::Done(
                ticket,
                match shell.backup_open(&name, size).await {
                    Ok(have) => HostOutcome::Have(have),
                    Err(e) => failed(self, e),
                },
            ),
            HostEffect::BackupWrite { name, offset, data } => {
                match shell.backup_write(&name, offset, &data).await {
                    Ok(()) => return None,
                    Err(e) => {
                        self.shell_error = Some(e.to_string());
                        HostInput::Fault
                    }
                }
            }
            HostEffect::BackupDigest { ticket, name } => HostInput::Done(
                ticket,
                match shell.backup_digest(&name).await {
                    Ok(d) => HostOutcome::Digest(d),
                    Err(e) => failed(self, e),
                },
            ),
            HostEffect::BackupDiscard { name } => match shell.backup_discard(&name).await {
                Ok(()) => return None,
                Err(e) => {
                    self.shell_error = Some(e.to_string());
                    HostInput::Fault
                }
            },
            HostEffect::BackupFinish { ticket, parts } => {
                let files = self.backup_files(&parts, shell.wall_clock_s());
                HostInput::Done(
                    ticket,
                    match shell.backup_finish(&files).await {
                        Ok(()) => HostOutcome::Done,
                        Err(e) => failed(self, e),
                    },
                )
            }
            HostEffect::FactoryRead {
                ticket,
                offset,
                len,
            } => HostInput::Done(
                ticket,
                match shell.factory_read(offset, len).await {
                    Ok(data) => HostOutcome::Data(data),
                    Err(e) => failed(self, e),
                },
            ),
        })
    }

    /// `tools/nv-backup`'s three files for the partitions the phone verified.
    fn backup_files(&self, parts: &[(BackupPart, [u8; 32])], now_s: u64) -> BackupFiles {
        let mut sums = String::new();
        let mut table = String::new();
        for (part, digest) in parts {
            sums.push_str(&format!("{}  {}.img\n", to_hex(digest), part.name));
            table.push_str(&format!("{} - ro=1 mounted=0 {}\n", part.name, part.size));
        }
        let names = |checked: bool| {
            let n: Vec<&str> = parts
                .iter()
                .filter(|(p, _)| p.checked == checked)
                .map(|(p, _)| p.name.as_str())
                .collect();
            if n.is_empty() {
                "none".to_string()
            } else {
                n.join(" ")
            }
        };
        let nv_backup = format!(
            "# tools/nv-backup: a read-only copy of the phone's radio and calibration partitions\n# (taken by omamobile-install over the install link)\ndevice: {}\ndate: {}\nchecked: {}\ncopied, may change: {}\n",
            self.start.device,
            utc(now_s),
            names(true),
            names(false)
        );
        BackupFiles {
            sha256sums: sums,
            partitions_txt: table,
            nv_backup,
        }
    }

    fn keep<S: LinkShell>(&mut self, shell: &mut S) {
        let record = self.record();
        if self.kept.as_ref() != Some(&record) {
            match shell.keep(&record) {
                Ok(()) => self.kept = Some(record),
                // Not a stop: the run goes on, and only a second run
                // would miss it
                Err(e) => self.shell_error = Some(e.to_string()),
            }
        }
    }

    /// Give the shell the screen when it changed while the phone works.
    fn show<S: LinkShell>(&mut self, shell: &mut S) {
        let s = self.screen();
        let key = (s.state.to_string(), s.title.clone(), s.body.last().cloned());
        if self.shown.as_ref() != Some(&key) {
            self.shown = Some(key);
            shell.screen(&s);
        }
    }

    /// The step to take next, from what the phone has and has not done.
    fn next_step(&mut self) -> Option<Command> {
        if self.look_first {
            self.look_first = false;
            return Some(Command::Status);
        }
        let stage = self.stage?;
        Some(match stage {
            Stage::Waiting => match &self.start.answers {
                Some(a) => Command::Answers(a.clone()),
                None => {
                    self.wait = Wait::Answers(Vec::new());
                    return None;
                }
            },
            Stage::FirstbootFailed => {
                if !matches!(self.wait, Wait::FirstbootFailed(_)) {
                    self.wait = Wait::FirstbootFailed(Vec::new());
                }
                return None;
            }
            Stage::Finished => {
                self.finished = true;
                return None;
            }
            s if s.before_provisioned() => {
                if !self.backup_done {
                    if !self.backup_place {
                        self.wait = Wait::BackupPlace;
                        return None;
                    }
                    Command::Backup
                } else if !self.factory_verified {
                    Command::Factory(self.start.factory.clone())
                } else if !self.firstboot_done {
                    Command::AwaitFirstboot
                } else {
                    Command::Provision
                }
            }
            _ => {
                if self.boot < 2 {
                    Command::Restart
                } else if self.smoke.is_none() {
                    Command::Smoke
                } else {
                    Command::Finish
                }
            }
        })
    }

    fn apply_status<S: LinkShell>(&mut self, s: &Status, shell: &mut S) {
        self.stage = Some(s.stage);
        self.boot = s.boot;
        let after = !s.stage.before_provisioned();
        self.backup_done = s.backup_state == BackupState::Done || after;
        self.factory_verified = s.factory_state == FactoryState::Verified || after;
        self.firstboot_done = matches!(
            s.stage,
            Stage::FirstbootDone | Stage::Provisioned | Stage::Finished
        );
        self.optional_failed = s.provision_optional_failed.clone();
        if s.stage == Stage::FirstbootFailed {
            self.wait = Wait::FirstbootFailed(s.firstboot.failed.clone());
        }
        if s.firstboot.state == JobState::Running {
            shell.progress("firstboot", s.firstboot.n.into(), s.firstboot.of.into());
            self.detail = (!s.firstboot.step.is_empty()).then(|| {
                format!(
                    "Step {} of {}: {}.",
                    s.firstboot.n, s.firstboot.of, s.firstboot.step
                )
            });
        }
        if s.provision_state == JobState::Running {
            shell.progress("provision", s.provision_n.into(), s.provision_of.into());
            self.detail = (!s.provision_step.is_empty()).then(|| {
                format!(
                    "Step {} of {}: {}.",
                    s.provision_n, s.provision_of, s.provision_step
                )
            });
        }
        if s.provision_state == JobState::Failed
            && !after
            && self.firstboot_done
            && self.backup_done
            && self.factory_verified
            && !matches!(self.running, Some("provision"))
        {
            self.wait = Wait::ProvisionFailed(s.provision_step.clone());
        }
    }

    fn on_event<S: LinkShell>(&mut self, e: Event, shell: &mut S) {
        match e {
            Event::Hello { stage, .. } => {
                self.stage = Some(stage);
            }
            Event::NeedCode => {
                self.pair = PairWait::Hello;
                self.code_asked = true;
                if !matches!(self.pending, Some(Command::Pair(_))) {
                    self.wait = Wait::Code;
                }
            }
            Event::NeedTap => self.pair = PairWait::Tap,
            Event::NeedSession => self.pair = PairWait::Session,
            Event::Locked { wait_s } => {
                self.pair = PairWait::Locked(wait_s);
                self.code_asked = false;
            }
            Event::WrongCode { left } => {
                self.wrong_left = Some(left);
                self.code_asked = false;
            }
            Event::Paired { stage } => {
                self.stage = Some(stage);
                self.pair = PairWait::Hello;
                self.wrong_left = None;
                self.code_asked = false;
                self.paired_once = true;
                self.look_first = true;
                self.lost = false;
                self.detail = None;
            }
            Event::Status(s) => {
                self.apply_status(&s, shell);
                if self.running == Some("status") {
                    self.running = None;
                }
            }
            Event::Progress(p) => self.on_progress(&p, shell),
            Event::AnswersAccepted => {
                if matches!(self.stage, Some(Stage::Waiting | Stage::FirstbootFailed)) {
                    self.stage = Some(Stage::Firstboot);
                }
                // In memory no longer than they are needed
                self.start.answers = None;
                self.running = None;
            }
            Event::FirstbootDone => {
                self.firstboot_done = true;
                if self.stage.is_some_and(Stage::before_provisioned) {
                    self.stage = Some(Stage::FirstbootDone);
                }
                self.running = None;
                self.detail = None;
            }
            Event::FirstbootFailed { steps } => {
                self.stage = Some(Stage::FirstbootFailed);
                self.wait = Wait::FirstbootFailed(steps);
                self.running = None;
            }
            Event::BackupProgress { name, done, total } => {
                shell.progress("backup", done, total);
                self.detail = Some(format!("Copying {name}."));
            }
            Event::BackupDone => {
                self.backup_done = true;
                self.running = None;
                self.detail = None;
            }
            Event::FactorySent { sent, size } => shell.progress("factory-send", sent, size),
            Event::FactoryAcked { .. } => {}
            Event::FactoryVerified => {
                self.factory_verified = true;
                self.running = None;
                self.detail = None;
            }
            Event::ProvisionDone {
                optional_failed, ..
            } => {
                self.stage = Some(Stage::Provisioned);
                self.optional_failed = optional_failed;
                self.running = None;
                self.detail = None;
            }
            Event::Restarting => self.restart_asked = true,
            Event::Restarted => {
                self.boot = 2;
                self.running = None;
            }
            Event::SmokeReport(report) => {
                self.smoke = Some(report);
                self.running = None;
            }
            Event::Finished => {
                self.stage = Some(Stage::Finished);
                self.finished = true;
                self.running = None;
            }
            Event::Refused { step, error } => {
                self.running = None;
                self.refused(step, error);
            }
            Event::Failed { step } => {
                self.running = None;
                let why = self
                    .shell_error
                    .clone()
                    .unwrap_or_else(|| "A file could not be read or written.".into());
                self.halt(
                    "link-computer",
                    "This computer could not do its part",
                    vec![
                        format!("During the step \"{step}\": {why}"),
                        "Nothing on the phone was changed by it.".into(),
                    ],
                    Retry::Whenever,
                );
            }
            Event::NotNow { step } => {
                if step == "pair" {
                    self.wait = Wait::Code;
                }
            }
            Event::LinkLost => self.lost = true,
            Event::Stopped(why) => self.stopped_by(why),
            Event::Aborted | Event::Closed => {}
        }
    }

    fn on_progress<S: LinkShell>(&mut self, p: &Progress, shell: &mut S) {
        let what = match p.what {
            What::FactoryCheck => "factory-check",
            What::Provision => "provision",
            What::BackupVerify => "backup",
            What::Smoke | What::Working => "link-wait",
        };
        shell.progress(what, p.done, p.total);
        self.detail = match p.what {
            What::FactoryCheck => {
                Some("The phone checks the whole file against its SHA-256.".into())
            }
            What::BackupVerify => {
                Some("The phone reads the partition a second time to compare.".into())
            }
            What::Provision if !p.step.is_empty() => Some(format!("Now: {}.", p.step)),
            _ => self.detail.take(),
        };
    }

    fn refused(&mut self, step: &str, error: ErrorMsg) {
        match error.code {
            ErrorCode::Problems { problems } => {
                self.start.answers = None;
                self.wait = Wait::Answers(problems.iter().map(problem_words).collect());
            }
            ErrorCode::FirstbootFailed { steps } => {
                self.stage = Some(Stage::FirstbootFailed);
                self.wait = Wait::FirstbootFailed(steps);
            }
            ErrorCode::ProvisionFailed { step } => self.wait = Wait::ProvisionFailed(step),
            ErrorCode::Battery => self.wait = Wait::Battery,
            ErrorCode::FactoryMismatch if self.factory_mismatches == 0 => {
                // Sent again once from the first byte: the phone deleted
                // what it had (§8)
                self.factory_mismatches = 1;
                self.factory_verified = false;
                self.pending = Some(Command::Factory(self.start.factory.clone()));
            }
            ErrorCode::FactoryMismatch => self.halt(
                "factory-mismatch",
                "The factory image did not reach the phone intact",
                vec![
                    "The phone checked the file it received against the SHA-256 in the release's signed list of Google's builds, twice, and both times it was another file. The phone deleted what it had received.".into(),
                    "The file on this computer may have changed since the installer checked it, or the cable damages what it carries. Download the factory image from Google's page again, try another cable, and start the installer again.".into(),
                ],
                Retry::AfterFix,
            ),
            ErrorCode::BackupPrecondition { parts } => self.halt(
                "backup-precondition",
                "The phone's storage guard isn't holding",
                vec![
                    format!(
                        "Before it copies its radio partitions the phone checks that each is found once, is read-only and is not mounted. These are not: {}. Nothing was read.",
                        parts.join(", ")
                    ),
                    format!("Don't work around this. Ask at {ISSUES}, with the names above, before anything else is done with this phone."),
                ],
                Retry::StopsAgain,
            ),
            ErrorCode::BackupMismatch => self.halt(
                "backup-mismatch",
                "The radio backup could not be verified",
                vec![
                    "A partition read from the phone and the copy on this computer differed five times in a row, so there is no backup of it. Nothing on the phone was written.".into(),
                    "Check the cable and the disk the backup goes to, then start the installer again.".into(),
                ],
                Retry::AfterFix,
            ),
            ErrorCode::NoSpace { need, free } => self.halt(
                "no-space",
                "The phone has too little free space for Google's factory image",
                vec![format!(
                    "The phone needs {} free for the image and its unpacking, and has {}.",
                    gib(need),
                    gib(free)
                )],
                Retry::StopsAgain,
            ),
            ErrorCode::TooLarge => self.halt(
                "too-large",
                "The factory image is larger than the phone takes",
                vec!["The phone takes a factory image of at most 8 GiB.".into()],
                Retry::StopsAgain,
            ),
            code => self.halt(
                "link-refused",
                "The phone refused a step of the install",
                vec![format!(
                    "The phone answered the step \"{step}\" with: {} ({}).",
                    code.text(),
                    code.as_str()
                )],
                Retry::Whenever,
            ),
        }
    }

    fn stopped_by(&mut self, why: Stop) {
        match why {
            Stop::WrongPhone => self.halt(
                "link-wrong-phone",
                "This is not the phone that was just installed",
                vec![
                    "The phone on the cable offers the install link, but its serial number is not the one the installer wrote to: the proof it sent does not match. Nothing more was said to it.".into(),
                    "Connect the phone that was just installed, and start the installer again.".into(),
                ],
                Retry::AfterFix,
            ),
            Stop::Version { phone } => self.halt(
                "link-version",
                "The phone and this installer speak different versions of the install link",
                vec![
                    format!(
                        "The phone speaks the install link's protocol {}; this installer speaks protocol {PROTOCOL}.",
                        phone.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
                    ),
                    "Use the installer of the release that was installed, then start it again. The phone waits.".into(),
                ],
                Retry::AfterFix,
            ),
            Stop::Protocol => self.halt(
                "link-protocol",
                "The phone's answers could not be read",
                vec![
                    "The phone ended three sessions in a row over what this installer sent, or kept answering outside the protocol.".into(),
                    format!("Start the installer again: it goes on from where the phone is. If it stops here again, tell us at {ISSUES}"),
                ],
                Retry::Whenever,
            ),
        }
    }

    fn halt(&mut self, id: &'static str, title: &str, message: Vec<String>, retry: Retry) {
        self.stop = Some(LinkStop {
            id,
            title: title.into(),
            message,
            retry,
        });
    }

    fn place(&self) -> Place {
        if self.finished {
            return Place::Ready;
        }
        let pairing = self.wait == Wait::Code
            || self.code_asked
            || matches!(
                self.pair,
                PairWait::Tap | PairWait::Session | PairWait::Locked(_)
            );
        if !self.host.ready() && pairing && self.connected {
            return Place::Pair;
        }
        if !self.paired_once {
            return Place::FirstStart;
        }
        if self.restart_asked && self.boot < 2 {
            return Place::Restart;
        }
        match self.wait {
            Wait::Answers(_) | Wait::FirstbootFailed(_) => return Place::Setup,
            Wait::BackupPlace => return Place::RadioBackup,
            Wait::ProvisionFailed(_) | Wait::Battery => return Place::Provision,
            _ => {}
        }
        match self.stage {
            None | Some(Stage::Waiting) => Place::Setup,
            Some(s) if s.before_provisioned() => {
                if !self.backup_done {
                    Place::RadioBackup
                } else if !self.factory_verified {
                    Place::FactorySend
                } else if !self.firstboot_done {
                    Place::Setup
                } else {
                    Place::Provision
                }
            }
            _ if self.boot < 2 => Place::Restart,
            Some(Stage::Finished) => Place::Ready,
            _ => Place::Check,
        }
    }

    /// The current step as text, as [`crate::flow::Flow::screen`] gives it.
    pub fn screen(&self) -> Screen {
        let mut s = Screen {
            state: "",
            stop: None,
            retry: None,
            title: String::new(),
            notice: None,
            body: Vec::new(),
            quote: None,
            steps: Vec::new(),
            link: None,
            file: None,
            problem: None,
            plan: Vec::new(),
            footer: Vec::new(),
            choices: Vec::new(),
            extras: false,
            action: "wait",
        };
        let phone = &self.start.device_name;
        if let Some(stop) = &self.stop {
            s.state = "stopped";
            s.stop = Some(stop.id);
            s.retry = Some(stop.retry);
            s.title = stop.title.clone();
            s.body = stop.message.clone();
            s.action = "none";
            s.footer = vec![
                "Omarchy Mobile is installed on the phone. What the install link did before this stop is kept on the phone, and the installer started again goes on from there, with no new write, for as long as the phone offers the link.".into(),
                "The phone offers the link at every start until it has an account, and after that for three more starts, ten minutes each without the installer.".into(),
            ];
            s.fit_to_print();
            return s;
        }
        match self.place() {
            Place::FirstStart => {
                s.state = "first-start";
                s.title = format!("Waiting for the {phone}'s first start");
                s.body = vec![
                    "The phone is starting Omarchy Mobile for the first time. At this first start it grows the system to fill its storage, which can take a few minutes. Then it shows a six-digit code on its screen.".into(),
                    "Keep the phone connected to this computer with the same cable. The installer finds it by itself.".into(),
                ];
                self.missing_words(&mut s);
            }
            Place::Pair => {
                s.state = "pair";
                match (self.wait == Wait::Code, self.pair) {
                    (true, _) => {
                        s.action = "pair";
                        s.title = "Type the code the phone shows".into();
                        s.body = vec![
                            "The phone shows a six-digit code on its screen. Type it here, so that the phone knows the installer it talks to is the one at its screen.".into(),
                            "No other computer the phone is plugged into can go on with the install without that code.".into(),
                        ];
                        s.problem = match (self.wrong_left, &self.detail) {
                            (_, Some(d)) => Some(d.clone()),
                            (Some(0), _) => Some("That is not the code the phone shows. The phone now shows a new code; type that one.".into()),
                            (Some(left), _) => Some(format!("That is not the code the phone shows. {left} more tries before the phone shows a new code and waits a little.")),
                            _ => None,
                        };
                    }
                    (_, PairWait::Tap) => {
                        s.title = "Unlock your phone and tap \"Setup is not finished\"".into();
                        s.body = vec![
                            "The phone's own setup is done, so its screen no longer shows the code. Unlock the phone, then tap the notice \"Setup is not finished\": a window on the phone shows a new code.".into(),
                            "The installer asks the phone again every few seconds, and asks you for the code as soon as the phone shows one.".into(),
                        ];
                    }
                    (_, PairWait::Session) => {
                        s.title = "Log in on your phone".into();
                        s.body = vec![
                            "The phone cannot show a code until you are logged in. Log in on the phone, then tap the notice \"Setup is not finished\": a window on the phone shows a new code.".into(),
                            "The installer asks the phone again every few seconds.".into(),
                        ];
                    }
                    (_, PairWait::Locked(w)) if w > 0 => {
                        s.title = "Too many wrong codes: the phone waits".into();
                        s.body = vec![format!("The phone takes no code for {w} seconds, then shows a new one. The installer asks for it then.")];
                    }
                    (_, PairWait::Locked(_)) => {
                        s.title = "Too many wrong codes".into();
                        s.body = vec![
                            "The phone takes no more codes until someone at the phone asks for a new one. Press a volume key on the phone, or, once you are logged in on it, tap the notice \"Setup is not finished\".".into(),
                            "The installer asks the phone again every few seconds.".into(),
                        ];
                    }
                    (_, PairWait::Hello) => {
                        s.title = "Connecting to the phone".into();
                        s.body = vec!["The phone offers the install link. The installer is saying hello to it.".into()];
                    }
                }
            }
            Place::Setup => {
                s.state = "setup";
                match &self.wait {
                    Wait::Answers(problems) => {
                        s.action = "answers";
                        s.extras = true;
                        s.title = "Your answers for the phone, once more".into();
                        s.body = vec![
                            "The phone has not made your account yet, and the installer no longer has your answers: it keeps them in memory only, and it was started again since you gave them. Give them once more.".into(),
                        ];
                        if !problems.is_empty() {
                            s.problem = Some(
                                problems
                                    .iter()
                                    .map(|(_, words)| words.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            );
                        }
                    }
                    Wait::FirstbootFailed(steps) => {
                        s.action = "retry";
                        s.title = "Setup on the phone stopped".into();
                        s.body = vec![
                            if steps.is_empty() {
                                "The phone's own setup from your answers stopped before it was done.".into()
                            } else {
                                format!("The phone's own setup from your answers stopped at: {}.", steps.join(", "))
                            },
                            "The phone keeps your answers. Trying again runs its setup again; it also runs again by itself at the phone's next start.".into(),
                        ];
                        s.footer = vec![format!(
                            "If it stops at the same step again, tell us at {ISSUES}"
                        )];
                    }
                    _ if self.stage == Some(Stage::Waiting) || self.stage.is_none() => {
                        s.title = "Giving the phone your answers".into();
                        s.body = vec!["The installer sends your answers to the phone: your user name, the hash of your PIN or password (never the PIN or password itself), the phone's name, the time zone and the Wi-Fi network. The phone checks them by its own rules and then makes your account.".into()];
                    }
                    _ => {
                        s.title = "The phone is setting itself up".into();
                        s.body = vec!["The phone is making your account from your answers and setting its name, time zone and Wi-Fi.".into()];
                    }
                }
            }
            Place::RadioBackup => {
                s.state = "radio-backup";
                if self.wait == Wait::BackupPlace {
                    s.action = "backup-dir";
                    s.title = "Choose where the radio backup goes".into();
                } else {
                    s.title = "Copying the phone's radio partitions to this computer".into();
                }
                s.body = vec![
                    "Before the phone's modem starts for the first time, the installer copies the phone's radio and calibration partitions to this computer, read-only: the phone reads each one twice and compares, and nothing on it is written.".into(),
                    "They hold the phone's IMEI, its serial numbers and its calibration, and nothing of what you do with the phone. Keep the backup private: don't share it or put it online. It is how the phone's identity can be checked or put right later.".into(),
                ];
                if self.wait == Wait::BackupPlace {
                    s.steps = vec![ScreenStep {
                        text: "Choose a new, empty folder on this computer for the backup (about 430 MB).".into(),
                        command: None,
                    }];
                }
            }
            Place::FactorySend => {
                s.state = "factory-send";
                s.title = "Sending Google's factory image to the phone".into();
                s.body = vec![
                    format!(
                        "The phone now gets the factory image you have from Google ({}, {}). It checks the whole file against the SHA-256 in the release's signed list of Google's builds, and then takes Google's files for Wi-Fi, Bluetooth, sound, cameras and modem out of it itself.",
                        self.start.factory.name,
                        crate::flow::about_size(self.start.factory.size)
                    ),
                    "The installer reads the file from where it is on this computer, a piece at a time. It changes nothing in it and keeps no copy of its own.".into(),
                ];
            }
            Place::Provision => {
                s.state = "provision";
                match &self.wait {
                    Wait::ProvisionFailed(step) => {
                        s.action = "retry";
                        s.title = "Installing the firmware stopped".into();
                        s.body = vec![
                            if step.is_empty() {
                                "The phone stopped installing Google's files.".into()
                            } else {
                                format!("The phone stopped installing Google's files at: {step}.")
                            },
                            "The factory image is still on the phone, and every part that was installed is whole or untouched. Trying again runs the whole step again.".into(),
                        ];
                        s.footer = vec![format!(
                            "If it stops at the same step again, tell us at {ISSUES}"
                        )];
                    }
                    Wait::Battery => {
                        s.action = "retry";
                        s.title = "Charge the phone first".into();
                        s.body = vec!["The phone's battery is too low to install the firmware. Plug it into a charger, or leave it on this computer's cable until it is at 30 percent or more, then try again.".into()];
                    }
                    _ => {
                        s.title = "The phone is installing its firmware".into();
                        s.body = vec![
                            "The phone takes Google's files for Wi-Fi, Bluetooth, sound, cameras, sensors, fingerprint and modem out of the factory image and installs them. This can take several minutes.".into(),
                            "It needs nothing from this computer meanwhile. Keep the cable in.".into(),
                        ];
                    }
                }
            }
            Place::Restart => {
                s.state = "restart";
                s.title = "The phone is restarting".into();
                s.body = vec![
                    "The firmware is in place, and the phone restarts once to start with it. The installer waits for it to come back.".into(),
                ];
                self.missing_words(&mut s);
            }
            Place::Check => {
                s.state = "check";
                s.title = "Checking the phone".into();
                s.body = vec!["The phone runs its own check of the screen, touch, sound, radios and sensors, and sends this computer the report.".into()];
            }
            Place::Ready => {
                s.state = "ready";
                s.action = "none";
                s.title = format!("Your {phone} is ready");
                s.body = vec![
                    "Omarchy Mobile is installed and set up: your account, Google's firmware for this phone, and the phone's own check are done. Unlock the phone with the PIN or password you chose.".into(),
                ];
                if self.optional_failed.iter().any(|s| s == "camera") {
                    s.body.push("Cameras: not set up. The phone works without them; a later update of Omarchy Mobile can set them up.".into());
                }
                let others: Vec<&str> = self
                    .optional_failed
                    .iter()
                    .filter(|s| *s != "camera")
                    .map(String::as_str)
                    .collect();
                if !others.is_empty() {
                    s.body.push(format!("Not set up: {}.", others.join(", ")));
                }
                s.footer = vec![
                    "The radio backup is in the folder you chose. Keep it private.".into(),
                    "The phone no longer offers the install link. Android is still on slot A, unused.".into(),
                ];
            }
        }
        if s.action == "wait" {
            if self.lost && !self.connected && !matches!(self.place(), Place::Restart) {
                s.body.push("The connection to the phone was lost. The installer takes it up again where the phone says it is, as soon as the phone is back on the cable.".into());
            }
            if let Some(d) = &self.detail {
                s.body.push(d.clone());
            }
        }
        s.fit_to_print();
        s
    }

    /// The link was looked for and not found for a long while.
    fn missing_words(&self, s: &mut Screen) {
        let long = self
            .missing_since
            .zip(self.last_look)
            .is_some_and(|(since, now)| now.saturating_sub(since) >= LINK_WAIT_MS);
        if long {
            s.problem = Some("The phone has not offered the install link for ten minutes. Check that the cable is in at both ends. If the phone's screen shows that the install link could not start, or nothing at all, restart the phone by holding its power key for about 30 seconds: the link is tried again at the next start. The installer goes on looking.".into());
        }
    }
}

/// Bytes for a person, in GiB with one decimal.
fn gib(n: u64) -> String {
    format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64)
}

/// `2026-11-13T14:03:00Z`.
fn utc(at: u64) -> String {
    let (days, secs) = (at / 86400, at % 86400);
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (
        doy - (153 * mp + 2) / 5 + 1,
        if mp < 10 { mp + 3 } else { mp - 9 },
    );
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}
