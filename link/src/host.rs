// SPDX-License-Identifier: MIT
//! The computer's side of the link as a state machine with no I/O.
//!
//! [`Host::handle`] takes the time and one thing that happened and returns,
//! in order, frames to write to the phone, things for the installer's shell
//! to do ([`HostEffect`]) and what the install flow should know ([`Event`]).
//! The flow asks for one step at a time with a [`Command`]; the machine does
//! the step's messages, and after a lost link it says `hello` again, pairs
//! again with the token and takes the step up where the phone says it is.
//!
//! There is no way to send bytes of the caller's choosing: a command is one
//! of a closed list, and each becomes typed messages (INSTALL-LINK-PLAN
//! §7.1, "the closed list").
use crate::answers::ValidAnswers;
use crate::digest::{ct_eq, serial_proof};
use crate::frame::{self, FrameError, Kind, DEFAULT_MAX_DATA, MAX_DATA_LIMIT, MIN_MAX_DATA};
use crate::json::Value;
use crate::msg::*;
use crate::PROTOCOL;

/// Names the answer to an effect.
pub type Ticket = u64;

/// Silence during a step for this long is a lost link (§5.3).
pub const SILENCE_MS: u64 = 60_000;
/// A partition whose copy differs is read again this many times in all (§8).
pub const BACKUP_TRIES: u32 = 5;
/// `status` is asked again this often while the phone works by itself.
pub const POLL_MS: u64 = 2_000;
/// A `busy` answer is asked again after this long, this many times.
pub const BUSY_WAIT_MS: u64 = 1_000;
pub const BUSY_TRIES: u32 = 120;
/// Sessions that ended in a protocol error, one after the other, before the
/// machine stops trying.
pub const STRIKES: u32 = 3;

/// What the installer knows before any frame.
#[derive(Clone, PartialEq, Eq)]
pub struct HostConfig {
    pub installer_name: String,
    pub installer_version: String,
    pub protos: Vec<u32>,
    pub max_data: u32,
    /// The serial the install pinned in fastboot. It never crosses the link.
    pub serial: Vec<u8>,
    /// The token a run before this one recorded.
    pub token: Option<Token>,
    /// The number of this run's first message. A run should not begin where
    /// the one before it did: an answer to that run's `hello`, still under
    /// way on the bus, would be taken for the answer to this one's. The time
    /// in seconds, or a random number, will do (LINK.md D3).
    pub first_id: u64,
}

impl HostConfig {
    pub fn new(installer_name: &str, installer_version: &str, serial: &[u8]) -> HostConfig {
        HostConfig {
            installer_name: installer_name.to_string(),
            installer_version: installer_version.to_string(),
            protos: vec![PROTOCOL],
            max_data: DEFAULT_MAX_DATA,
            serial: serial.to_vec(),
            token: None,
            first_id: 1,
        }
    }
}

impl core::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "HostConfig {{ {} {}, serial and token withheld }}",
            self.installer_name, self.installer_version
        )
    }
}

/// One step of the install, asked for by the flow. One at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The code the owner typed. Only after [`Event::NeedCode`].
    Pair(Code),
    Status,
    /// Send the answers, unless the phone has them already.
    Answers(ValidAnswers),
    FirstbootRetry,
    /// Wait until first-boot setup ends.
    AwaitFirstboot,
    /// The radio backup: list, read, verify, commit.
    Backup,
    /// Send the factory image and have it checked.
    Factory(FactoryBegin),
    /// Start provisioning, or join one that runs, and wait for its end.
    Provision,
    /// Restart the phone and wait for it to come back.
    Restart,
    Smoke,
    Finish,
    /// End the open stream and the step that owns it.
    Abort,
    Bye,
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Pair(_) => "pair",
            Command::Status => "status",
            Command::Answers(_) => "answers",
            Command::FirstbootRetry => "firstboot-retry",
            Command::AwaitFirstboot => "await-firstboot",
            Command::Backup => "backup",
            Command::Factory(_) => "factory",
            Command::Provision => "provision",
            Command::Restart => "restart",
            Command::Smoke => "smoke",
            Command::Finish => "finish",
            Command::Abort => "abort",
            Command::Bye => "bye",
        }
    }
}

/// Something for the installer's shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEffect {
    /// Keep the token (memory and the work directory, mode 600; never a report).
    RecordToken(Token),
    ForgetToken,
    /// How many bytes of `NAME.img` are kept from an earlier try? Answer:
    /// [`HostOutcome::Have`] (0: none).
    BackupOpen {
        ticket: Ticket,
        name: String,
        size: u64,
    },
    /// Write at `offset` of `NAME.img`, and cut the file there first.
    BackupWrite {
        name: String,
        offset: u64,
        data: Vec<u8>,
    },
    /// SHA-256 of the whole `NAME.img`, read back from disk. Answer:
    /// [`HostOutcome::Digest`].
    BackupDigest {
        ticket: Ticket,
        name: String,
    },
    /// Delete `NAME.img`: it is not the partition.
    BackupDiscard {
        name: String,
    },
    /// Every partition is verified and the phone has committed: write
    /// `SHA256SUMS`, `partitions.txt` and `NV-BACKUP` as `tools/nv-backup` does.
    BackupFinish {
        parts: Vec<(BackupPart, [u8; 32])>,
    },
    /// `len` bytes of the factory image at `offset`. Answer: [`HostOutcome::Data`].
    FactoryRead {
        ticket: Ticket,
        offset: u64,
        len: u32,
    },
}

