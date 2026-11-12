// SPDX-License-Identifier: MIT
//! The phone's side of the link as a state machine with no I/O.
//!
//! [`Phone::start`] takes what the daemon found on the phone ([`Found`]),
//! squares the link's state file with it by INSTALL-LINK-PLAN §6.5's tables
//! ([`reconcile`]) and says whether this boot offers the link at all.
//! [`Phone::handle`] then takes the time and one thing that happened (a
//! frame, the link coming or going, a deadline, the outcome of something the
//! shell did) and returns, in order, what the shell must do next: frames to
//! write to the host and [`Effect`]s to carry out. The shell works through
//! that list in order and stops at the first step it cannot do, which it
//! reports as [`PhoneInput::Fault`]: a reply is never sent before the state
//! it announces is on disk.
//!
//! The machine never opens a file, runs a program or draws a number. An
//! effect that has an answer carries a [`Ticket`]; the shell hands the
//! answer back with [`PhoneInput::Done`], at once or much later, and the
//! machine goes on answering `ping`, `status` and a new `hello` meanwhile.
use crate::answers::{Problem, ValidAnswers};
use crate::digest::{ct_eq, serial_proof, sha256, to_hex};
use crate::frame::{self, FrameError, Kind, DEFAULT_MAX_DATA, MAX_DATA_LIMIT, MIN_MAX_DATA};
use crate::json::{self, ObjWriter, Value, MAX_NUMBER};
use crate::msg::*;
use crate::{Entropy, PROTOCOL};

/// Names the answer to an effect.
pub type Ticket = u64;

/// A session ends after this long with no frame from the host (§5.3).
pub const SESSION_IDLE_MS: u64 = 600_000;
/// An incoming stream is suspended after this long without a frame (§5.3).
pub const STREAM_IDLE_MS: u64 = 120_000;
/// While an answer is owed, `progress` goes out at least this often (§5.3).
pub const PROGRESS_EVERY_MS: u64 = 5_000;
/// Every fifth wrong code replaces the code and locks `pair` (§2.5).
pub const TRIES_PER_CODE: u32 = 5;
/// The locks after the fifth, tenth and fifteenth wrong code (§2.5).
pub const LOCK_MS: [u64; 3] = [30_000, 60_000, 120_000];
/// Wrong codes since the last act at the phone before `pair` is locked
/// until the next one (§2.5).
pub const TRIES_PER_ACT: u32 = 20;
/// Setup boots a phone with an account gets (§2.2).
pub const SETUP_BOOTS: u8 = 3;
/// With an account, the link stays open this long without a paired host (§2.2).
pub const UNPAIRED_MS: u64 = 600_000;
/// A code asked for in the owner's session is shown this long (§2.8).
pub const SESSION_CODE_MS: u64 = 600_000;
/// An acknowledgement after each 64 MiB written and synced (§5.6).
pub const ACK_EVERY: u64 = 64 << 20;
/// The image's window: the host never has more than this sent beyond the
/// last acknowledgement it has read (§5.7).
pub const WINDOW: u64 = 2 * ACK_EVERY;
/// `provision` needs the charger or this much battery (§5.6).
pub const MIN_BATTERY: u8 = 30;
/// The step a provisioning run with no result stopped at (§6.5).
pub const INTERRUPTED: &str = "interrupted";
/// `status` requests answered at once at most.
const MAX_STATUS_WAITING: usize = 4;

/// What the daemon knows before any frame.
#[derive(Clone, PartialEq, Eq)]
pub struct PhoneConfig {
    /// The daemon's version, for `hello.link`.
    pub link: String,
    /// The codename.
    pub device: String,
    /// `androidboot.serialno`. It never crosses the link: only the proof does.
    pub serial: Vec<u8>,
    /// The protocol numbers the daemon speaks.
    pub protos: Vec<u32>,
    /// The largest data payload the daemon reads.
    pub max_data: u32,
    /// Bytes between acknowledgements; [`ACK_EVERY`] outside tests.
    pub ack_every: u64,
    /// The image's window; [`WINDOW`] outside tests. Never less than
    /// `ack_every` and one frame, or a host that keeps to it could not
    /// reach the next acknowledgement.
    pub window: u64,
    /// The descriptor's `PROVISION_OPTIONAL`: the steps whose failure does
    /// not fail a provisioning run (§6.6).
    pub optional: Vec<String>,
}

impl PhoneConfig {
    pub fn new(link: &str, device: &str, serial: &[u8]) -> PhoneConfig {
        PhoneConfig {
            link: link.to_string(),
            device: device.to_string(),
            serial: serial.to_vec(),
            protos: vec![PROTOCOL],
            max_data: DEFAULT_MAX_DATA,
            ack_every: ACK_EVERY,
            window: WINDOW,
            optional: vec!["camera".to_string()],
        }
    }
}

impl core::fmt::Debug for PhoneConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "PhoneConfig {{ link: {:?}, device: {:?}, serial withheld }}",
            self.link, self.device
        )
    }
}

/// The factory image's one file, as far as the state file knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactoryFile {
    pub size: u64,
    pub sha256: [u8; 32],
    /// Bytes written and synced: where a resumed transfer goes on.
    pub synced: u64,
    /// The whole file was read back and is the announced one.
    pub verified: bool,
}

/// What survives a restart of the daemon or of the phone: the link's state
/// file. It exists from the first start (§6.5). The machine hands a new copy
/// to the shell ([`Effect::Persist`]) before the reply that announces a
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persistent {
    pub stage: Stage,
    /// SHA-256 of the session token. The token itself is never kept.
    pub token_hash: Option<[u8; 32]>,
    /// 1 before the planned restart; 2 from the first boot that begins at
    /// `provisioned`.
    pub boot: u8,
    /// The boot this file was last squared with the phone in: the kernel's
    /// `/proc/sys/kernel/random/boot_id`, as [`Found::boot_id`] gave it. A
    /// start that finds another one is the first of a new boot. It is in
    /// this file, and not in a marker of the daemon's under `/run`, so that
    /// "this boot was counted" and the count are one write: a daemon killed
    /// between the two would otherwise lose a setup boot, or the 2 of
    /// `boot`. A random number of the kernel's; it never crosses the link.
    pub boot_id: String,
    /// Setup boots counted: boots whose start ended at stage `firstboot` or
    /// later (§2.2).
    pub setup_boots: u8,
    /// Setup boots this phone gets: [`SETUP_BOOTS`], and one more for each
    /// `omamobile-install-link reopen` ([`reopen`]).
    pub boots_allowed: u8,
    /// `backup-commit` succeeded: the pending checksum file exists.
    pub backup_done: bool,
    pub factory: Option<FactoryFile>,
    pub provision: JobState,
    /// The step provisioning stopped at.
    pub provision_failed: String,
    /// The optional steps that failed in the run that succeeded (§6.6).
    pub optional_failed: Vec<String>,
    /// The first-boot steps that failed.
    pub firstboot_failed: Vec<String>,
}

impl Default for Persistent {
    fn default() -> Self {
        Persistent {
            stage: Stage::Waiting,
            token_hash: None,
            boot: 1,
            boot_id: String::new(),
            setup_boots: 0,
            boots_allowed: SETUP_BOOTS,
            backup_done: false,
            factory: None,
            provision: JobState::None,
            provision_failed: String::new(),
            optional_failed: Vec::new(),
            firstboot_failed: Vec::new(),
        }
    }
}

impl Persistent {
    /// The state file's content.
    pub fn to_json(&self) -> String {
        let mut w = ObjWriter::new();
        w.num("v", 1).str("stage", self.stage.as_str());
        if let Some(h) = &self.token_hash {
            w.str("token_sha256", &to_hex(h));
        }
        w.num("boot", self.boot as u64)
            .str("boot_id", &self.boot_id)
            .num("setup_boots", self.setup_boots as u64)
            .num("boots_allowed", self.boots_allowed as u64)
            .bool("backup_done", self.backup_done);
        if let Some(f) = &self.factory {
            let mut o = ObjWriter::new();
            o.num("size", f.size)
                .str("sha256", &to_hex(&f.sha256))
                .num("synced", f.synced)
                .bool("verified", f.verified);
            w.raw("factory", &o.end());
        }
        let list = |v: &[String]| json::array(v.iter().map(|s| json::quoted(s)));
        w.str("provision", self.provision.as_str())
            .str("provision_failed", &self.provision_failed)
            .raw("optional_failed", &list(&self.optional_failed))
            .raw("firstboot_failed", &list(&self.firstboot_failed));
        w.end()
    }

    /// A state file, read as strictly as a message.
    pub fn from_json(bytes: &[u8]) -> Result<Persistent, MsgError> {
        let v = json::parse(bytes).map_err(MsgError::Json)?;
        let mut rd = Rd::new("state", "", "", &v, false)?;
        let ver = rd.req("v")?;
        rd.check_num("v", ver, 1, 1)?;
        let stage = rd.word("stage", Stage::from_wire)?;
        let token_hash = match rd.opt("token_sha256") {
            Some(_) => Some(rd.hex32("token_sha256")?),
            None => None,
        };
        let b = rd.req("boot")?;
        let boot = rd.check_num("boot", b, 1, 2)? as u8;
        let boot_id = rd.str("boot_id", MAX_TEXT, is_text)?;
        let setup_boots = rd.num("setup_boots", 255)? as u8;
        let boots_allowed = rd.num("boots_allowed", 255)? as u8;
        let backup_done = rd.boolean("backup_done")?;
        let factory = match rd.opt("factory") {
            Some(_) => {
                let mut f = rd.child("factory")?;
                let size = f.num("size", MAX_NUMBER)?;
                let x = FactoryFile {
                    size,
                    sha256: f.hex32("sha256")?,
                    synced: f.num("synced", size)?,
                    verified: f.boolean("verified")?,
                };
                f.end()?;
                Some(x)
            }
            None => None,
        };
        let p = Persistent {
            stage,
            token_hash,
            boot,
            boot_id,
            setup_boots,
            boots_allowed,
            backup_done,
            factory,
            provision: rd.word("provision", JobState::from_wire)?,
            provision_failed: rd.str("provision_failed", MAX_TEXT, is_text)?,
            optional_failed: rd.strings("optional_failed", MAX_STEPS, MAX_TEXT, is_text1)?,
            firstboot_failed: rd.strings("firstboot_failed", MAX_STEPS, MAX_TEXT, is_text1)?,
        };
        rd.end()?;
        Ok(p)
    }

