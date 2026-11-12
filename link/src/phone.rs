// SPDX-License-Identifier: MIT
//! The phone's side of the link as a state machine with no I/O.
//!
//! [`Phone::handle`] takes the time and one thing that happened (a frame,
//! the link coming or going, a deadline, the outcome of something the shell
//! did) and returns, in order, what the shell must do next: frames to write
//! to the host and [`Effect`]s to carry out. The shell works through that
//! list in order and stops at the first step it cannot do, which it reports
//! as [`PhoneInput::Fault`]: a reply is never sent before the state it
//! announces is on disk.
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
/// The first lock after five wrong tries; it doubles (§2.5).
pub const LOCK_FIRST_MS: u64 = 30_000;
/// The longest timed lock (§2.5).
pub const LOCK_MAX_MS: u64 = 600_000;
/// Wrong tries before the code is replaced and pairing locked (§2.5).
pub const TRIES_PER_CODE: u32 = 5;
/// Wrong tries in one boot before pairing is locked until a restart (§2.5).
pub const TRIES_PER_BOOT: u32 = 20;
/// Once provisioned, the link is offered this long after the session is up (§2.2).
pub const LATE_WINDOW_MS: u64 = 600_000;
/// ... in at most this many boots (§2.2).
pub const LATE_BOOTS: u8 = 3;
/// An acknowledgement after each 64 MiB written and synced (§5.6).
pub const ACK_EVERY: u64 = 64 << 20;
/// `provision` needs the charger or this much battery (§5.6).
pub const MIN_BATTERY: u8 = 30;
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
    /// Can the code be drawn on the screen now? Not once the desktop
    /// session owns it.
    pub code_screen: bool,
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
            code_screen: true,
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
/// file. The machine hands a new copy to the shell ([`Effect::Persist`])
/// before the reply that announces a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persistent {
    pub stage: Stage,
    /// SHA-256 of the session token. The token itself is never kept.
    pub token_hash: Option<[u8; 32]>,
    /// 1 before the planned restart, 2 after.
    pub boot: u8,
    /// Boots begun at stage `provisioned`.
    pub late_boots: u8,
    /// `backup-commit` succeeded: the pending checksum file exists.
    pub backup_done: bool,
    pub factory: Option<FactoryFile>,
    pub provision: JobState,
    /// The step provisioning stopped at.
    pub provision_failed: String,
    /// The first-boot steps that failed.
    pub firstboot_failed: Vec<String>,
}

impl Default for Persistent {
    fn default() -> Self {
        Persistent {
            stage: Stage::Waiting,
            token_hash: None,
            boot: 1,
            late_boots: 0,
            backup_done: false,
            factory: None,
            provision: JobState::None,
            provision_failed: String::new(),
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
            .num("late_boots", self.late_boots as u64)
            .bool("backup_done", self.backup_done);
        if let Some(f) = &self.factory {
            let mut o = ObjWriter::new();
            o.num("size", f.size)
                .str("sha256", &to_hex(&f.sha256))
                .num("synced", f.synced)
                .bool("verified", f.verified);
            w.raw("factory", &o.end());
        }
        w.str("provision", self.provision.as_str())
            .str("provision_failed", &self.provision_failed)
            .raw(
                "firstboot_failed",
                &json::array(self.firstboot_failed.iter().map(|s| json::quoted(s))),
            );
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
        let late_boots = rd.num("late_boots", 255)? as u8;
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
            late_boots,
            backup_done,
            factory,
            provision: rd.word("provision", JobState::from_wire)?,
            provision_failed: rd.str("provision_failed", MAX_TEXT, is_text)?,
            firstboot_failed: rd.strings("firstboot_failed", MAX_STEPS, MAX_TEXT, is_text1)?,
        };
        rd.end()?;
        Ok(p)
    }
}

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

/// What the screen program should show. The words are its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// "Waiting for the installer on your computer", with the code.
    Code(Code),
    /// Wrong code, tries left before the code is replaced.
    WrongCode {
        left: u32,
    },
    /// Pairing is locked; 0 is until a restart.
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
}