/// The answer to an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOutcome {
    Failed,
    Have(u64),
    Digest([u8; 32]),
    Data(Vec<u8>),
}

/// Why the machine stopped for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The serial proof differs: not the phone that was installed.
    WrongPhone,
    /// No protocol number in common; the phone's list.
    Version { phone: Vec<u32> },
    /// The phone can show no code and this run holds no token it takes.
    CannotPair,
    /// The phone keeps answering outside protocol 1.
    Protocol,
}

/// What the install flow should know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The phone answered `hello`.
    Hello {
        stage: Stage,
        device: String,
        link: String,
    },
    /// Ask the owner for the code on the phone's screen.
    NeedCode,
    WrongCode {
        left: u32,
    },
    /// Pairing is locked; 0 is until the phone restarts.
    Locked {
        wait_s: u32,
    },
    Paired {
        stage: Stage,
    },
    Status(Box<Status>),
    Progress(Progress),
    /// The phone has the answers and first-boot setup was started.
    AnswersAccepted,
    FirstbootDone,
    FirstbootFailed {
        steps: Vec<String>,
    },
    BackupProgress {
        name: String,
        done: u64,
        total: u64,
    },
    BackupDone,
    FactorySent {
        sent: u64,
        size: u64,
    },
    /// The phone has this much on disk.
    FactoryAcked {
        offset: u64,
    },
    FactoryVerified,
    /// Empty `steps`: learned from `status`, the list was missed.
    ProvisionDone {
        steps: Vec<StepResult>,
    },
    Restarting,
    Restarted,
    SmokeReport(Value),
    Finished,
    Aborted,
    Closed,
    /// The phone said no and the step is over. The flow chooses the words
    /// by the code.
    Refused {
        step: &'static str,
        error: ErrorMsg,
    },
    /// The shell could not do its part and the step is over.
    Failed {
        step: &'static str,
    },
    /// A command that cannot be taken now (another step runs, or not paired).
    NotNow {
        step: &'static str,
    },
    /// The session ended without `bye`; the machine starts again from
    /// `hello` when the link is there.
    LinkLost,
    Stopped(Stop),
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostInput<'a> {
    /// The link's interface was found and claimed.
    LinkUp,
    /// The device left the bus.
    LinkDown,
    /// One completed read from the phone.
    Frame(&'a [u8]),
    /// The time passed [`Host::deadline`].
    Tick,
    Command(Command),
    Done(Ticket, HostOutcome),
    /// An effect without a ticket could not be carried out.
    Fault,
}

/// What the shell does next, in this order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOutput {
    Send(Vec<u8>),
    Do(HostEffect),
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Conn {
    Down,
    Hello {
        id: u64,
    },
    NeedCode,
    Pairing {
        id: u64,
        by_code: bool,
    },
    /// Locked, or closed with `bye`: nothing goes out until something changes.
    Idle,
    Ready,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Start,
    /// `status` first, to see where the phone is.
    Check,
    Sent,
    Wait,
    BList,
    BOpen(Ticket),
    BBegin {
        offset: u64,
    },
    BData {
        next: u64,
    },
    BHash(Ticket),
    BVerify {
        digest: [u8; 32],
    },
    BCommit,
    FBegin,
    FRead {
        ticket: Ticket,
        offset: u64,
    },
    FEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    cmd: Command,
    phase: Phase,
    /// The message whose answer is awaited.
    id: Option<u64>,
    busy: u32,
    wake: Option<u64>,
    parts: Vec<BackupPart>,
    sums: Vec<[u8; 32]>,
    tries: u32,
}

/// The computer's side of the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    cfg: HostConfig,
    link_up: bool,
    conn: Conn,
    tx_next: u32,
    rx_next: u32,
    next_id: u64,
    max_data: u32,
    nonce: [u8; 32],
    pair: PairMode,
    stage: Stage,
    token: Option<Token>,
    step: Option<Step>,
    quiet_since: u64,
    rehello_at: Option<u64>,
    strikes: u32,
    tickets: Ticket,
}

type Out = Vec<HostOutput>;

fn ev(out: &mut Out, e: Event) {
    out.push(HostOutput::Event(e));
}
fn do_(out: &mut Out, e: HostEffect) {
    out.push(HostOutput::Do(e));
}