    /// Setup boots left (§2.2).
    pub fn boots_left(&self) -> u8 {
        self.boots_allowed.saturating_sub(self.setup_boots)
    }
}

/// `omamobile-install-link reopen` (§2.8): the state file that grants one
/// more setup boot, from the one an `expired` end left. `None` when there is
/// nothing to reopen: the budget is not spent, or the stage is not one an
/// `expired` end leaves. The command itself checks that `done` says
/// `expired`, deletes `done` and puts the marker back; the daemon's next
/// start does the rest.
pub fn reopen(left: &Persistent) -> Option<Persistent> {
    let expired_stage = matches!(
        left.stage,
        Stage::Firstboot | Stage::FirstbootFailed | Stage::FirstbootDone
    );
    if !expired_stage || left.setup_boots < left.boots_allowed {
        return None;
    }
    Some(Persistent {
        boots_allowed: left.setup_boots.checked_add(1)?,
        ..left.clone()
    })
}

// --- What the daemon finds at its start (§6.5) -----------------------------

/// The link's state file as the daemon found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFile {
    Missing,
    /// Damaged, or another version.
    Unreadable,
    Read(Persistent),
}

impl StateFile {
    /// From the file's bytes, `None` when there is no file.
    pub fn read(bytes: Option<&[u8]>) -> StateFile {
        match bytes.map(Persistent::from_json) {
            None => StateFile::Missing,
            Some(Ok(p)) => StateFile::Read(p),
            Some(Err(_)) => StateFile::Unreadable,
        }
    }
}

/// Is there an account with a uid of 1000 or more, and whose?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Account {
    None,
    /// The one first boot's `/var/lib/omamobile/firstboot/user` names.
    Firstboot,
    /// Another one.
    Other,
}

/// `omamobile-firstboot.service` in this boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstbootUnit {
    /// It has not run yet in this boot.
    NotRun,
    Running,
    /// It ended failed in this boot, with the steps its log marks `FAILED`.
    Failed(Vec<String>),
}

/// What the daemon found on the phone at its start: a plain structure it
/// fills in and hands to [`Phone::start`]. Every field is a fact that can be
/// read without changing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// This boot's name: `/proc/sys/kernel/random/boot_id`, at most 64
    /// printable ASCII. The state file keeps the one it was last squared
    /// in; another one here means a new boot. Empty (it could not be read)
    /// counts as a new boot every time: the budget is then spent sooner,
    /// never later.
    pub boot_id: String,
    /// `/var/lib/omamobile/install-link/done` exists.
    pub done: bool,
    /// `/etc/omamobile/install-link` exists.
    pub marker: bool,
    pub state: StateFile,
    /// `/etc/omamobile/firstboot.conf` exists.
    pub firstboot_conf: bool,
    /// `/var/lib/omamobile/firstboot/done` exists.
    pub firstboot_done: bool,
    pub account: Account,
    pub firstboot_unit: FirstbootUnit,
    /// `/etc/omamobile/boot-update.conf` exists.
    pub boot_update_conf: bool,
    /// The image file's length; `None`: no file.
    pub image_len: Option<u64>,
    /// The pending `modem-nv.sha256` exists.
    pub pending_sums: bool,
    /// `omamobile-provision-run.service` is active.
    pub provision_unit: bool,
    /// The worker's result file: each step and its state. `None`: no file.
    pub provision_result: Option<Vec<StepResult>>,
    /// A working directory of the extraction exists.
    pub work_dir: bool,
    /// A candidate or a temporary file of the daemon's own exists.
    pub leftovers: bool,
}

impl Found {
    /// A freshly installed phone at its first start: the marker and nothing
    /// else.
    pub fn first_start(boot_id: &str) -> Found {
        Found {
            boot_id: boot_id.to_string(),
            done: false,
            marker: true,
            state: StateFile::Missing,
            firstboot_conf: false,
            firstboot_done: false,
            account: Account::None,
            firstboot_unit: FirstbootUnit::NotRun,
            boot_update_conf: false,
            image_len: None,
            pending_sums: false,
            provision_unit: false,
            provision_result: None,
            work_dir: false,
            leftovers: false,
        }
    }
}

/// Why the link ended without ever being this phone's (§2.2, §6.5): the
/// word `done` gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A marker, and a `firstboot.conf`, a first-boot `done` or an account,
    /// but no state file: a phone the link never set up.
    NotFirstStart,
    /// A state file that cannot be read, on a phone that has an account.
    StateUnreadable,
    /// An account first boot did not make, and nothing of first boot's.
    Inconsistent,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::NotFirstStart => "not-first-start",
            Refusal::StateUnreadable => "state-unreadable",
            Refusal::Inconsistent => "inconsistent",
        }
    }
}

/// What an `expired` end leaves undone (§2.2): words, no values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    Backup,
    Firmware,
    Cameras,
}

impl Missing {
    pub fn as_str(self) -> &'static str {
        match self {
            Missing::Backup => "backup",
            Missing::Firmware => "firmware",
            Missing::Cameras => "cameras",
        }
    }
}

/// Where the start-up rules ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The link is offered in this boot, from this state.
    Offer(Persistent),
    /// It is not: the effects say why, and no gadget change is made.
    Ended,
}

/// The start-up rules' result: the state to start in and what to carry out
/// first, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub start: Start,
    pub effects: Vec<Effect>,
}

/// Is the run in `result` a success, and which optional steps failed in it?
/// `Err` names the step it stopped at. A run succeeds when every required
/// step is `ok` (§5.6).
fn judge(result: Option<&[StepResult]>, optional: &[String]) -> Result<Vec<String>, String> {
    let steps = match result {
        Some(s) if !s.is_empty() => s,
        _ => return Err(INTERRUPTED.to_string()),
    };
    let is_optional = |s: &StepResult| optional.contains(&s.step);
    if let Some(bad) = steps
        .iter()
        .find(|s| !is_optional(s) && s.state != StepState::Ok)
    {
        let step = clean(&bad.step);
        return Err(if step.is_empty() {
            "unknown".to_string()
        } else {
            step
        });
    }
    Ok(steps
        .iter()
        .filter(|s| is_optional(s) && s.state == StepState::Failed)
        .map(|s| clean(&s.step))
        .filter(|s| !s.is_empty())
        .take(MAX_STEPS)
        .collect())
}

/// The names of failed steps, made fit for the state file and a message.
fn failed_steps(failed: &[String]) -> Vec<String> {
    let mut v: Vec<String> = failed
        .iter()
        .take(MAX_STEPS)
        .map(|s| clean(s))
        .filter(|s| !s.is_empty())
        .collect();
    if v.is_empty() {
        v.push("unknown".to_string());
    }
    v
}

fn missing(p: &Persistent) -> Vec<Missing> {
    let mut m = Vec::new();
    if !p.backup_done {
        m.push(Missing::Backup);
    }
    if p.stage != Stage::Provisioned {
        m.push(Missing::Firmware);
        m.push(Missing::Cameras);
    } else if !p.optional_failed.is_empty() {
        m.push(Missing::Cameras);
    }
    m
}

fn has_account(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Firstboot | Stage::FirstbootFailed | Stage::FirstbootDone | Stage::Provisioned
    )
}

/// INSTALL-LINK-PLAN §6.5: the daemon squares its state file with the phone
/// before it offers anything. A pure function from what was found to the
/// state to start in and the effects to carry out, in order; `optional` is
/// the descriptor's `PROVISION_OPTIONAL`. [`Phone::start`] calls it.
pub fn reconcile(found: &Found, optional: &[String]) -> Reconciled {
    let ended = |effects: Vec<Effect>| Reconciled {
        start: Start::Ended,
        effects,
    };
    // Row 1: the link ended in a boot before this one
    if found.done {
        return ended(if found.marker {
            vec![Effect::DeleteMarker]
        } else {
            Vec::new()
        });
    }
    let account = found.account != Account::None;
    let (mut p, fresh) = match &found.state {
        // Rows 2 and 3
        StateFile::Missing => {
            if found.firstboot_conf || found.firstboot_done || account {
                return ended(vec![Effect::Refuse {
                    reason: Refusal::NotFirstStart,
                }]);
            }
            (Persistent::default(), true)
        }
        // Rows 4 and 5: it never guesses about a phone that has an account
        StateFile::Unreadable => {
            if account {
                return ended(vec![Effect::Refuse {
                    reason: Refusal::StateUnreadable,
                }]);
            }
            (Persistent::default(), true)
        }
        StateFile::Read(p) => (p.clone(), false),
    };
    // Row 6: the end was begun and not completed
    if p.stage == Stage::Finished {
        return ended(vec![Effect::Finish]);
    }
    let before = p.clone();
    let mut todo: Vec<Effect> = Vec::new();

    // The second table: what first boot itself left, the first row that fits
    if matches!(
        p.stage,
        Stage::Waiting | Stage::Firstboot | Stage::FirstbootFailed
    ) {
        if found.firstboot_done {
            p.stage = Stage::FirstbootDone;
            p.firstboot_failed.clear();
        } else if found.firstboot_conf {
            match &found.firstboot_unit {
                FirstbootUnit::Failed(steps) => {
                    p.stage = Stage::FirstbootFailed;
                    p.firstboot_failed = failed_steps(steps);
                }
                FirstbootUnit::NotRun | FirstbootUnit::Running => {
                    p.stage = Stage::Firstboot;
                    p.firstboot_failed.clear();
                }
            }
            if !found.boot_update_conf {
                todo.push(Effect::BootUpdateConf);
            }
        } else {
            match found.account {
                // The script deletes firstboot.conf and then writes its
                // done: power was lost between the two
                Account::Firstboot => {
                    p.stage = Stage::FirstbootDone;
                    p.firstboot_failed.clear();
                }
                Account::Other => {
                    return ended(vec![Effect::Refuse {
                        reason: Refusal::Inconsistent,
                    }])
                }
                // From `firstboot`: the answers are gone, and are sent again
                Account::None => {
                    p.stage = Stage::Waiting;
                    p.firstboot_failed.clear();
                }
            }
        }
    }

    // The third table. Provisioning first: it can move the stage.
    if p.provision == JobState::Running {
        if found.provision_unit {
            todo.push(Effect::ProvisionWatch);
        } else {
            match judge(found.provision_result.as_deref(), optional) {
                Ok(optional_failed) => {
                    p.provision = JobState::Done;
                    p.stage = Stage::Provisioned;
                    p.optional_failed = optional_failed;
                }
                Err(step) => {
                    p.provision = JobState::Failed;
                    p.provision_failed = step;
                }
            }
        }
    }
    if found.work_dir && !found.provision_unit {
        todo.push(Effect::WorkDirRemove);
    }
    // The image
    if p.stage == Stage::Provisioned {
        p.factory = None;
        if found.image_len.is_some() {
            todo.push(Effect::FactoryDelete);
        }
    } else {
        match (&mut p.factory, found.image_len) {
            (Some(_), None) => p.factory = None,
            (None, Some(_)) => todo.push(Effect::FactoryDelete),
            (Some(f), Some(len)) => {
                if f.verified && (len != f.size || f.synced != f.size) {
                    f.verified = false;
                }
                if len > f.synced {
                    // Bytes after the last sync are not trusted
                    todo.push(Effect::FactoryTruncate { len: f.synced });
                } else if len < f.synced {
                    // A sync that did not hold
                    f.synced = 0;
                    f.verified = false;
                    todo.push(Effect::FactoryTruncate { len: 0 });
                }
            }
            (None, None) => {}
        }
    }
    // The backup's mark and the pending checksums. Not at `provisioned`:
    // provisioning has taken the pending file by then (docs/LINK.md §6).
    if p.stage.before_provisioned() {
        if p.backup_done && !found.pending_sums {
            p.backup_done = false;
        } else if !p.backup_done && found.pending_sums {
            todo.push(Effect::SumsDelete);
        }
    }
    if found.leftovers {
        todo.push(Effect::Sweep);
    }

    // Then the budget of §2.2, once for each boot
    let boot_id = clean(&found.boot_id);
    let new_boot = boot_id.is_empty() || boot_id != p.boot_id;
    p.boot_id = boot_id;
    if new_boot && has_account(p.stage) {
        if p.stage == Stage::Provisioned {
            p.boot = 2;
        }
        if p.setup_boots >= p.boots_allowed {
            p.token_hash = None;
            p.factory = None;
            return if p.stage == Stage::Provisioned {
                // The smoke report is a report, not a requirement
                p.stage = Stage::Finished;
                ended(vec![Effect::Persist(p), Effect::Finish])
            } else {
                let missing = missing(&p);
                ended(vec![Effect::Persist(p), Effect::Expire { missing }])
            };
        }
        p.setup_boots += 1;
    }

    // Then the result is recorded, and the link offered
    let mut effects = Vec::new();
    if fresh || p != before {
        effects.push(Effect::Persist(p.clone()));
    }
    effects.extend(todo);
    Reconciled {
        start: Start::Offer(p),
        effects,
    }
}