/// For the journal: the kind of thing, never content (§9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// A frame with no session open and no `hello` in it was dropped.
    Dropped,
    Frame(FrameError),
    Message(MsgError),
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
/// one that cannot is a [`PhoneInput::Fault`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Screen(Screen),
    /// Write the state file: temporary file, sync, rename.
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
    /// `retry` false: move the checked candidate to
    /// `/etc/omamobile/firstboot.conf`, write `boot-update.conf` from the
    /// descriptor (the plan's Q3) and start the unit. True: start the unit
    /// again with the kept file. Answer: [`Outcome::Done`].
    StartFirstboot {
        ticket: Ticket,
        retry: bool,
    },
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
    /// Read the whole file back. Answer: [`Outcome::Digest`].
    FactoryDigest {
        ticket: Ticket,
    },
    /// Delete the image's file.
    FactoryDelete,
    /// Start the provisioning unit; report with
    /// [`PhoneInput::Progress`] and [`PhoneInput::ProvisionEnded`].
    StartProvision,
    /// Wait for the session and run the smoke test. Answer: [`Outcome::Report`].
    Smoke {
        ticket: Ticket,
    },
    /// `systemctl reboot`, after the frames before it are written.
    Restart,
    /// Delete the marker, the token and the state, write `done`, put the
    /// gadget back.
    Finish,
    /// The machine cannot go on (a fault): stop the daemon, and let it start
    /// again from the state file.
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
    /// FunctionFS `DISABLE` or `UNBIND`: the cable, a host reset.
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
    /// Provisioning ended. `failed` is the step it stopped at; a camera step
    /// that failed alone is in `steps`, not here (§6.3).
    ProvisionEnded {
        steps: Vec<StepResult>,
        failed: Option<String>,
    },
    /// The desktop session is up: the ten minutes of §2.2 begin.
    DesktopUp,
    /// Whether the code can be drawn on the screen changed.
    CodeScreen(bool),
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
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    epoch: u64,
    nonce: [u8; 32],
    max_data: u32,
    paired: bool,
    rx_next: u32,
    last_rx: u64,
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
    wrong: u32,
    lock: Lock,
    stream: Stream,
    backup: Option<Vec<Part>>,
    op: Option<Op>,
    status_waiting: Vec<(Ticket, u64, u64)>,
    late_deadline: Option<u64>,
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
    /// The machine at the daemon's start, from the state file (or
    /// `Persistent::default()` on a first start). `new_boot` is false when
    /// the daemon starts again within one boot.
    pub fn start(
        cfg: PhoneConfig,
        state: Persistent,
        new_boot: bool,
        now: u64,
        rng: &mut dyn Entropy,
    ) -> (Phone, Vec<PhoneOutput>) {
        let mut cfg = cfg;
        cfg.max_data = cfg.max_data.clamp(MIN_MAX_DATA, MAX_DATA_LIMIT);
        cfg.ack_every = cfg.ack_every.max(1);
        let mut me = Phone {
            cfg,
            p: state.clone(),
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
            late_deadline: None,
            stopped: false,
        };
        let mut out = Vec::new();
        if me.p.stage == Stage::Finished {
            me.stopped = true;
            out.push(PhoneOutput::Do(Effect::Finish));
            return (me, out);
        }
        if new_boot && me.p.stage == Stage::Provisioned {
            me.p.boot = 2;
            me.p.late_boots = me.p.late_boots.saturating_add(1);
            if me.p.late_boots > LATE_BOOTS {
                me.finish(None, &mut out);
                return (me, out);
            }
        }
        if me.p != state {
            out.push(PhoneOutput::Do(Effect::Persist(me.p.clone())));
        }
        if me.cfg.code_screen {
            let code = draw_code(rng);
            me.code = Some(code);
            out.push(PhoneOutput::Do(Effect::Screen(Screen::Code(code))));
        }
        let _ = now;
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
    /// The code on the screen. For the screen and for tests; not for a log.
    pub fn code(&self) -> Option<Code> {
        self.code
    }
    /// Wrong tries counted this boot.
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
    /// The machine has ended (`finish`, the limits, a fault): feed it nothing more.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// When to send [`PhoneInput::Tick`] at the latest, if nothing else
    /// happens first.
    pub fn deadline(&self) -> Option<u64> {
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
        if let Some(t) = self.late_deadline {
            add(t);
        }
        d
    }

    /// One step: what happened at `now` (milliseconds on any clock that does
    /// not go back), and what to do about it.
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
                self.end_session();
                self.link_up = true;
                self.tx_next = 0;
            }
            PhoneInput::LinkDown => {
                self.end_session();
                self.link_up = false;
            }
            PhoneInput::Frame(bytes) => self.on_frame(now, bytes, rng, &mut out),
            PhoneInput::Tick => self.on_tick(now, &mut out),
            PhoneInput::Done(ticket, outcome) => self.on_done(now, ticket, outcome, rng, &mut out),
            PhoneInput::FirstbootEnded { failed } => {
                if self.p.stage == Stage::Firstboot {
                    let any = !failed.is_empty();
                    let mut failed: Vec<String> = failed
                        .iter()
                        .take(MAX_STEPS)
                        .map(|s| clean(s))
                        .filter(|s| !s.is_empty())
                        .collect();
                    if any && failed.is_empty() {
                        failed.push("unknown".to_string());
                    }
                    if failed.is_empty() {
                        self.p.stage = Stage::FirstbootDone;
                    } else {
                        self.p.stage = Stage::FirstbootFailed;
                        do_(
                            &mut out,
                            Effect::Screen(Screen::Stopped {
                                step: failed[0].clone(),
                            }),
                        );
                    }
                    self.p.firstboot_failed = failed;
                    do_(&mut out, Effect::Persist(self.p.clone()));
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
            PhoneInput::ProvisionEnded { steps, failed } => {
                if self.p.provision == JobState::Running {
                    match failed {
                        None => {
                            self.p.provision = JobState::Done;
                            self.p.stage = Stage::Provisioned;
                            self.p.factory = None;
                            do_(&mut out, Effect::FactoryDelete);
                            do_(&mut out, Effect::Persist(self.p.clone()));
                            if self.paired() {
                                self.send(None, PhoneMsg::ProvisionDone { steps }, &mut out);
                            }
                        }
                        Some(step) => {
                            let step = clean(&step);
                            self.p.provision = JobState::Failed;
                            self.p.provision_failed = step.clone();
                            do_(&mut out, Effect::Persist(self.p.clone()));
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
            PhoneInput::DesktopUp => {
                if self.p.stage == Stage::Provisioned {
                    self.late_deadline = Some(now + LATE_WINDOW_MS);
                }
            }
            PhoneInput::CodeScreen(on) => {
                self.cfg.code_screen = on;
                if !on {
                    self.code = None;
                } else if self.code.is_none() && self.lock != Lock::Restart {
                    let code = draw_code(rng);
                    self.code = Some(code);
                    do_(&mut out, Effect::Screen(Screen::Code(code)));
                }
            }
            PhoneInput::Fault => {
                if self.session.is_some() {
                    self.refuse(None, ErrorCode::Internal, &mut out);
                }
                self.end_session();
                self.stopped = true;
                do_(&mut out, Effect::Stop);
            }
        }
        out
    }

    // --- Sessions and frames ----------------------------------------------

    fn end_session(&mut self) {
        self.session = None;
        self.status_waiting.clear();
        // A stream stays as it was at its last acknowledged offset: the
        // factory file's synced length is in the state file already.
        if matches!(self.stream, Stream::Backup { .. }) {
            self.op = None;
        }
        self.stream = Stream::None;
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

    /// §5.1: a wrong frame gets `protocol`, ends the session and counts a
    /// wrong try.
    fn protocol_error(&mut self, now: u64, note: Note, rng: &mut dyn Entropy, out: &mut Out) {
        do_(out, Effect::Note(note));
        self.refuse(None, ErrorCode::Protocol, out);
        self.end_session();
        self.wrong_try(now, rng, out);
    }

    /// Counts a wrong try. `Some(wait_s)` when pairing is locked by it.
    fn wrong_try(&mut self, now: u64, rng: &mut dyn Entropy, out: &mut Out) -> Option<u32> {
        self.wrong = self.wrong.saturating_add(1);
        if self.wrong >= TRIES_PER_BOOT {
            self.lock = Lock::Restart;
            self.code = None;
            do_(out, Effect::Screen(Screen::Locked { wait_s: 0 }));
            return Some(0);
        }
        if self.wrong.is_multiple_of(TRIES_PER_CODE) {
            let doubled =
                LOCK_FIRST_MS.saturating_mul(1 << (self.wrong / TRIES_PER_CODE - 1).min(20));
            let wait = doubled.min(LOCK_MAX_MS);
            self.lock = Lock::Until(now + wait);
            if self.cfg.code_screen {
                self.code = Some(draw_code(rng));
            }
            let wait_s = (wait / 1000) as u32;
            do_(out, Effect::Screen(Screen::Locked { wait_s }));
            return Some(wait_s);
        }
        None
    }

    /// The seconds pairing is locked for: `Some(0)` until a restart.
    fn locked(&self, now: u64) -> Option<u32> {
        match self.lock {
            Lock::Open => None,
            Lock::Restart => Some(0),
            Lock::Until(t) if t > now => Some((t - now).div_ceil(1000).max(1) as u32),
            Lock::Until(_) => None,
        }
    }

    fn pair_mode(&self, now: u64) -> (PairMode, u32) {
        match self.locked(now) {
            Some(wait_s) => (PairMode::Locked, wait_s),
            None if self.code.is_some() => (PairMode::Code, 0),
            None if self.p.token_hash.is_some() => (PairMode::Token, 0),
            // No code can be shown and no token exists: nothing can pair
            // until a restart.
            None => (PairMode::Locked, 0),
        }
    }

    fn on_frame(&mut self, now: u64, bytes: &[u8], rng: &mut dyn Entropy, out: &mut Out) {
        if !self.link_up {
            do_(out, Effect::Note(Note::Dropped));
            return;
        }
        let decoded = frame::decode(bytes, self.max_data());
        // A `hello` is always frame 0 and starts the numbering again, in
        // both directions (LINK.md D2).
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
            Err(e) => return self.protocol_error(now, Note::Frame(e), rng, out),
        };
        if f.seq != s.rx_next {
            let e = FrameError::Seq {
                expected: s.rx_next,
                got: f.seq,
            };
            return self.protocol_error(now, Note::Frame(e), rng, out);
        }
        s.rx_next = s.rx_next.wrapping_add(1);
        match f.kind {
            Kind::Data => self.on_data(now, f.offset, f.payload, rng, out),
            Kind::Message => match HostFrame::parse(f.payload) {
                Ok(HostFrame {
                    msg: HostMsg::Hello(_),
                    ..
                }) => {
                    let e = FrameError::Seq {
                        expected: 0,
                        got: f.seq,
                    };
                    self.protocol_error(now, Note::Frame(e), rng, out)
                }
                Ok(m) => self.on_message(now, m, rng, out),
                Err(MsgError::UnknownType { id }) => self.refuse(id, ErrorCode::Unknown, out),
                Err(e) => self.protocol_error(now, Note::Message(e), rng, out),
            },
        }
    }

    fn on_hello(&mut self, now: u64, id: u64, h: HostHello, rng: &mut dyn Entropy, out: &mut Out) {
        do_(out, Effect::Note(Note::Received("hello")));
        self.end_session();
        self.tx_next = 0;
        let common = h.proto.iter().filter(|n| self.cfg.protos.contains(n)).max();
        let Some(&proto) = common else {
            do_(out, Effect::Screen(Screen::Version));
            let proto = self.cfg.protos.clone();
            self.refuse(Some(id), ErrorCode::Version { proto }, out);
            return;
        };
        let mut nonce = [0u8; 32];
        rng.fill(&mut nonce);
        self.epochs += 1;
        let max_data = h.max_data.min(self.cfg.max_data);
        self.session = Some(Session {
            epoch: self.epochs,
            nonce,
            max_data,
            paired: false,
            rx_next: 1,
            last_rx: now,
        });
        let (pair, wait_s) = self.pair_mode(now);
        let hello = PhoneHello {
            proto,
            link: self.cfg.link.clone(),
            device: self.cfg.device.clone(),
            max_data,
            nonce,
            stage: self.p.stage,
            pair,
            wait_s,
        };
        self.send(Some(id), PhoneMsg::Hello(hello), out);
    }

    fn on_data(
        &mut self,
        now: u64,
        offset: u64,
        payload: &[u8],
        rng: &mut dyn Entropy,
        out: &mut Out,
    ) {
        let size = self.p.factory.as_ref().map_or(0, |f| f.size);
        let Stream::Factory { next, acked, last } = &mut self.stream else {
            return self.protocol_error(now, Note::Frame(FrameError::NoStream), rng, out);
        };
        if offset != *next {
            let e = FrameError::StreamOffset {
                expected: *next,
                got: offset,
            };
            return self.protocol_error(now, Note::Frame(e), rng, out);
        }
        let end = offset + payload.len() as u64; // decode refused an overflow
        if end > size {
            return self.protocol_error(
                now,
                Note::Frame(FrameError::StreamOverrun { size, end }),
                rng,
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

    fn on_message(&mut self, now: u64, m: HostFrame, rng: &mut dyn Entropy, out: &mut Out) {
        let re = Some(m.id);
        do_(out, Effect::Note(Note::Received(m.msg.t())));
        let Some(s) = &self.session else { return };
        let (paired, nonce, max_data) = (s.paired, s.nonce, s.max_data);
        let stage = self.p.stage;
        match m.msg {
            HostMsg::Hello(_) => {}
            HostMsg::Ping => self.send(re, PhoneMsg::Pong, out),
            HostMsg::Bye => {
                self.send(re, PhoneMsg::Bye, out);
                self.end_session();
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
                    return match self.wrong_try(now, rng, out) {
                        Some(wait_s) => self.refuse(re, ErrorCode::Locked { wait_s }, out),
                        None => {
                            let left = TRIES_PER_CODE - self.wrong % TRIES_PER_CODE;
                            do_(out, Effect::Screen(Screen::WrongCode { left }));
                            self.refuse(re, ErrorCode::BadCode { left }, out)
                        }
                    };
                }
                let mut token = [0u8; 32];
                rng.fill(&mut token);
                self.p.token_hash = Some(sha256(&token));
                do_(out, Effect::Persist(self.p.clone()));
                do_(out, Effect::Screen(Screen::Paired));
                if let Some(s) = &mut self.session {
                    s.paired = true;
                }
                let serial_proof = serial_proof(&nonce, &self.cfg.serial);
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
                if paired {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
                if let Some(wait_s) = self.locked(now) {
                    return self.refuse(re, ErrorCode::Locked { wait_s }, out);
                }
                let Some(kept) = self.p.token_hash else {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                };
                if !ct_eq(&kept, &sha256(&token.0)) {
                    return match self.wrong_try(now, rng, out) {
                        Some(wait_s) => self.refuse(re, ErrorCode::Locked { wait_s }, out),
                        None => self.refuse(re, ErrorCode::BadToken, out),
                    };
                }
                if let Some(s) = &mut self.session {
                    s.paired = true;
                }
                let serial_proof = serial_proof(&nonce, &self.cfg.serial);
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
                if stage != Stage::Provisioned {
                    return self.refuse(re, ErrorCode::NotAllowed, out);
                }
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
                self.finish(re, out);
            }
        }
    }

    /// The end: by `finish`, or by itself when §2.2's limits ran out.
    fn finish(&mut self, re: Option<u64>, out: &mut Out) {
        self.p.stage = Stage::Finished;
        self.p.token_hash = None;
        self.p.factory = None;
        do_(out, Effect::Persist(self.p.clone()));
        if re.is_some() {
            self.send(re, PhoneMsg::Finished, out);
        }
        self.end_session();
        self.op = None;
        self.code = None;
        self.stopped = true;
        do_(out, Effect::Finish);
    }

    fn on_tick(&mut self, now: u64, out: &mut Out) {
        if matches!(self.lock, Lock::Until(t) if t <= now) {
            self.lock = Lock::Open;
            if let Some(code) = self.code {
                do_(out, Effect::Screen(Screen::Code(code)));
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| now >= s.last_rx + SESSION_IDLE_MS)
        {
            do_(out, Effect::Note(Note::SessionIdle));
            self.end_session();
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
        if self.late_deadline.is_some_and(|t| now >= t) && self.op.is_none() {
            self.finish(None, out);
        }
    }

    // --- Answers from the shell ----------------------------------------------

    fn status(&self, f: Facts) -> Status {
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
            battery: f.battery.min(100),
            charging: f.charging,
            free: f.free.min(MAX_NUMBER),
        }
    }

    fn on_done(
        &mut self,
        now: u64,
        ticket: Ticket,
        outcome: Outcome,
        rng: &mut dyn Entropy,
        out: &mut Out,
    ) {
        if let Some(i) = self.status_waiting.iter().position(|w| w.0 == ticket) {
            let (_, _, re) = self.status_waiting.remove(i);
            return match outcome {
                Outcome::Facts(f) => {
                    let s = self.status(f);
                    self.send(Some(re), PhoneMsg::Status(Box::new(s)), out)
                }
                _ => self.refuse(Some(re), ErrorCode::Internal, out),
            };
        }
        let Some(op) = self.op.take_if(|op| op.ticket == ticket) else {
            do_(out, Effect::Note(Note::StaleTicket));
            return;
        };
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
                self.send(re, PhoneMsg::FactoryReady { offset: have }, out);
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
                    self.p.factory = None;
                    do_(out, Effect::FactoryDelete);
                    do_(out, Effect::Persist(self.p.clone()));
                    do_(out, Effect::Screen(Screen::Damaged));
                    if live {
                        self.refuse(re, ErrorCode::FactoryMismatch, out);
                    }
                    self.wrong_try(now, rng, out);
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