impl Host {
    pub fn new(cfg: HostConfig) -> Host {
        let mut cfg = cfg;
        cfg.max_data = cfg.max_data.clamp(MIN_MAX_DATA, MAX_DATA_LIMIT);
        Host {
            token: cfg.token,
            max_data: cfg.max_data,
            next_id: cfg.first_id.clamp(1, 1 << 52),
            cfg,
            link_up: false,
            conn: Conn::Down,
            tx_next: 0,
            rx_next: 0,
            nonce: [0; 32],
            pair: PairMode::Locked,
            stage: Stage::Waiting,
            step: None,
            quiet_since: 0,
            rehello_at: None,
            strikes: 0,
            tickets: 0,
        }
    }

    /// Paired, and free or busy with a step.
    pub fn ready(&self) -> bool {
        self.conn == Conn::Ready
    }
    /// Is a step running or waiting to go on?
    pub fn busy(&self) -> bool {
        self.step.is_some()
    }
    pub fn stopped(&self) -> bool {
        self.conn == Conn::Stopped
    }
    /// The token, for the shell's record. Not for a log.
    pub fn token(&self) -> Option<Token> {
        self.token
    }
    /// The data payload limit of the session; the shell's read buffer is
    /// [`frame::read_buffer_len`] of the configured one.
    pub fn max_data(&self) -> u32 {
        self.max_data
    }

    fn awaiting(&self) -> bool {
        match (&self.conn, &self.step) {
            (Conn::Hello { .. } | Conn::Pairing { .. }, _) => true,
            (Conn::Ready, Some(s)) => !matches!(
                s.phase,
                Phase::Start
                    | Phase::Wait
                    | Phase::BOpen(_)
                    | Phase::BHash(_)
                    | Phase::FRead { .. }
            ),
            _ => false,
        }
    }

    /// When to send [`HostInput::Tick`] at the latest.
    pub fn deadline(&self) -> Option<u64> {
        let mut d: Option<u64> = None;
        let mut add = |t: u64| d = Some(d.map_or(t, |x| x.min(t)));
        if self.awaiting() {
            add(self.quiet_since + SILENCE_MS);
        }
        if let Some(t) = self.rehello_at {
            add(t);
        }
        if let (Conn::Ready, Some(Step { wake: Some(t), .. })) = (&self.conn, &self.step) {
            add(*t);
        }
        d
    }

    /// One step: what happened at `now` (milliseconds), and what to do.
    pub fn handle(&mut self, now: u64, input: HostInput<'_>) -> Vec<HostOutput> {
        let mut out = Vec::new();
        if self.conn == Conn::Stopped {
            return out;
        }
        match input {
            HostInput::LinkUp => {
                self.link_up = true;
                self.hello(now, &mut out);
            }
            HostInput::LinkDown => {
                self.link_up = false;
                if !matches!(self.conn, Conn::Down | Conn::Idle) {
                    ev(&mut out, Event::LinkLost);
                }
                self.conn = Conn::Down;
                self.rehello_at = None;
                self.rewind();
            }
            HostInput::Frame(bytes) => self.on_frame(now, bytes, &mut out),
            HostInput::Tick => self.on_tick(now, &mut out),
            HostInput::Command(cmd) => self.on_command(now, cmd, &mut out),
            HostInput::Done(ticket, outcome) => self.on_done(now, ticket, outcome, &mut out),
            HostInput::Fault => self.fail_step(now, &mut out),
        }
        out
    }

    // --- Sending -------------------------------------------------------------

    fn send(&mut self, now: u64, msg: HostMsg, out: &mut Out) -> Option<u64> {
        let id = self.next_id;
        let json = (HostFrame { id, msg }).to_json().ok()?;
        let bytes = frame::encode(
            Kind::Message,
            self.tx_next,
            0,
            json.as_bytes(),
            self.max_data,
        )
        .ok()?;
        self.next_id += 1;
        self.tx_next = self.tx_next.wrapping_add(1);
        self.quiet_since = now;
        out.push(HostOutput::Send(bytes));
        Some(id)
    }

    /// `hello` is always frame 0 and starts the numbering again (LINK.md D2).
    fn hello(&mut self, now: u64, out: &mut Out) {
        self.rehello_at = None;
        if !self.link_up {
            self.conn = Conn::Down;
            return;
        }
        self.tx_next = 0;
        self.rx_next = 0;
        self.max_data = self.cfg.max_data;
        self.rewind();
        let h = HostHello {
            proto: self.cfg.protos.clone(),
            installer_name: self.cfg.installer_name.clone(),
            installer_version: self.cfg.installer_version.clone(),
            max_data: self.cfg.max_data,
        };
        match self.send(now, HostMsg::Hello(h), out) {
            Some(id) => self.conn = Conn::Hello { id },
            None => self.stop(Stop::Protocol, out),
        }
    }