// --- What the shell does ---------------------------------------------------

/// What the shell reads for `status` and for `provision`'s battery rule.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facts {
    /// From `/usr/share/omamobile/rootfs/MANIFEST`.
    pub release: String,
    /// `uname -r`.
    pub kernel: String,
    pub firstboot_step: String,
    pub firstboot_n: u32,
    pub firstboot_of: u32,
    pub provision_step: String,
    pub provision_n: u32,
    pub provision_of: u32,
    pub battery: u8,
    pub charging: bool,
    pub free: u64,
}

/// One radio partition as the shell found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartFound {
    pub name: String,
    pub size: u64,
    /// In `NV_PARTS`.
    pub checked: bool,
    /// The kernel's device name. Compared between `backup-list` and
    /// `backup-commit`; it never crosses the link.
    pub dev: String,
}

/// What the first-start screen should show. The words are its own. The
/// screen is up while no account exists and while first-boot setup runs or
/// has failed (§2.5); from stage `firstboot-done` on the session owns the
/// display and the shell passes these over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// "Waiting for the installer on your computer", with the code.
    Code(Code),
    /// Wrong code, tries left before the code is replaced.
    WrongCode {
        left: u32,
    },
    /// `pair` is locked; 0: "Too many wrong codes from the computer. Press
    /// a volume key for a new code."
    Locked {
        wait_s: u32,
    },
    Paired,
    /// The installer is too old or too new.
    Version,
    /// First-boot setup is running.
    Setup,
    /// "Setup stopped at: <step>".
    Stopped {
        step: String,
    },
    NoSpace,
    /// "The file from the computer was damaged. Sending again."
    Damaged,
    /// First-boot setup is done: the screen program ends, and the display
    /// manager is no longer held back (§3.3).
    Off,
}

/// A line for the owner's session agent (§2.8), on the daemon's socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLine {
    /// The link is open and no host is paired: keep the notice.
    Open { boots_left: u8, closes_s: u32 },
    /// Show this code in the window. Never in a notification's text, a
    /// file or a log.
    Code { code: Code, valid_s: u32 },
    /// A host is paired: no notice, no window.
    Paired,
    /// The link is closed for this boot.
    Closed { boots_left: u8 },
    /// The link ended.
    Ended {
        reason: &'static str,
        missing: Vec<Missing>,
    },
}

impl AgentLine {
    /// The line as the socket carries it. It holds the code: not for a log.
    pub fn line(&self) -> String {
        match self {
            AgentLine::Open {
                boots_left,
                closes_s,
            } => format!("open {boots_left} {closes_s}"),
            AgentLine::Code { code, valid_s } => format!("code {} {valid_s}", code.digits()),
            AgentLine::Paired => "paired".to_string(),
            AgentLine::Closed { boots_left } => format!("closed {boots_left}"),
            AgentLine::Ended { reason, missing } => {
                let mut s = format!("ended {reason}");
                for m in missing {
                    s.push(' ');
                    s.push_str(m.as_str());
                }
                s
            }
        }
    }
}

/// For the journal: the kind of thing, never content (§9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// A frame with no session open and no `hello` in it was dropped.
    Dropped,
    Frame(FrameError),
    Message(MsgError),
    /// A data frame with no stream open was dropped (§5.1): not an error.
    LateData,
    SessionIdle,
    StreamIdle,
    /// An answer for a ticket nobody waits for.
    StaleTicket,
    /// The shell's answer does not fit what was asked.
    BadOutcome,
    /// A message of this type arrived, or left.
    Received(&'static str),
    Sent(&'static str),
}

/// Something for the shell to do. One with a ticket has an answer, which
/// comes back as [`PhoneInput::Done`]; the others just have to succeed, and
/// one that cannot is a [`PhoneInput::Fault`]. Every write is a temporary
/// file in the target's own directory, synced, then renamed, then the
/// directory synced (§6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Offer the link in this boot: `omamobile-link-gadget add`. Only ever
    /// in what [`Phone::start`] returns, after everything that must be on
    /// disk first.
    Offer,
    Screen(Screen),
    /// A new `hello` ended the session before it: cancel that session's
    /// frames that the bus has not yet taken (§5.3, §5.7), so that the
    /// answer to the `hello` is the next thing the host reads from here on.
    DropUnsent,
    /// Tell the owner's session agent, if one is connected.
    Agent(AgentLine),
    /// Write the state file.
    Persist(Persistent),
    /// Answer: [`Outcome::Facts`].
    ReadFacts {
        ticket: Ticket,
    },
    /// Write `conf` as the `firstboot.conf` candidate (mode 600, tmpfs) and
    /// run `omamobile-firstboot --check --porcelain` on it. Answer:
    /// [`Outcome::Problems`], empty when first boot has no objection.
    CheckAnswers {
        ticket: Ticket,
        conf: String,
    },
    /// `retry` false, in §6.5's order: write `boot-update.conf` from the
    /// descriptor (the plan's Q3), copy the checked candidate to
    /// `/etc/omamobile/firstboot.conf` (mode 600) and start the unit. True:
    /// start the unit again with the kept file. Answer: [`Outcome::Done`].
    StartFirstboot {
        ticket: Ticket,
        retry: bool,
    },
    /// Write `boot-update.conf` from the descriptor: it is missing on a
    /// phone whose answers were taken (start-up only).
    BootUpdateConf,
    /// Run `tools/nv-backup`'s checks. Answer: [`Outcome::Parts`] or
    /// [`Outcome::Precondition`].
    BackupList {
        ticket: Ticket,
    },
    /// Exactly `len` bytes of the partition at `offset`, opened read-only.
    /// Answer: [`Outcome::Data`].
    BackupRead {
        ticket: Ticket,
        name: String,
        offset: u64,
        len: u32,
    },
    /// Read the whole partition again. Answer: [`Outcome::Digest`].
    BackupDigest {
        ticket: Ticket,
        name: String,
    },
    /// Keep `sums` (`sha256sum`'s format, one `NAME.img` line per checked
    /// partition) as the pending `modem-nv.sha256`. Answer: [`Outcome::Done`].
    BackupKeep {
        ticket: Ticket,
        sums: String,
    },
    /// Delete the pending `modem-nv.sha256` (start-up only).
    SumsDelete,
    /// Open the image's file for a transfer of `size` bytes that goes on at
    /// `resume_at`: check the free space (the bytes still to come and the
    /// working space of §6.4), and only then cut the file to that length (0:
    /// start it anew). Answer: [`Outcome::FactoryOpened`] or
    /// [`Outcome::NoSpace`], with nothing changed.
    FactoryOpen {
        ticket: Ticket,
        size: u64,
        resume_at: u64,
    },
    /// Write at `offset`; with `sync`, sync the file before going on.
    FactoryWrite {
        offset: u64,
        data: Vec<u8>,
        sync: bool,
    },
    /// Cut the image's file to `len` bytes (start-up only).
    FactoryTruncate {
        len: u64,
    },
    /// Read the whole file back. Answer: [`Outcome::Digest`].
    FactoryDigest {
        ticket: Ticket,
    },
    /// Delete the image's file.
    FactoryDelete,
    /// Start the provisioning unit; report with
    /// [`PhoneInput::Progress`] and [`PhoneInput::ProvisionEnded`].
    StartProvision,
    /// The provisioning unit is still active after a restart of the daemon:
    /// go on watching it, and report as after [`Effect::StartProvision`].
    ProvisionWatch,
    /// Remove the extraction's working directory, if there is one.
    WorkDirRemove,
    /// Delete a candidate or a temporary file of the daemon's own.
    Sweep,
    /// Wait for the session and run the smoke test. Answer: [`Outcome::Report`].
    Smoke {
        ticket: Ticket,
    },
    /// `systemctl reboot`, after the frames before it are written.
    Restart,
    /// Close the link for this boot (§2.2): put the gadget back, drop the
    /// sleep inhibitor. The next boot opens it again if a setup boot is left.
    Close,
    /// The end as `finished`: delete the marker, the token and the state,
    /// write `done` (`finished`), put the gadget back if it was changed.
    Finish,
    /// The end as `expired` (§2.2): delete the marker, the factory image
    /// and the working directory, write `done` (`expired`, with what is
    /// missing), and keep the state file as it was just written (the stage,
    /// no token) and the pending checksums. No gadget change: it only ever
    /// comes at a start.
    Expire {
        missing: Vec<Missing>,
    },
    /// The link is not this phone's, or cannot be trusted to be: delete the
    /// marker, the factory image and the working directory if there are
    /// any, and write `done` with the reason. No gadget change.
    Refuse {
        reason: Refusal,
    },
    /// `done` exists: delete the marker that is still there, and exit.
    DeleteMarker,
    /// The machine cannot go on (a fault): stop the daemon, and let it start
    /// again from what it then finds.
    Stop,
    Note(Note),
}