    /// The step, if any, starts again from its beginning in the next session.
    fn rewind(&mut self) {
        if let Some(s) = &mut self.step {
            // A step that only talks is dropped; one that carries the
            // install forward is taken up again.
            if matches!(
                s.cmd,
                Command::Pair(_) | Command::Status | Command::Abort | Command::Bye
            ) {
                self.step = None;
            } else {
                s.phase = Phase::Start;
                s.id = None;
                s.wake = None;
            }
        }
    }

    fn stop(&mut self, why: Stop, out: &mut Out) {
        self.conn = Conn::Stopped;
        self.step = None;
        ev(out, Event::Stopped(why));
    }

    /// The session is over without `bye`: say so, and start again.
    fn lost(&mut self, now: u64, strike: bool, out: &mut Out) {
        ev(out, Event::LinkLost);
        if strike {
            self.strikes += 1;
            if self.strikes >= STRIKES {
                return self.stop(Stop::Protocol, out);
            }
        }
        self.hello(now, out);
    }

    // --- Frames ----------------------------------------------------------------

    fn on_frame(&mut self, now: u64, bytes: &[u8], out: &mut Out) {
        let decoded = frame::decode(bytes, self.max_data);
        if let Conn::Hello { id } = self.conn {
            // Until the phone's own `hello` (frame 0, answering ours),
            // whatever arrives is from a session that is over.
            let Ok(f) = decoded else { return };
            if f.kind != Kind::Message || f.seq != 0 {
                return;
            }
            let Ok(PhoneFrame { re: Some(re), msg }) = PhoneFrame::parse(f.payload) else {
                return;
            };
            if re != id {
                return;
            }
            self.rx_next = 1;
            self.quiet_since = now;
            return match msg {
                PhoneMsg::Hello(h) => self.on_hello(now, h, out),
                PhoneMsg::Error(ErrorMsg {
                    code: ErrorCode::Version { proto },
                    ..
                }) => self.stop(Stop::Version { phone: proto }, out),
                _ => self.lost(now, true, out),
            };
        }
        if matches!(self.conn, Conn::Down | Conn::Idle) {
            return;
        }
        let f = match decoded {
            Ok(f) if f.seq == self.rx_next => f,
            _ => return self.lost(now, true, out),
        };
        self.rx_next = self.rx_next.wrapping_add(1);
        self.quiet_since = now;
        match f.kind {
            Kind::Data => self.on_data(now, f.offset, f.payload, out),
            Kind::Message => match PhoneFrame::parse(f.payload) {
                Ok(m) => self.on_message(now, m, out),
                Err(_) => self.lost(now, true, out),
            },
        }
    }

    fn on_hello(&mut self, now: u64, h: PhoneHello, out: &mut Out) {
        if !self.cfg.protos.contains(&h.proto) || h.max_data > self.cfg.max_data {
            return self.lost(now, true, out);
        }
        self.max_data = h.max_data;
        self.nonce = h.nonce;
        self.pair = h.pair;
        self.stage = h.stage;
        ev(
            out,
            Event::Hello {
                stage: h.stage,
                device: h.device,
                link: h.link,
            },
        );
        match (h.pair, self.token) {
            (PairMode::Locked, _) => {
                self.conn = Conn::Idle;
                ev(out, Event::Locked { wait_s: h.wait_s });
                if h.wait_s > 0 {
                    self.rehello_at = Some(now + h.wait_s as u64 * 1000 + 500);
                }
            }
            (_, Some(token)) => match self.send(now, HostMsg::Resume { token }, out) {
                Some(id) => self.conn = Conn::Pairing { id, by_code: false },
                None => self.stop(Stop::Protocol, out),
            },
            (PairMode::Code, None) => {
                self.conn = Conn::NeedCode;
                ev(out, Event::NeedCode);
            }
            (PairMode::Token, None) => self.stop(Stop::CannotPair, out),
        }
    }

    fn on_data(&mut self, now: u64, offset: u64, payload: &[u8], out: &mut Out) {
        let Some(s) = &mut self.step else {
            return self.lost(now, true, out);
        };
        if s.cmd == Command::Abort {
            return; // frames that were under way when `abort` left
        }
        let (Phase::BData { next }, Some(part)) = (&mut s.phase, s.parts.get(s.sums.len())) else {
            return self.lost(now, true, out);
        };
        let end = offset + payload.len() as u64;
        if offset != *next || end > part.size {
            return self.lost(now, true, out);
        }
        *next = end;
        let name = part.name.clone();
        let total = part.size;
        do_(
            out,
            HostEffect::BackupWrite {
                name: name.clone(),
                offset,
                data: payload.to_vec(),
            },
        );
        ev(
            out,
            Event::BackupProgress {
                name,
                done: end,
                total,
            },
        );
    }

    fn on_message(&mut self, now: u64, m: PhoneFrame, out: &mut Out) {
        // What comes by itself
        match (&m.re, &m.msg) {
            (None, PhoneMsg::Progress(p)) => return ev(out, Event::Progress(p.clone())),
            (None, PhoneMsg::Error(e)) if e.code == ErrorCode::Protocol => {
                // The phone ended the session over a frame of ours
                return self.lost(now, true, out);
            }
            (None, PhoneMsg::Error(e)) if e.code == ErrorCode::Internal => {
                // The phone's daemon could not go on
                if let Some(s) = self.step.take() {
                    ev(
                        out,
                        Event::Refused {
                            step: s.cmd.name(),
                            error: e.clone(),
                        },
                    );
                }
                return self.lost(now, false, out);
            }
            (None, PhoneMsg::ProvisionDone { .. } | PhoneMsg::Error(_)) => {
                // The end of provisioning comes by itself, whatever the
                // flow is doing. A step that waits for it ends with it; one
                // that is asking `status` learns it from the answer.
                let mine = self
                    .step
                    .as_ref()
                    .map(|s| (s.cmd == Command::Provision, s.phase == Phase::Wait));
                let event = match m.msg {
                    PhoneMsg::ProvisionDone { steps } => Event::ProvisionDone { steps },
                    PhoneMsg::Error(error)
                        if matches!(error.code, ErrorCode::ProvisionFailed { .. }) =>
                    {
                        Event::Refused {
                            step: "provision",
                            error,
                        }
                    }
                    _ => return self.lost(now, true, out),
                };
                return match mine {
                    Some((true, true)) => self.done(out, event),
                    Some((true, false)) => {}
                    _ => ev(out, event),
                };
            }
            _ => {}
        }
        if let Conn::Pairing { id, by_code } = self.conn {
            if m.re != Some(id) {
                return self.lost(now, true, out);
            }
            return self.on_pair_reply(now, by_code, m.msg, out);
        }
        let Some(step) = &mut self.step else {
            return self.lost(now, true, out);
        };
        // An answer must be to the message that waits for one
        if m.re.is_some() && m.re != step.id {
            return self.lost(now, true, out);
        }
        let name = step.cmd.name();
        let aborting = step.cmd == Command::Abort;
        if let (Some(_), PhoneMsg::Error(e)) = (&m.re, &m.msg) {
            match &e.code {
                ErrorCode::Busy if step.busy < BUSY_TRIES => {
                    step.busy += 1;
                    step.phase = Phase::Start;
                    step.id = None;
                    step.wake = Some(now + BUSY_WAIT_MS);
                    return;
                }
                ErrorCode::NotPaired => return self.lost(now, false, out),
                ErrorCode::BackupMismatch if matches!(step.phase, Phase::BVerify { .. }) => {
                    let part = step.parts[step.sums.len()].name.clone();
                    do_(out, HostEffect::BackupDiscard { name: part });
                    step.tries += 1;
                    if step.tries < BACKUP_TRIES {
                        return self.backup_next(now, out);
                    }
                }
                _ => {}
            }
            self.step = None;
            return ev(
                out,
                Event::Refused {
                    step: name,
                    error: e.clone(),
                },
            );
        }
        let phase = step.phase.clone();
        match (phase, m.re, m.msg) {
            (Phase::Check, Some(_), PhoneMsg::Status(s)) => self.on_check(now, *s, out),
            (Phase::Sent, Some(_), msg) => self.on_answer(now, msg, out),
            (Phase::BList, Some(_), PhoneMsg::BackupParts { parts }) if !parts.is_empty() => {
                step.parts = parts;
                step.sums.clear();
                step.tries = 0;
                self.backup_next(now, out)
            }
            (
                Phase::BBegin { offset },
                Some(_),
                PhoneMsg::DataBegin {
                    name: n,
                    size,
                    offset: o,
                },
            ) => {
                let part = &step.parts[step.sums.len()];
                if n != part.name || size != part.size || o != offset {
                    return self.lost(now, true, out);
                }
                step.id = None;
                step.phase = Phase::BData { next: offset };
            }
            (Phase::BData { next }, None, PhoneMsg::DataEnd { name: n }) => {
                let part = &step.parts[step.sums.len()];
                if n != part.name || next != part.size {
                    return self.lost(now, true, out);
                }
                self.tickets += 1;
                step.phase = Phase::BHash(self.tickets);
                do_(
                    out,
                    HostEffect::BackupDigest {
                        ticket: self.tickets,
                        name: n,
                    },
                );
            }
            (Phase::BVerify { digest }, Some(_), PhoneMsg::BackupVerified) => {
                step.sums.push(digest);
                step.tries = 0;
                self.backup_next(now, out)
            }
            (Phase::BCommit, Some(_), PhoneMsg::BackupDone) => {
                let parts = step
                    .parts
                    .iter()
                    .cloned()
                    .zip(step.sums.iter().copied())
                    .collect();
                do_(out, HostEffect::BackupFinish { parts });
                self.done(out, Event::BackupDone)
            }
            (Phase::FBegin, Some(_), PhoneMsg::FactoryReady { offset }) => {
                self.factory_next(now, offset, out)
            }
            (Phase::FRead { .. } | Phase::FEnd, None, PhoneMsg::Ack { offset }) => {
                ev(out, Event::FactoryAcked { offset })
            }
            (Phase::FEnd, Some(_), PhoneMsg::FactoryVerified) => {
                self.done(out, Event::FactoryVerified)
            }
            // Under way when `abort` left
            (_, None, PhoneMsg::DataEnd { .. } | PhoneMsg::Ack { .. }) if aborting => {}
            _ => self.lost(now, true, out),
        }
    }