/// The answer to an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The shell could not do it. The host gets `internal`.
    Failed,
    Done,
    Facts(Facts),
    Problems(Vec<Problem>),
    Parts(Vec<PartFound>),
    /// The partitions that are writable, mounted, missing or doubled.
    Precondition(Vec<String>),
    Data(Vec<u8>),
    Digest([u8; 32]),
    /// `have`: the file's length now, at most the `resume_at` asked for.
    FactoryOpened {
        have: u64,
    },
    NoSpace {
        need: u64,
        free: u64,
    },
    Report(Value),
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneInput<'a> {
    /// FunctionFS `ENABLE`.
    LinkUp,
    /// FunctionFS `DISABLE` or `UNBIND`: the cable, a host reset. Also a
    /// write the bus did not take for 30 s (§5.7).
    LinkDown,
    /// One completed read from the host.
    Frame(&'a [u8]),
    /// The time passed [`Phone::deadline`].
    Tick,
    Done(Ticket, Outcome),
    /// First-boot setup ended; `failed` names the steps that failed.
    FirstbootEnded {
        failed: Vec<String>,
    },
    /// A running step got further.
    Progress(Progress),
    /// The provisioning unit ended. `result` is the worker's result file,
    /// each step and its state; `None` when it left none. The machine
    /// judges it: a run succeeds when every required step is `ok` (§5.6).
    ProvisionEnded {
        result: Option<Vec<StepResult>>,
    },
    /// The owner's session is up (§2.8) though no agent has connected:
    /// three minutes after the display manager started. From here the ten
    /// unpaired minutes of §2.2 can run.
    SessionUp,
    /// The owner's session agent connected to the daemon's socket (the one
    /// peer the daemon accepts). Its first connection in a boot also means
    /// the session is up.
    AgentUp,
    /// The agent went away.
    AgentDown,
    /// The agent's `show`: the owner tapped the notice in the unlocked
    /// session. An act at the phone (§2.5).
    AgentShow,
    /// The agent's `hide`: the window was closed.
    AgentHide,
    /// A volume key was pressed while the first-start screen is up. An act
    /// at the phone (§2.5).
    Key,
    /// An effect without a ticket could not be carried out.
    Fault,
}

/// What the shell does next, in this order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneOutput {
    /// One frame: one bulk transfer to the host.
    Send(Vec<u8>),
    Do(Effect),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lock {
    Open,
    Until(u64),
    /// Until someone acts at the phone.
    Act,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    epoch: u64,
    nonce: [u8; 32],
    host_nonce: [u8; 32],
    max_data: u32,
    paired: bool,
    rx_next: u32,
    last_rx: u64,
    /// A data frame with no stream open was answered since the last message.
    late_data_said: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stream {
    None,
    /// Host to phone: the factory image.
    Factory {
        next: u64,
        acked: u64,
        last: u64,
    },
    /// Phone to host: one radio partition.
    Backup {
        name: String,
        size: u64,
        next: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Part {
    found: PartFound,
    read_done: bool,
    /// The computer's digest, once a second read on the phone gave the same.
    digest: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpKind {
    CheckAnswers,
    StartFirstboot,
    BackupList,
    BackupChunk,
    BackupVerify { name: String, want: [u8; 32] },
    CommitList,
    CommitKeep,
    FactoryOpen(FactoryBegin, u64),
    FactoryVerify,
    ProvisionFacts,
    Smoke,
}

/// The one step that owes the host an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Op {
    ticket: Ticket,
    epoch: u64,
    re: u64,
    kind: OpKind,
    last_note: u64,
}

/// The phone's side of the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    cfg: PhoneConfig,
    p: Persistent,
    link_up: bool,
    session: Option<Session>,
    epochs: u64,
    tx_next: u32,
    tickets: Ticket,
    code: Option<Code>,
    /// Wrong codes since the last act at the phone.
    wrong: u32,
    lock: Lock,
    stream: Stream,
    backup: Option<Vec<Part>>,
    op: Option<Op>,
    status_waiting: Vec<(Ticket, u64, u64)>,
    /// The owner's session agent is connected.
    agent: bool,
    /// The session's code window closes then.
    window_until: Option<u64>,
    /// The owner's session is up since then.
    session_up: Option<u64>,
    /// The latest end of a paired session, of a job, or of a step that owed
    /// an answer.
    quiet_from: u64,
    restart_taken: bool,
    stopped: bool,
}

type Out = Vec<PhoneOutput>;

fn draw_code(rng: &mut dyn Entropy) -> Code {
    // Every code as likely as another: draw again above the last whole
    // million a u32 holds. A source that never gives a value below it (a
    // test's) ends the loop anyway.
    let mut n = 0u32;
    for _ in 0..64 {
        let mut b = [0u8; 4];
        rng.fill(&mut b);
        n = u32::from_le_bytes(b);
        if n < 4_294_000_000 {
            break;
        }
    }
    Code::from_number(n % 1_000_000)
}

impl Phone {
    /// The machine at the daemon's start, from what the daemon found
    /// (§6.5). The list it returns is carried out like any other, in order:
    /// it ends in [`Effect::Offer`] when this boot offers the link, and the
    /// machine is [`Phone::stopped`] when it does not.
    pub fn start(
        cfg: PhoneConfig,
        found: &Found,
        now: u64,
        rng: &mut dyn Entropy,
    ) -> (Phone, Vec<PhoneOutput>) {
        let mut cfg = cfg;
        cfg.max_data = cfg.max_data.clamp(MIN_MAX_DATA, MAX_DATA_LIMIT);
        cfg.ack_every = cfg.ack_every.clamp(1, MAX_NUMBER / 4);
        cfg.window = cfg
            .window
            .clamp(cfg.ack_every + cfg.max_data as u64, MAX_NUMBER);
        let r = reconcile(found, &cfg.optional);
        let mut out: Out = r.effects.into_iter().map(PhoneOutput::Do).collect();
        let (p, stopped) = match r.start {
            Start::Offer(p) => (p, false),
            Start::Ended => (Persistent::default(), true),
        };
        let mut me = Phone {
            cfg,
            p,
            link_up: false,
            session: None,
            epochs: 0,
            tx_next: 0,
            tickets: 0,
            code: None,
            wrong: 0,
            lock: Lock::Open,
            stream: Stream::None,
            backup: None,
            op: None,
            status_waiting: Vec::new(),
            agent: false,
            window_until: None,
            session_up: None,
            quiet_from: now,
            restart_taken: false,
            stopped,
        };
        if stopped {
            return (me, out);
        }
        if me.screen_up() {
            match me.p.stage {
                Stage::Firstboot => do_(&mut out, Effect::Screen(Screen::Setup)),
                Stage::FirstbootFailed => {
                    let step = me.p.firstboot_failed.first().cloned().unwrap_or_default();
                    do_(&mut out, Effect::Screen(Screen::Stopped { step }));
                }
                _ => {}
            }
            let code = draw_code(rng);
            me.code = Some(code);
            do_(&mut out, Effect::Screen(Screen::Code(code)));
        }
        do_(&mut out, Effect::Offer);
        (me, out)
    }

    /// The state file as the machine has it.
    pub fn state(&self) -> &Persistent {
        &self.p
    }
    pub fn stage(&self) -> Stage {
        self.p.stage
    }
    /// Is a session open and paired?
    pub fn paired(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.paired)
    }
    /// The code that pairs now: on the first-start screen, or in the
    /// session's window. For those and for tests; not for a log.
    pub fn code(&self) -> Option<Code> {
        self.code
    }
    /// Wrong codes counted since the last act at the phone.
    pub fn wrong_tries(&self) -> u32 {
        self.wrong
    }
    /// The data payload limit: the session's, or the daemon's own before one.
    /// The shell's read buffer is [`frame::read_buffer_len`] of the daemon's own.
    pub fn max_data(&self) -> u32 {
        self.session
            .as_ref()
            .map_or(self.cfg.max_data, |s| s.max_data)
    }
    /// The machine has ended for this boot (the start-up rules, `finish`,
    /// ten unpaired minutes, a fault): feed it nothing more.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// The first-start screen is up: no account yet, or first-boot setup
    /// runs or has failed and the display manager is held back (§2.5, §3.3).
    fn screen_up(&self) -> bool {
        matches!(
            self.p.stage,
            Stage::Waiting | Stage::Firstboot | Stage::FirstbootFailed
        )
    }

    /// A job runs that needs no host: the ten minutes do not (§2.2).
    fn job_running(&self) -> bool {
        self.p.stage == Stage::Firstboot || self.p.provision == JobState::Running
    }

    /// When the link closes for this boot, if that clock is running (§2.2).
    fn close_at(&self) -> Option<u64> {
        let up = self.session_up?;
        if !has_account(self.p.stage) || self.paired() || self.job_running() || self.op.is_some() {
            return None;
        }
        Some(up.max(self.quiet_from) + UNPAIRED_MS)
    }

    fn closes_s(&self, now: u64) -> u32 {
        self.close_at()
            .map_or(0, |t| t.saturating_sub(now).div_ceil(1000).max(1) as u32)
    }

    /// When to send [`PhoneInput::Tick`] at the latest, if nothing else
    /// happens first.
    pub fn deadline(&self) -> Option<u64> {
        if self.stopped {
            return None;
        }
        let mut d: Option<u64> = None;
        let mut add = |t: u64| d = Some(d.map_or(t, |x| x.min(t)));
        if let Lock::Until(t) = self.lock {
            add(t);
        }
        if let Some(s) = &self.session {
            add(s.last_rx + SESSION_IDLE_MS);
            if let Some(op) = &self.op {
                if op.epoch == s.epoch && op.kind != OpKind::BackupChunk {
                    add(op.last_note + PROGRESS_EVERY_MS);
                }
            }
        }
        if let Stream::Factory { last, .. } = self.stream {
            add(last + STREAM_IDLE_MS);
        }
        if let Some(t) = self.window_until {
            add(t);
        }
        if let Some(t) = self.close_at() {
            add(t);
        }
        d
    }

    /// One step: what happened at `now` (milliseconds on a clock that does
    /// not go back and does not follow the wall clock: a new phone's can be
    /// wrong), and what to do about it.
    pub fn handle(
        &mut self,
        now: u64,
        input: PhoneInput<'_>,
        rng: &mut dyn Entropy,
    ) -> Vec<PhoneOutput> {
        let mut out = Vec::new();
        if self.stopped {
            return out;
        }
        match input {
            PhoneInput::LinkUp => {
                self.end_session(now, &mut out);
                self.link_up = true;
                self.tx_next = 0;
            }
            PhoneInput::LinkDown => {
                self.end_session(now, &mut out);
                self.link_up = false;
            }
            PhoneInput::Frame(bytes) => self.on_frame(now, bytes, rng, &mut out),
            PhoneInput::Tick => self.on_tick(now, &mut out),
            PhoneInput::Done(ticket, outcome) => self.on_done(now, ticket, outcome, &mut out),
            PhoneInput::FirstbootEnded { failed } => {
                if self.p.stage == Stage::Firstboot {
                    self.quiet_from = now;
                    if failed.is_empty() {
                        self.p.stage = Stage::FirstbootDone;
                        self.p.firstboot_failed.clear();
                        do_(&mut out, Effect::Persist(self.p.clone()));
                        // The session owns the display from here on: the
                        // code on the first-start screen is gone with it
                        self.code = None;
                        do_(&mut out, Effect::Screen(Screen::Off));
                    } else {
                        let failed = failed_steps(&failed);
                        self.p.stage = Stage::FirstbootFailed;
                        do_(
                            &mut out,
                            Effect::Screen(Screen::Stopped {
                                step: failed[0].clone(),
                            }),
                        );
                        self.p.firstboot_failed = failed;
                        do_(&mut out, Effect::Persist(self.p.clone()));
                    }
                }
            }
            PhoneInput::Progress(p) => {
                if self.paired() {
                    self.send(None, PhoneMsg::Progress(p), &mut out);
                    if let Some(op) = &mut self.op {
                        op.last_note = now;
                    }
                }
            }
            PhoneInput::ProvisionEnded { result } => {
                if self.p.provision == JobState::Running {
                    self.quiet_from = now;
                    match judge(result.as_deref(), &self.cfg.optional) {
                        Ok(optional_failed) => {
                            // §6.5's order: the stage, then the image and the
                            // working directory, then the event
                            self.p.provision = JobState::Done;
                            self.p.stage = Stage::Provisioned;
                            self.p.optional_failed = optional_failed;
                            self.p.factory = None;
                            do_(&mut out, Effect::Persist(self.p.clone()));
                            do_(&mut out, Effect::FactoryDelete);
                            do_(&mut out, Effect::WorkDirRemove);
                            if self.paired() {
                                let steps = result
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|s| StepResult {
                                        step: clean(&s.step),
                                        state: s.state,
                                    })
                                    .filter(|s| !s.step.is_empty())
                                    .take(MAX_STEPS)
                                    .collect();
                                self.send(None, PhoneMsg::ProvisionDone { steps }, &mut out);
                            }
                        }
                        Err(step) => {
                            self.p.provision = JobState::Failed;
                            self.p.provision_failed = step.clone();
                            do_(&mut out, Effect::Persist(self.p.clone()));
                            do_(&mut out, Effect::WorkDirRemove);
                            do_(
                                &mut out,
                                Effect::Screen(Screen::Stopped { step: step.clone() }),
                            );
                            if self.paired() {
                                self.refuse(None, ErrorCode::ProvisionFailed { step }, &mut out);
                            }
                        }
                    }
                }
            }
            PhoneInput::SessionUp => {
                if self.session_up.is_none() {
                    self.session_up = Some(now);
                }
            }
            PhoneInput::AgentUp => {
                self.agent = true;
                // The agent's first connection in a boot is what "the
                // session is up" means (§2.8)
                if self.session_up.is_none() {
                    self.session_up = Some(now);
                }
                self.tell_agent(now, &mut out);
            }
            PhoneInput::AgentDown => {
                self.agent = false;
                self.hide_window();
            }
            PhoneInput::AgentShow => {
                // The window opens only in the owner's unlocked session,
                // and only while a code is of use
                if self.agent && !self.screen_up() && !self.paired() {
                    self.wrong = 0;
                    self.lock = Lock::Open;
                    let code = draw_code(rng);
                    self.code = Some(code);
                    self.window_until = Some(now + SESSION_CODE_MS);
                    let valid_s = (SESSION_CODE_MS / 1000) as u32;
                    do_(&mut out, Effect::Agent(AgentLine::Code { code, valid_s }));
                }
            }
            PhoneInput::AgentHide => self.hide_window(),
            PhoneInput::Key => {
                if self.screen_up() {
                    self.wrong = 0;
                    self.lock = Lock::Open;
                    let code = draw_code(rng);
                    self.code = Some(code);
                    if !self.paired() {
                        do_(&mut out, Effect::Screen(Screen::Code(code)));
                    }
                }
            }
            PhoneInput::Fault => {
                if self.session.is_some() {
                    self.refuse(None, ErrorCode::Internal, &mut out);
                }
                self.session = None;
                self.stopped = true;
                do_(&mut out, Effect::Stop);
            }
        }
        out
    }

    // --- Sessions and frames ----------------------------------------------

    /// The session's window closes: its code is gone.
    fn hide_window(&mut self) {
        if self.window_until.take().is_some() {
            self.code = None;
        }
    }

    /// The agent's notice follows whether a host is paired.
    fn tell_agent(&mut self, now: u64, out: &mut Out) {
        if !self.agent || self.screen_up() {
            return;
        }
        let line = if self.paired() {
            AgentLine::Paired
        } else {
            match (self.code, self.window_until) {
                (Some(code), Some(until)) => AgentLine::Code {
                    code,
                    valid_s: until.saturating_sub(now).div_ceil(1000) as u32,
                },
                _ => AgentLine::Open {
                    boots_left: self.p.boots_left(),
                    closes_s: self.closes_s(now),
                },
            }
        };
        do_(out, Effect::Agent(line));
    }

    fn end_session(&mut self, now: u64, out: &mut Out) {
        let was_paired = self.paired();
        self.session = None;
        self.status_waiting.clear();
        // A stream stays as it was at its last acknowledged offset: the
        // factory file's synced length is in the state file already.
        if matches!(self.stream, Stream::Backup { .. }) {
            self.op = None;
        }
        self.stream = Stream::None;
        if was_paired {
            // No host is paired any more: the ten minutes run from here,
            // and the code, or the notice, is back
            self.quiet_from = now;
            if self.screen_up() {
                if let (Some(code), None) = (self.code, self.locked(now)) {
                    do_(out, Effect::Screen(Screen::Code(code)));
                }
            } else {
                self.tell_agent(now, out);
            }
        }
    }

    fn ticket(&mut self) -> Ticket {
        self.tickets += 1;
        self.tickets
    }

    fn send(&mut self, re: Option<u64>, msg: PhoneMsg, out: &mut Out) {
        let t = msg.t();
        let stage = self.p.stage;
        let json = match (PhoneFrame { re, msg }).to_json() {
            Ok(j) => j,
            Err(e) => {
                // Never sent: a message the host would refuse. `internal`
                // always fits.
                do_(out, Effect::Note(Note::Message(e)));
                let re = if PhoneMsg::re_rule(t) == Re::Event {
                    None
                } else {
                    re
                };
                let msg = PhoneMsg::Error(ErrorMsg::new(ErrorCode::Internal, stage));
                match (PhoneFrame { re, msg }).to_json() {
                    Ok(j) => j,
                    Err(_) => return,
                }
            }
        };
        if let Ok(bytes) = frame::encode(
            Kind::Message,
            self.tx_next,
            0,
            json.as_bytes(),
            self.max_data(),
        ) {
            self.tx_next = self.tx_next.wrapping_add(1);
            do_(out, Effect::Note(Note::Sent(t)));
            out.push(PhoneOutput::Send(bytes));
        }
    }

    fn refuse(&mut self, re: Option<u64>, code: ErrorCode, out: &mut Out) {
        let stage = self.p.stage;
        self.send(re, PhoneMsg::Error(ErrorMsg::new(code, stage)), out);
    }

    /// §5.1: a wrong frame gets `protocol` and ends the session. Nothing is
    /// counted toward the pairing lock (§2.5).
    fn protocol_error(&mut self, now: u64, note: Note, out: &mut Out) {
        do_(out, Effect::Note(note));
        self.refuse(None, ErrorCode::Protocol, out);
        self.end_session(now, out);
    }

    /// Counts a wrong code: the only thing that is counted (§2.5).
    /// `Some(wait_s)` when `pair` is locked by it.
    fn wrong_code(&mut self, now: u64, rng: &mut dyn Entropy, out: &mut Out) -> Option<u32> {
        self.wrong = self.wrong.saturating_add(1);
        if self.wrong >= TRIES_PER_ACT {
            // No code is shown until someone acts at the phone
            self.lock = Lock::Act;
            self.code = None;
            if self.window_until.take().is_some() {
                self.tell_agent(now, out);
            }
            do_(out, Effect::Screen(Screen::Locked { wait_s: 0 }));
            return Some(0);
        }
        if self.wrong.is_multiple_of(TRIES_PER_CODE) {
            let wait = LOCK_MS[((self.wrong / TRIES_PER_CODE) as usize - 1).min(LOCK_MS.len() - 1)];
            self.lock = Lock::Until(now + wait);
            // A code is used once: five wrong ones replace it
            let code = draw_code(rng);
            self.code = Some(code);
            if self.window_until.is_some() {
                self.tell_agent(now, out);
            }
            let wait_s = (wait / 1000) as u32;
            do_(out, Effect::Screen(Screen::Locked { wait_s }));
            return Some(wait_s);
        }
        None
    }

    /// The seconds `pair` is locked for: `Some(0)` until someone acts at
    /// the phone.
    fn locked(&self, now: u64) -> Option<u32> {
        match self.lock {
            Lock::Open => None,
            Lock::Act => Some(0),
            Lock::Until(t) if t > now => Some((t - now).div_ceil(1000).max(1) as u32),
            Lock::Until(_) => None,
        }
    }

    fn pair_mode(&self, now: u64) -> (PairMode, u32) {
        match self.locked(now) {
            Some(wait_s) => (PairMode::Locked, wait_s),
            None if self.code.is_some() => (PairMode::Code, 0),
            // The owner can make the phone show one: the notice in the
            // unlocked session (§2.8)
            None if self.agent && !self.screen_up() => (PairMode::Ask, 0),
            None if self.p.token_hash.is_some() => (PairMode::Token, 0),
            // No code can be shown now and no token exists
            None => (PairMode::Locked, 0),
        }
    }

    fn on_frame(&mut self, now: u64, bytes: &[u8], rng: &mut dyn Entropy, out: &mut Out) {
        if !self.link_up {
            do_(out, Effect::Note(Note::Dropped));
            return;
        }
        let decoded = frame::decode(bytes, self.max_data());
        // A `hello` is always frame 0, at any time, and starts the
        // numbering again in both directions (§5.3)
        if let Ok(f) = &decoded {
            if f.kind == Kind::Message && f.seq == 0 {
                if let Ok(HostFrame {
                    id,
                    msg: HostMsg::Hello(h),
                }) = HostFrame::parse(f.payload)
                {
                    self.on_hello(now, id, h, rng, out);
                    return;
                }
            }
        }
        let Some(s) = &mut self.session else {
            do_(out, Effect::Note(Note::Dropped));
            return;
        };
        s.last_rx = now;
        let f = match decoded {
            Ok(f) => f,
            Err(e) => return self.protocol_error(now, Note::Frame(e), out),
        };
        if f.seq != s.rx_next {
            let e = FrameError::Seq {
                expected: s.rx_next,
                got: f.seq,
            };
            return self.protocol_error(now, Note::Frame(e), out);
        }
        s.rx_next = s.rx_next.wrapping_add(1);
        match f.kind {
            Kind::Data => self.on_data(now, f.offset, f.payload, out),
            Kind::Message => {
                s.late_data_said = false;
                match HostFrame::parse(f.payload) {
                    Ok(HostFrame {
                        msg: HostMsg::Hello(_),
                        ..
                    }) => {
                        let e = FrameError::Seq {
                            expected: 0,
                            got: f.seq,
                        };
                        self.protocol_error(now, Note::Frame(e), out)
                    }
                    Ok(m) => self.on_message(now, m, rng, out),
                    Err(MsgError::UnknownType { id }) => self.refuse(id, ErrorCode::Unknown, out),
                    Err(e) => self.protocol_error(now, Note::Message(e), out),
                }
            }
        }
    }

    fn on_hello(&mut self, now: u64, id: u64, h: HostHello, rng: &mut dyn Entropy, out: &mut Out) {
        do_(out, Effect::Note(Note::Received("hello")));
        // A `hello` ends the session before it; the new one is unpaired
        if self.session.is_some() {
            do_(out, Effect::DropUnsent);
        }
        self.end_session(now, out);
        self.tx_next = 0;
        let common = h.proto.iter().filter(|n| self.cfg.protos.contains(n)).max();
        let Some(&proto) = common else {
            do_(out, Effect::Screen(Screen::Version));
            let proto = self.cfg.protos.clone();
            let code = ErrorCode::Version {
                proto,
                host_nonce: h.nonce,
            };
            self.refuse(Some(id), code, out);
            return;
        };
        let mut nonce = [0u8; 32];
        rng.fill(&mut nonce);
        self.epochs += 1;
        let max_data = h.max_data.min(self.cfg.max_data);
        self.session = Some(Session {
            epoch: self.epochs,
            nonce,
            host_nonce: h.nonce,
            max_data,
            paired: false,
            rx_next: 1,
            last_rx: now,
            late_data_said: false,
        });
        let (pair, wait_s) = self.pair_mode(now);
        let hello = PhoneHello {
            proto,
            link: self.cfg.link.clone(),
            device: self.cfg.device.clone(),
            max_data,
            nonce,
            host_nonce: h.nonce,
            stage: self.p.stage,
            pair,
            wait_s,
        };
        self.send(Some(id), PhoneMsg::Hello(hello), out);
    }

    fn on_data(&mut self, now: u64, offset: u64, payload: &[u8], out: &mut Out) {
        let size = self.p.factory.as_ref().map_or(0, |f| f.size);
        let Stream::Factory { next, acked, last } = &mut self.stream else {
            // What an honest host sends after its stream was suspended
            // behind its back (§5.1): dropped, answered once, the session
            // kept, nothing counted
            do_(out, Effect::Note(Note::LateData));
            let said = self
                .session
                .as_mut()
                .is_some_and(|s| core::mem::replace(&mut s.late_data_said, true));
            if !said {
                self.refuse(None, ErrorCode::NotAllowed, out);
            }
            return;
        };
        if offset != *next {
            let e = FrameError::StreamOffset {
                expected: *next,
                got: offset,
            };
            return self.protocol_error(now, Note::Frame(e), out);
        }
        let end = offset + payload.len() as u64; // decode refused an overflow
        if end > size {
            return self.protocol_error(
                now,
                Note::Frame(FrameError::StreamOverrun { size, end }),
                out,
            );
        }
        *next = end;
        *last = now;
        let sync = end == size || end / self.cfg.ack_every > *acked / self.cfg.ack_every;
        if sync {
            *acked = end;
        }
        do_(
            out,
            Effect::FactoryWrite {
                offset,
                data: payload.to_vec(),
                sync,
            },
        );
        if sync {
            if let Some(f) = &mut self.p.factory {
                f.synced = end;
            }
            do_(out, Effect::Persist(self.p.clone()));
            self.send(None, PhoneMsg::Ack { offset: end }, out);
        }
    }

    // --- Messages ------------------------------------------------------------

    fn begin(&mut self, now: u64, re: u64, kind: OpKind) -> Ticket {
        let ticket = self.ticket();
        let epoch = self.session.as_ref().map_or(0, |s| s.epoch);
        self.op = Some(Op {
            ticket,
            epoch,
            re,
            kind,
            last_note: now,
        });
        ticket
    }

    /// A host is paired: the first-start screen says so, the notice in the
    /// session goes, and the code that did it is used up.
    fn now_paired(&mut self, now: u64, out: &mut Out) {
        if let Some(s) = &mut self.session {
            s.paired = true;
        }
        if self.screen_up() {
            do_(out, Effect::Screen(Screen::Paired));
        } else {
            self.hide_window();
            self.tell_agent(now, out);
        }
    }

    fn on_message(&mut self, now: u64, m: HostFrame, rng: &mut dyn Entropy, out: &mut Out) {
        let re = Some(m.id);
        do_(out, Effect::Note(Note::Received(m.msg.t())));
        let Some(s) = &self.session else { return };
        let (paired, nonce, host_nonce, max_data) = (s.paired, s.nonce, s.host_nonce, s.max_data);
        let stage = self.p.stage;
        match m.msg {
            HostMsg::Hello(_) => {}
            HostMsg::Ping => self.send(re, PhoneMsg::Pong, out),
            HostMsg::Bye => {
                self.send(re, PhoneMsg::Bye, out);
                self.end_session(now, out);
            }
            HostMsg::Status => {
                if self.status_waiting.len() >= MAX_STATUS_WAITING {
                    return self.refuse(re, ErrorCode::Busy, out);
                }
                let ticket = self.ticket();
                self.status_waiting.push((ticket, self.epochs, m.id));
                do_(out, Effect::ReadFacts { ticket });
            }
            HostMsg::Pair { code } => {
                if paired {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                if let Some(wait_s) = self.locked(now) {
                    return self.refuse(re, ErrorCode::Locked { wait_s }, out);
                }
                let Some(shown) = self.code else {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                };
                if !ct_eq(shown.bytes(), code.bytes()) {
                    return match self.wrong_code(now, rng, out) {
                        Some(wait_s) => self.refuse(re, ErrorCode::Locked { wait_s }, out),
                        None => {
                            let left = TRIES_PER_CODE - self.wrong % TRIES_PER_CODE;
                            do_(out, Effect::Screen(Screen::WrongCode { left }));
                            self.refuse(re, ErrorCode::BadCode { left }, out)
                        }
                    };
                }
                // Every right code makes a new token, and the one before
                // stops working
                let mut token = [0u8; 32];
                rng.fill(&mut token);
                self.p.token_hash = Some(sha256(&token));
                do_(out, Effect::Persist(self.p.clone()));
                // A code is used once: the first-start screen gets another
                // for when no host is paired again
                self.code = self.screen_up().then(|| draw_code(rng));
                self.now_paired(now, out);
                let serial_proof = serial_proof(&host_nonce, &nonce, &self.cfg.serial);
                self.send(
                    re,
                    PhoneMsg::Paired {
                        token: Some(Token(token)),
                        serial_proof,
                    },
                    out,
                );
            }
            HostMsg::Resume { token } => {
                // Never locked (§2.5): the lock protects a code of a
                // million values, and a token is 256 random bits
                if paired {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let Some(kept) = self.p.token_hash else {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                };
                if !ct_eq(&kept, &sha256(&token.0)) {
                    return self.refuse(re, ErrorCode::BadToken, out);
                }
                self.now_paired(now, out);
                let serial_proof = serial_proof(&host_nonce, &nonce, &self.cfg.serial);
                self.send(
                    re,
                    PhoneMsg::Paired {
                        token: None,
                        serial_proof,
                    },
                    out,
                );
            }
            _ if !paired => self.refuse(re, ErrorCode::NotPaired, out),
            HostMsg::Abort => match self.stream {
                Stream::None => self.refuse(re, ErrorCode::NotAllowed, out),
                _ => {
                    if matches!(self.stream, Stream::Backup { .. }) {
                        self.op = None;
                    }
                    self.stream = Stream::None;
                    self.send(re, PhoneMsg::Aborted, out);
                }
            },
            _ if self.op.is_some() => self.refuse(re, ErrorCode::Busy, out),
            HostMsg::FactoryEnd => {
                let size = self.p.factory.as_ref().map_or(0, |f| f.size);
                match self.stream {
                    Stream::Factory { next, .. } if next == size => {
                        self.stream = Stream::None;
                        let ticket = self.begin(now, m.id, OpKind::FactoryVerify);
                        do_(out, Effect::FactoryDigest { ticket });
                    }
                    _ => self.refuse(re, ErrorCode::NotAllowed, out),
                }
            }
            // With the image's stream open, only its own messages go on
            _ if self.stream != Stream::None => self.refuse(re, ErrorCode::NotAllowed, out),
            HostMsg::Answers(a) => {
                // At `waiting` only: no account, password or key can be
                // added to a phone that has an account (§2.7)
                if stage != Stage::Waiting {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                match a.validate() {
                    Err(problems) => self.refuse(re, ErrorCode::Problems { problems }, out),
                    Ok(valid) => {
                        let ticket = self.begin(now, m.id, OpKind::CheckAnswers);
                        do_(
                            out,
                            Effect::CheckAnswers {
                                ticket,
                                conf: ValidAnswers::firstboot_conf(&valid),
                            },
                        );
                    }
                }
            }
            HostMsg::FirstbootRetry => {
                if stage != Stage::FirstbootFailed {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let ticket = self.begin(now, m.id, OpKind::StartFirstboot);
                do_(
                    out,
                    Effect::StartFirstboot {
                        ticket,
                        retry: true,
                    },
                );
            }
            HostMsg::BackupList => {
                if !stage.before_provisioned() {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let ticket = self.begin(now, m.id, OpKind::BackupList);
                do_(out, Effect::BackupList { ticket });
            }
            HostMsg::BackupRead { name, offset } => {
                let part = self
                    .backup
                    .as_mut()
                    .and_then(|b| b.iter_mut().find(|p| p.found.name == name));
                let Some(part) = part.filter(|p| {
                    stage.before_provisioned()
                        && offset <= p.found.size
                        && offset % max_data as u64 == 0
                }) else {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                };
                let size = part.found.size;
                part.read_done = offset == size;
                part.digest = None;
                self.send(
                    re,
                    PhoneMsg::DataBegin {
                        name: name.clone(),
                        size,
                        offset,
                    },
                    out,
                );
                if offset == size {
                    return self.send(None, PhoneMsg::DataEnd { name }, out);
                }
                self.stream = Stream::Backup {
                    name: name.clone(),
                    size,
                    next: offset,
                };
                let ticket = self.begin(now, m.id, OpKind::BackupChunk);
                let len = (size - offset).min(max_data as u64) as u32;
                do_(
                    out,
                    Effect::BackupRead {
                        ticket,
                        name,
                        offset,
                        len,
                    },
                );
            }
            HostMsg::BackupVerify { name, sha256 } => {
                let ready = self
                    .backup
                    .as_ref()
                    .is_some_and(|b| b.iter().any(|p| p.found.name == name && p.read_done));
                if !ready || !stage.before_provisioned() {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let ticket = self.begin(
                    now,
                    m.id,
                    OpKind::BackupVerify {
                        name: name.clone(),
                        want: sha256,
                    },
                );
                do_(out, Effect::BackupDigest { ticket, name });
            }
            HostMsg::BackupCommit => {
                let ready = self
                    .backup
                    .as_ref()
                    .is_some_and(|b| b.iter().all(|p| p.digest.is_some()));
                if !ready || !stage.before_provisioned() {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let ticket = self.begin(now, m.id, OpKind::CommitList);
                do_(out, Effect::BackupList { ticket });
            }
            HostMsg::FactoryBegin(b) => {
                if !stage.before_provisioned() || self.p.provision == JobState::Running {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                if b.size > MAX_FACTORY_SIZE {
                    return self.refuse(re, ErrorCode::TooLarge, out);
                }
                let resume_at = match &self.p.factory {
                    Some(f) if f.size == b.size && f.sha256 == b.sha256 => f.synced,
                    _ => 0,
                };
                let size = b.size;
                let ticket = self.begin(now, m.id, OpKind::FactoryOpen(b, resume_at));
                do_(
                    out,
                    Effect::FactoryOpen {
                        ticket,
                        size,
                        resume_at,
                    },
                );
            }
            HostMsg::Provision => {
                if stage == Stage::FirstbootFailed {
                    let steps = self.p.firstboot_failed.clone();
                    return self.refuse(re, ErrorCode::FirstbootFailed { steps }, out);
                }
                let verified = self.p.factory.as_ref().is_some_and(|f| f.verified);
                if stage != Stage::FirstbootDone
                    || !self.p.backup_done
                    || !verified
                    || self.p.provision == JobState::Running
                {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let ticket = self.begin(now, m.id, OpKind::ProvisionFacts);
                do_(out, Effect::ReadFacts { ticket });
            }
            HostMsg::Restart => {
                // Once: in the boot in which provisioning ended (§2.7)
                if stage != Stage::Provisioned || self.p.boot != 1 || self.restart_taken {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                self.restart_taken = true;
                self.send(re, PhoneMsg::Restarting, out);
                do_(out, Effect::Restart);
            }
            HostMsg::Smoke => {
                if stage != Stage::Provisioned || self.p.boot != 2 {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                let ticket = self.begin(now, m.id, OpKind::Smoke);
                do_(out, Effect::Smoke { ticket });
            }
            HostMsg::Finish => {
                if stage != Stage::Provisioned {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                // §6.5's order: the stage, the reply, then the rest
                self.p.stage = Stage::Finished;
                self.p.token_hash = None;
                self.p.factory = None;
                do_(out, Effect::Persist(self.p.clone()));
                self.send(re, PhoneMsg::Finished, out);
                self.session = None;
                self.op = None;
                self.code = None;
                self.stopped = true;
                if self.agent {
                    let line = AgentLine::Ended {
                        reason: "finished",
                        missing: Vec::new(),
                    };
                    do_(out, Effect::Agent(line));
                }
                do_(out, Effect::Finish);
            }
        }
    }

    fn on_tick(&mut self, now: u64, out: &mut Out) {
        if matches!(self.lock, Lock::Until(t) if t <= now) {
            self.lock = Lock::Open;
            if let (Some(code), true, false) = (self.code, self.screen_up(), self.paired()) {
                do_(out, Effect::Screen(Screen::Code(code)));
            }
        }
        if self.window_until.is_some_and(|t| now >= t) {
            // Ten minutes in the session's window: the code is gone, the
            // notice is back
            self.hide_window();
            self.tell_agent(now, out);
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| now >= s.last_rx + SESSION_IDLE_MS)
        {
            do_(out, Effect::Note(Note::SessionIdle));
            self.end_session(now, out);
        }
        if matches!(self.stream, Stream::Factory { last, .. } if now >= last + STREAM_IDLE_MS) {
            do_(out, Effect::Note(Note::StreamIdle));
            self.stream = Stream::None;
        }
        let epoch = self.session.as_ref().map(|s| s.epoch);
        if let Some(op) = &mut self.op {
            if Some(op.epoch) == epoch
                && op.kind != OpKind::BackupChunk
                && now >= op.last_note + PROGRESS_EVERY_MS
            {
                op.last_note = now;
                let what = match op.kind {
                    OpKind::FactoryVerify => What::FactoryCheck,
                    OpKind::BackupVerify { .. } => What::BackupVerify,
                    OpKind::Smoke => What::Smoke,
                    _ => What::Working,
                };
                let p = Progress {
                    what,
                    step: String::new(),
                    done: 0,
                    total: 0,
                };
                self.send(None, PhoneMsg::Progress(p), out);
            }
        }
        if self.close_at().is_some_and(|t| now >= t) {
            // Ten minutes with an account and no paired host: closed for
            // this boot. The next boot's start decides what is left.
            self.session = None;
            self.status_waiting.clear();
            self.stream = Stream::None;
            self.hide_window();
            self.code = None;
            self.stopped = true;
            if self.agent {
                let boots_left = self.p.boots_left();
                do_(out, Effect::Agent(AgentLine::Closed { boots_left }));
            }
            do_(out, Effect::Close);
        }
    }

    // --- Answers from the shell ----------------------------------------------

    fn status(&self, now: u64, f: Facts) -> Status {
        let stage = self.p.stage;
        let (factory_state, factory_have, factory_size) = match (&self.p.factory, &self.stream) {
            (None, _) => (FactoryState::None, 0, 0),
            (Some(x), _) if x.verified => (FactoryState::Verified, x.size, x.size),
            (Some(x), Stream::Factory { next, .. }) => (FactoryState::Partial, *next, x.size),
            (Some(x), _) => (FactoryState::Partial, x.synced, x.size),
        };
        Status {
            stage,
            boot: self.p.boot,
            release: clean(&f.release),
            kernel: clean_to(&f.kernel, MAX_KERNEL),
            device: self.cfg.device.clone(),
            firstboot: FirstbootStatus {
                state: match stage {
                    Stage::Waiting => JobState::None,
                    Stage::Firstboot => JobState::Running,
                    Stage::FirstbootFailed => JobState::Failed,
                    _ => JobState::Done,
                },
                step: clean(&f.firstboot_step),
                n: f.firstboot_n,
                of: f.firstboot_of,
                failed: self.p.firstboot_failed.clone(),
            },
            backup_state: match (&self.backup, self.p.backup_done) {
                (_, true) => BackupState::Done,
                (Some(_), _) => BackupState::Listed,
                _ => BackupState::None,
            },
            backup_parts: self
                .backup
                .iter()
                .flatten()
                .map(|p| PartStatus {
                    name: p.found.name.clone(),
                    size: p.found.size,
                    verified: p.digest.is_some(),
                })
                .collect(),
            factory_state,
            factory_have,
            factory_size,
            provision_state: self.p.provision,
            provision_step: match self.p.provision {
                JobState::Failed => self.p.provision_failed.clone(),
                _ => clean(&f.provision_step),
            },
            provision_n: f.provision_n,
            provision_of: f.provision_of,
            provision_optional_failed: self.p.optional_failed.clone(),
            link_boots_left: self.p.boots_left(),
            link_closes_s: self.closes_s(now),
            battery: f.battery.min(100),
            charging: f.charging,
            free: f.free.min(MAX_NUMBER),
        }
    }

    fn on_done(&mut self, now: u64, ticket: Ticket, outcome: Outcome, out: &mut Out) {
        if let Some(i) = self.status_waiting.iter().position(|w| w.0 == ticket) {
            let (_, _, re) = self.status_waiting.remove(i);
            return match outcome {
                Outcome::Facts(f) => {
                    let s = self.status(now, f);
                    self.send(Some(re), PhoneMsg::Status(Box::new(s)), out)
                }
                _ => self.refuse(Some(re), ErrorCode::Internal, out),
            };
        }
        let Some(op) = self.op.take_if(|op| op.ticket == ticket) else {
            do_(out, Effect::Note(Note::StaleTicket));
            return;
        };
        // A step that owed an answer has it: the ten minutes may run again
        self.quiet_from = now;
        // Does the session that asked still wait for the answer?
        let live = self.session.as_ref().is_some_and(|s| s.epoch == op.epoch);
        let re = Some(op.re);
        let max_data = self.max_data();
        match (op.kind, outcome) {
            (OpKind::CheckAnswers, Outcome::Problems(problems)) if live => {
                if problems.is_empty() {
                    let ticket = self.begin(now, op.re, OpKind::StartFirstboot);
                    do_(
                        out,
                        Effect::StartFirstboot {
                            ticket,
                            retry: false,
                        },
                    );
                } else {
                    let problems = problems.into_iter().take(32).collect();
                    self.refuse(re, ErrorCode::Problems { problems }, out);
                }
            }
            (OpKind::StartFirstboot, Outcome::Done) => {
                // §6.5's order: the answers are placed and the unit is
                // started; now the stage, then the reply
                self.p.stage = Stage::Firstboot;
                self.p.firstboot_failed.clear();
                do_(out, Effect::Persist(self.p.clone()));
                do_(out, Effect::Screen(Screen::Setup));
                if live {
                    self.send(re, PhoneMsg::AnswersOk, out);
                }
            }
            (OpKind::BackupList, Outcome::Parts(found)) => {
                if !listing_ok(&found) {
                    do_(out, Effect::Note(Note::BadOutcome));
                    if live {
                        self.refuse(re, ErrorCode::Internal, out);
                    }
                    return;
                }
                let same = self
                    .backup
                    .as_ref()
                    .is_some_and(|b| b.iter().map(|p| &p.found).eq(found.iter()));
                if !same {
                    self.backup = Some(
                        found
                            .into_iter()
                            .map(|found| Part {
                                found,
                                read_done: false,
                                digest: None,
                            })
                            .collect(),
                    );
                }
                if live {
                    let parts = self
                        .backup
                        .iter()
                        .flatten()
                        .map(|p| BackupPart {
                            name: p.found.name.clone(),
                            size: p.found.size,
                            checked: p.found.checked,
                        })
                        .collect();
                    self.send(re, PhoneMsg::BackupParts { parts }, out);
                }
            }
            (OpKind::BackupList | OpKind::CommitList, Outcome::Precondition(names)) => {
                self.backup = None;
                if live {
                    let parts = names
                        .into_iter()
                        .filter(|n| is_part_name(n))
                        .take(MAX_PARTS)
                        .collect();
                    self.refuse(re, ErrorCode::BackupPrecondition { parts }, out);
                }
            }
            (OpKind::BackupChunk, Outcome::Data(data)) if live => {
                let Stream::Backup {
                    name,
                    size,
                    next: offset,
                } = self.stream.clone()
                else {
                    return;
                };
                let want = (size - offset).min(max_data as u64);
                if data.len() as u64 != want {
                    self.stream = Stream::None;
                    do_(out, Effect::Note(Note::BadOutcome));
                    return self.refuse(None, ErrorCode::Internal, out);
                }
                let end = offset + want;
                self.stream = Stream::Backup {
                    name: name.clone(),
                    size,
                    next: end,
                };
                if let Ok(bytes) = frame::encode(Kind::Data, self.tx_next, offset, &data, max_data)
                {
                    self.tx_next = self.tx_next.wrapping_add(1);
                    out.push(PhoneOutput::Send(bytes));
                }
                if end == size {
                    self.stream = Stream::None;
                    if let Some(p) = self
                        .backup
                        .iter_mut()
                        .flatten()
                        .find(|p| p.found.name == name)
                    {
                        p.read_done = true;
                    }
                    self.send(None, PhoneMsg::DataEnd { name }, out);
                } else {
                    let ticket = self.begin(now, op.re, OpKind::BackupChunk);
                    let len = (size - end).min(max_data as u64) as u32;
                    do_(
                        out,
                        Effect::BackupRead {
                            ticket,
                            name,
                            offset: end,
                            len,
                        },
                    );
                }
            }
            (OpKind::BackupVerify { name, want }, Outcome::Digest(got)) => {
                let same = ct_eq(&want, &got);
                if let Some(p) = self
                    .backup
                    .iter_mut()
                    .flatten()
                    .find(|p| p.found.name == name)
                {
                    p.digest = same.then_some(got);
                    p.read_done = same;
                }
                match (live, same) {
                    (true, true) => self.send(re, PhoneMsg::BackupVerified, out),
                    (true, false) => self.refuse(re, ErrorCode::BackupMismatch, out),
                    _ => {}
                }
            }
            (OpKind::CommitList, Outcome::Parts(found)) if live => {
                let parts = self.backup.take().unwrap_or_default();
                // The names that are not as the first run found them
                let mut differ: Vec<String> = Vec::new();
                let changed = parts
                    .iter()
                    .filter(|p| !found.contains(&p.found))
                    .map(|p| &p.found.name)
                    .chain(
                        found
                            .iter()
                            .filter(|f| !parts.iter().any(|p| p.found == **f))
                            .map(|f| &f.name),
                    );
                for name in changed {
                    if is_part_name(name) && !differ.contains(name) && differ.len() < MAX_PARTS {
                        differ.push(name.clone());
                    }
                }
                if !differ.is_empty() || found.len() != parts.len() {
                    return self.refuse(re, ErrorCode::BackupPrecondition { parts: differ }, out);
                }
                let mut sums = String::new();
                for p in parts.iter().filter(|p| p.found.checked) {
                    if let Some(d) = &p.digest {
                        sums.push_str(&format!("{}  {}.img\n", to_hex(d), p.found.name));
                    }
                }
                self.backup = Some(parts);
                let ticket = self.begin(now, op.re, OpKind::CommitKeep);
                do_(out, Effect::BackupKeep { ticket, sums });
            }
            (OpKind::CommitKeep, Outcome::Done) => {
                self.p.backup_done = true;
                do_(out, Effect::Persist(self.p.clone()));
                if live {
                    self.send(re, PhoneMsg::BackupDone, out);
                }
            }
            (OpKind::FactoryOpen(b, resume_at), Outcome::FactoryOpened { have }) if live => {
                if have > resume_at {
                    do_(out, Effect::Note(Note::BadOutcome));
                    return self.refuse(re, ErrorCode::Internal, out);
                }
                self.p.factory = Some(FactoryFile {
                    size: b.size,
                    sha256: b.sha256,
                    synced: have,
                    verified: false,
                });
                do_(out, Effect::Persist(self.p.clone()));
                self.stream = Stream::Factory {
                    next: have,
                    acked: have,
                    last: now,
                };
                let ready = PhoneMsg::FactoryReady {
                    offset: have,
                    ack_every: self.cfg.ack_every,
                    window: self.cfg.window,
                };
                self.send(re, ready, out);
            }
            (OpKind::FactoryOpen(..), Outcome::NoSpace { need, free }) if live => {
                do_(out, Effect::Screen(Screen::NoSpace));
                let (need, free) = (need.min(MAX_NUMBER), free.min(MAX_NUMBER));
                self.refuse(re, ErrorCode::NoSpace { need, free }, out);
            }
            (OpKind::FactoryVerify, Outcome::Digest(got)) => {
                let same = self
                    .p
                    .factory
                    .as_ref()
                    .is_some_and(|f| ct_eq(&f.sha256, &got));
                if same {
                    if let Some(f) = &mut self.p.factory {
                        f.verified = true;
                    }
                    do_(out, Effect::Persist(self.p.clone()));
                    if live {
                        self.send(re, PhoneMsg::FactoryVerified, out);
                    }
                } else {
                    // Nothing is counted (§2.5): a damaged download is not
                    // a guess at the code
                    self.p.factory = None;
                    do_(out, Effect::FactoryDelete);
                    do_(out, Effect::Persist(self.p.clone()));
                    do_(out, Effect::Screen(Screen::Damaged));
                    if live {
                        self.refuse(re, ErrorCode::FactoryMismatch, out);
                    }
                }
            }
            (OpKind::ProvisionFacts, Outcome::Facts(f)) if live => {
                if !f.charging && f.battery < MIN_BATTERY {
                    return self.refuse(re, ErrorCode::Battery, out);
                }
                self.p.provision = JobState::Running;
                self.p.provision_failed.clear();
                do_(out, Effect::Persist(self.p.clone()));
                do_(out, Effect::StartProvision);
                self.send(re, PhoneMsg::ProvisionStarted, out);
            }
            (OpKind::Smoke, Outcome::Report(report)) if live => {
                self.send(re, PhoneMsg::SmokeReport { report }, out);
            }
            (kind, outcome) => {
                // The shell could not do it, the answer is not one the step
                // has, or nobody waits for it any more.
                if kind == OpKind::BackupChunk {
                    self.stream = Stream::None;
                }
                if outcome != Outcome::Failed && live {
                    do_(out, Effect::Note(Note::BadOutcome));
                }
                if live {
                    let re = if kind == OpKind::BackupChunk {
                        None
                    } else {
                        re
                    };
                    self.refuse(re, ErrorCode::Internal, out);
                }
            }
        }
    }
}

fn do_(out: &mut Out, e: Effect) {
    out.push(PhoneOutput::Do(e));
}

/// A shell's text made fit for a message: printable ASCII, cut to length.
fn clean_to(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| (' '..='~').contains(c))
        .take(max)
        .collect()
}
fn clean(s: &str) -> String {
    clean_to(s, MAX_TEXT)
}

fn listing_ok(found: &[PartFound]) -> bool {
    !found.is_empty()
        && found.len() <= MAX_PARTS
        && found
            .iter()
            .all(|p| is_part_name(&p.name) && p.size <= MAX_NUMBER)
        && found
            .iter()
            .enumerate()
            .all(|(i, p)| !found[..i].iter().any(|q| q.name == p.name))
}