    fn on_pair_reply(&mut self, now: u64, by_code: bool, msg: PhoneMsg, out: &mut Out) {
        match msg {
            PhoneMsg::Paired {
                token,
                serial_proof: proof,
            } => {
                if token.is_some() != by_code {
                    return self.lost(now, true, out);
                }
                // The phone being set up is the phone that was flashed, or
                // nothing more is said to it.
                if !ct_eq(&proof, &serial_proof(&self.nonce, &self.cfg.serial)) {
                    let _ = self.send(now, HostMsg::Bye, out);
                    return self.stop(Stop::WrongPhone, out);
                }
                if let Some(t) = token {
                    self.token = Some(t);
                    do_(out, HostEffect::RecordToken(t));
                }
                self.conn = Conn::Ready;
                self.strikes = 0;
                ev(out, Event::Paired { stage: self.stage });
                self.run(now, out);
            }
            PhoneMsg::Error(e) => match e.code {
                ErrorCode::BadCode { left } => {
                    self.conn = Conn::NeedCode;
                    ev(out, Event::WrongCode { left });
                    ev(out, Event::NeedCode);
                }
                ErrorCode::Locked { wait_s } => {
                    self.conn = Conn::Idle;
                    ev(out, Event::Locked { wait_s });
                    if wait_s > 0 {
                        self.rehello_at = Some(now + wait_s as u64 * 1000 + 500);
                    }
                }
                ErrorCode::BadToken | ErrorCode::NotAllowed if !by_code => {
                    self.token = None;
                    do_(out, HostEffect::ForgetToken);
                    if self.pair == PairMode::Code {
                        self.conn = Conn::NeedCode;
                        ev(out, Event::NeedCode);
                    } else {
                        self.stop(Stop::CannotPair, out);
                    }
                }
                _ => self.lost(now, true, out),
            },
            _ => self.lost(now, true, out),
        }
    }

    // --- Steps -----------------------------------------------------------------

    fn on_command(&mut self, now: u64, cmd: Command, out: &mut Out) {
        let name = cmd.name();
        match cmd {
            Command::Pair(code) => {
                if self.conn != Conn::NeedCode {
                    return ev(out, Event::NotNow { step: name });
                }
                match self.send(now, HostMsg::Pair { code }, out) {
                    Some(id) => self.conn = Conn::Pairing { id, by_code: true },
                    None => self.stop(Stop::Protocol, out),
                }
            }
            Command::Abort => {
                let streaming = self.conn == Conn::Ready
                    && self.step.as_ref().is_some_and(|s| {
                        matches!(s.cmd, Command::Backup | Command::Factory(_))
                            && s.phase != Phase::Start
                    });
                if !streaming {
                    self.step = None;
                    return ev(out, Event::Aborted);
                }
                self.step = Some(new_step(Command::Abort));
                self.run(now, out);
            }
            cmd => {
                if self.step.is_some() {
                    return ev(out, Event::NotNow { step: name });
                }
                self.step = Some(new_step(cmd));
                if self.conn == Conn::Ready {
                    self.run(now, out);
                } else if self.conn == Conn::Idle && self.link_up {
                    self.hello(now, out);
                }
            }
        }
    }

    /// Begin the step in this session.
    fn run(&mut self, now: u64, out: &mut Out) {
        let Some(step) = &mut self.step else { return };
        if step.phase != Phase::Start || self.conn != Conn::Ready {
            return;
        }
        step.wake = None;
        let (msg, phase) = match &step.cmd {
            Command::Status => (HostMsg::Status, Phase::Sent),
            Command::Answers(_)
            | Command::FirstbootRetry
            | Command::AwaitFirstboot
            | Command::Provision
            | Command::Restart => (HostMsg::Status, Phase::Check),
            Command::Backup => (HostMsg::BackupList, Phase::BList),
            Command::Factory(b) => (HostMsg::FactoryBegin(b.clone()), Phase::FBegin),
            Command::Smoke => (HostMsg::Smoke, Phase::Sent),
            Command::Finish => (HostMsg::Finish, Phase::Sent),
            Command::Abort => (HostMsg::Abort, Phase::Sent),
            Command::Bye => (HostMsg::Bye, Phase::Sent),
            Command::Pair(_) => return,
        };
        step.phase = phase;
        self.ask(now, msg, out);
    }

    /// Send a message of the step and wait for its answer.
    fn ask(&mut self, now: u64, msg: HostMsg, out: &mut Out) {
        let id = self.send(now, msg, out);
        match (&mut self.step, id) {
            (Some(s), Some(id)) => s.id = Some(id),
            // A message this crate would not read back: the shell's values
            (Some(s), None) => {
                let step = s.cmd.name();
                self.step = None;
                ev(out, Event::Failed { step });
            }
            _ => {}
        }
    }

    fn done(&mut self, out: &mut Out, e: Event) {
        self.step = None;
        self.strikes = 0;
        ev(out, e);
    }

    /// `status` came back for a step that looks before it acts.
    fn on_check(&mut self, now: u64, s: Status, out: &mut Out) {
        let Some(step) = &mut self.step else { return };
        match &step.cmd {
            Command::Answers(a) => {
                // §2.6: the answers go again only at `waiting`
                if s.stage == Stage::Waiting {
                    let msg = HostMsg::Answers(a.get().clone());
                    step.phase = Phase::Sent;
                    self.ask(now, msg, out)
                } else {
                    self.done(out, Event::AnswersAccepted)
                }
            }
            Command::FirstbootRetry => {
                if s.stage == Stage::FirstbootFailed {
                    step.phase = Phase::Sent;
                    self.ask(now, HostMsg::FirstbootRetry, out)
                } else {
                    self.done(out, Event::AnswersAccepted)
                }
            }
            Command::AwaitFirstboot => match s.firstboot.state {
                JobState::Done => self.done(out, Event::FirstbootDone),
                JobState::Failed => self.done(
                    out,
                    Event::FirstbootFailed {
                        steps: s.firstboot.failed,
                    },
                ),
                JobState::Running => {
                    step.phase = Phase::Wait;
                    step.id = None;
                    step.wake = Some(now + POLL_MS);
                    ev(out, Event::Status(Box::new(s)))
                }
                JobState::None => {
                    self.step = None;
                    ev(
                        out,
                        Event::NotNow {
                            step: "await-firstboot",
                        },
                    )
                }
            },
            Command::Provision => {
                if !s.stage.before_provisioned() {
                    self.done(out, Event::ProvisionDone { steps: Vec::new() })
                } else if s.provision_state == JobState::Running {
                    step.phase = Phase::Wait;
                    step.id = None;
                    step.wake = Some(now + POLL_MS);
                    ev(out, Event::Status(Box::new(s)))
                } else {
                    step.phase = Phase::Sent;
                    self.ask(now, HostMsg::Provision, out)
                }
            }
            Command::Restart => {
                if s.boot == 2 {
                    self.done(out, Event::Restarted)
                } else {
                    step.phase = Phase::Sent;
                    self.ask(now, HostMsg::Restart, out)
                }
            }
            _ => self.lost(now, true, out),
        }
    }

    /// The answer to a step's one message.
    fn on_answer(&mut self, now: u64, msg: PhoneMsg, out: &mut Out) {
        let Some(step) = &mut self.step else { return };
        match (&step.cmd, msg) {
            (Command::Status, PhoneMsg::Status(s)) => self.done(out, Event::Status(s)),
            (Command::Answers(_) | Command::FirstbootRetry, PhoneMsg::AnswersOk) => {
                self.done(out, Event::AnswersAccepted)
            }
            (Command::Provision, PhoneMsg::ProvisionStarted) => {
                step.phase = Phase::Wait;
                step.id = None;
                step.wake = Some(now + POLL_MS);
            }
            (Command::Restart, PhoneMsg::Restarting) => {
                // The link goes away now; if it does not, look again
                step.phase = Phase::Wait;
                step.id = None;
                step.wake = Some(now + SILENCE_MS);
                ev(out, Event::Restarting)
            }
            (Command::Smoke, PhoneMsg::SmokeReport { report }) => {
                self.done(out, Event::SmokeReport(report))
            }
            (Command::Finish, PhoneMsg::Finished) => {
                self.token = None;
                do_(out, HostEffect::ForgetToken);
                self.conn = Conn::Idle;
                self.done(out, Event::Finished)
            }
            (Command::Abort, PhoneMsg::Aborted) => self.done(out, Event::Aborted),
            (Command::Bye, PhoneMsg::Bye) => {
                self.conn = Conn::Idle;
                self.done(out, Event::Closed)
            }
            _ => self.lost(now, true, out),
        }
    }

    /// The next partition of the backup, or the commit.
    fn backup_next(&mut self, now: u64, out: &mut Out) {
        let Some(step) = &mut self.step else { return };
        match step.parts.get(step.sums.len()) {
            Some(part) => {
                self.tickets += 1;
                step.id = None;
                step.phase = Phase::BOpen(self.tickets);
                do_(
                    out,
                    HostEffect::BackupOpen {
                        ticket: self.tickets,
                        name: part.name.clone(),
                        size: part.size,
                    },
                );
            }
            None => {
                step.phase = Phase::BCommit;
                self.ask(now, HostMsg::BackupCommit, out);
            }
        }
    }

    /// The next piece of the factory image, or its end.
    fn factory_next(&mut self, now: u64, offset: u64, out: &mut Out) {
        let Some(step) = &mut self.step else { return };
        let Command::Factory(b) = &step.cmd else {
            return;
        };
        if offset > b.size {
            return self.lost(now, true, out);
        }
        if offset == b.size {
            step.phase = Phase::FEnd;
            return self.ask(now, HostMsg::FactoryEnd, out);
        }
        self.tickets += 1;
        step.id = None;
        step.phase = Phase::FRead {
            ticket: self.tickets,
            offset,
        };
        let len = (b.size - offset).min(self.max_data as u64) as u32;
        do_(
            out,
            HostEffect::FactoryRead {
                ticket: self.tickets,
                offset,
                len,
            },
        );
    }

    fn on_done(&mut self, now: u64, ticket: Ticket, outcome: HostOutcome, out: &mut Out) {
        let Some(step) = &mut self.step else { return };
        if self.conn != Conn::Ready {
            return;
        }
        match (step.phase.clone(), outcome) {
            (Phase::BOpen(t), HostOutcome::Have(have)) if t == ticket => {
                let part = &step.parts[step.sums.len()];
                let name = part.name.clone();
                // §5.5: the offset is a multiple of max_data. A copy longer
                // than the partition is not a copy of it.
                let offset = if have > part.size {
                    do_(out, HostEffect::BackupDiscard { name: name.clone() });
                    0
                } else {
                    have - have % self.max_data as u64
                };
                step.phase = Phase::BBegin { offset };
                self.ask(now, HostMsg::BackupRead { name, offset }, out);
            }
            (Phase::BHash(t), HostOutcome::Digest(digest)) if t == ticket => {
                let name = step.parts[step.sums.len()].name.clone();
                step.phase = Phase::BVerify { digest };
                self.ask(
                    now,
                    HostMsg::BackupVerify {
                        name,
                        sha256: digest,
                    },
                    out,
                );
            }
            (Phase::FRead { ticket: t, offset }, HostOutcome::Data(data)) if t == ticket => {
                let Command::Factory(b) = &step.cmd else {
                    return;
                };
                let size = b.size;
                let want = (size - offset).min(self.max_data as u64);
                let frame = frame::encode(Kind::Data, self.tx_next, offset, &data, self.max_data);
                match frame {
                    Ok(bytes) if data.len() as u64 == want => {
                        self.tx_next = self.tx_next.wrapping_add(1);
                        out.push(HostOutput::Send(bytes));
                        ev(
                            out,
                            Event::FactorySent {
                                sent: offset + want,
                                size,
                            },
                        );
                        self.factory_next(now, offset + want, out);
                    }
                    _ => self.fail_step(now, out),
                }
            }
            (Phase::BOpen(t) | Phase::BHash(t) | Phase::FRead { ticket: t, .. }, _)
                if t == ticket =>
            {
                self.fail_step(now, out)
            }
            _ => {} // an answer nobody waits for
        }
    }

    /// The shell could not do its part: the step is over, and a stream that
    /// is open is ended.
    fn fail_step(&mut self, now: u64, out: &mut Out) {
        let Some(step) = self.step.take() else { return };
        ev(
            out,
            Event::Failed {
                step: step.cmd.name(),
            },
        );
        let streaming = matches!(
            step.phase,
            Phase::BBegin { .. } | Phase::BData { .. } | Phase::FRead { .. }
        );
        if streaming && self.conn == Conn::Ready {
            self.step = Some(new_step(Command::Abort));
            self.run(now, out);
        }
    }

    fn on_tick(&mut self, now: u64, out: &mut Out) {
        if self.rehello_at.is_some_and(|t| now >= t) {
            return self.hello(now, out);
        }
        if self.awaiting() && now >= self.quiet_since + SILENCE_MS {
            return self.lost(now, false, out);
        }
        if let (Conn::Ready, Some(step)) = (&self.conn, &mut self.step) {
            if step.wake.is_some_and(|t| now >= t) {
                step.wake = None;
                step.phase = Phase::Start;
                self.run(now, out);
            }
        }
    }
}

fn new_step(cmd: Command) -> Step {
    Step {
        cmd,
        phase: Phase::Start,
        id: None,
        busy: 0,
        wake: None,
        parts: Vec::new(),
        sums: Vec::new(),
        tries: 0,
    }
}

/// What is wrong with a frame from the phone, for a log. The machine itself
/// only needs to know that something is.
pub fn explain(bytes: &[u8], max_data: u32) -> Option<FrameError> {
    frame::decode(bytes, max_data).err()
}
