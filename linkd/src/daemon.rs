// SPDX-License-Identifier: MIT
//! `omamobile-linkd`: the shell around the protocol crate's phone machine
//! (INSTALL-LINK-PLAN stage L2). It fills in what the machine asks about
//! the phone, carries out what the machine returns, in order, and moves
//! the frames. It applies no rule of the protocol itself.
use crate::bus::Bus;
use crate::descriptor::Descriptor;
use crate::durable::{self, kill_point};
use crate::found;
use crate::paths::Root;
use crate::programs::{Programs, UnitState, DISPLAY_MANAGER, FIRSTBOOT_UNIT, PROVISION_UNIT};
use crate::sockets::{self, Line, Who};
use omamobile_link::digest::Hasher;
use omamobile_link::msg::{Progress, Stage, StepResult};
use omamobile_link::phone::{
    Effect, Outcome, PartFound, Persistent, Phone, PhoneConfig, PhoneInput, PhoneOutput, Screen,
    Ticket,
};
use omamobile_link::Entropy;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The working space `factory-begin` wants beside the image (§6.4): "at
/// most about 24 GiB at the peak", until runbook step R1 has measured it.
pub const WORKING_SPACE: u64 = 24 << 30;
/// A write the bus has not taken after this long is a lost link (§5.7).
pub const WRITE_TIMEOUT_MS: u64 = 30_000;
/// With no agent, the session counts as up this long after the display
/// manager started (§2.8).
const SESSION_AFTER_US: u64 = 180_000_000;
/// `smoke` waits this long for the session (§5.6).
const SMOKE_WAIT_MS: u64 = 180_000;

/// What carries the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// None was named: the daemon offers no link.
    None,
    /// The `SOCK_SEQPACKET` socket on standard input (stage L2, the tests).
    Stdio,
    /// FunctionFS, mounted by `omamobile-link-gadget add` (stage L3, the
    /// phone).
    Ffs,
}

/// How long `omamobile-link-gadget add` may take to mount FunctionFS, and
/// then to put the function on the bus once the descriptors are written.
const GADGET_WAIT: Duration = Duration::from_secs(20);

/// How the daemon was started.
#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub transport: Transport,
    // What follows is for tests, and taken on a scratch root only.
    /// The machine's clock runs this many times faster than the wall's.
    pub clock_scale: u64,
    /// How often the units and the worker's progress are looked at.
    pub poll_ms: u64,
    pub write_timeout_ms: u64,
    pub max_data: Option<u32>,
    pub ack_every: Option<u64>,
    pub window: Option<u64>,
    pub working_space: u64,
    /// The kill test's counter ([`durable::arm_kill_points`]).
    pub die_file: Option<PathBuf>,
}

impl Options {
    pub fn new(root: PathBuf) -> Options {
        Options {
            root,
            transport: Transport::None,
            clock_scale: 1,
            poll_ms: 1000,
            write_timeout_ms: WRITE_TIMEOUT_MS,
            max_data: None,
            ack_every: None,
            window: None,
            working_space: WORKING_SPACE,
            die_file: None,
        }
    }
}

/// One thing that happened, for the daemon's one loop.
pub enum Ev {
    /// The stand-in for FunctionFS's `ENABLE` (true) and `DISABLE`.
    Link(bool),
    /// One completed read.
    Frame(Vec<u8>),
    /// FunctionFS's event from `ep0`.
    Function(crate::ffs::Event),
    /// The transport is gone for good.
    Gone,
    /// A write the bus did not take in time.
    WriteStalled,
    /// Nothing waits for the bus any more.
    QueueEmpty,
    Done(Ticket, Outcome),
    FirstbootEnded(Vec<String>),
    ProvisionEnded(Option<Vec<StepResult>>),
    Progress(Progress),
    SessionUp,
    Peer(Who, u64, UnixStream),
    PeerGone(Who, u64),
    Line(Who, u64, Line),
}

fn log(msg: &str) {
    eprintln!("omamobile-linkd: {msg}");
}

/// The kernel's random numbers (`getrandom`).
struct OsRng;

impl Entropy for OsRng {
    fn fill(&mut self, buf: &mut [u8]) {
        let mut at = 0;
        while at < buf.len() {
            match rustix::rand::getrandom(&mut buf[at..], rustix::rand::GetRandomFlags::empty()) {
                Ok(n) => at += n,
                Err(rustix::io::Errno::INTR) => {}
                // No code and no nonce may ever come from anything else
                Err(_) => {
                    log("no random numbers from the kernel");
                    std::process::exit(1);
                }
            }
        }
    }
}

struct Clock {
    start: Instant,
    scale: u64,
}

impl Clock {
    /// Milliseconds on a clock that does not go back and does not follow
    /// the wall clock.
    fn now(&self) -> u64 {
        (self.start.elapsed().as_millis() as u64).saturating_mul(self.scale)
    }
    /// The real time `ms` of the machine's clock take.
    fn real(&self, ms: u64) -> Duration {
        Duration::from_millis(ms.div_ceil(self.scale))
    }
}

/// What the watcher thread looks at for the daemon.
#[derive(Default)]
struct Watch {
    firstboot: AtomicBool,
    provision: AtomicBool,
    /// The owner's session is up: an agent connected, or the display
    /// manager has been up for three minutes.
    session: AtomicBool,
}

fn screen_line(s: &Screen) -> String {
    match s {
        Screen::Code(code) => format!("code {}", code.digits()),
        Screen::WrongCode { left } => format!("wrong-code {left}"),
        Screen::Locked { wait_s } => format!("locked {wait_s}"),
        Screen::Paired => "paired".into(),
        Screen::Version => "version".into(),
        Screen::Setup => "setup".into(),
        Screen::Stopped { step } => format!("stopped {step}"),
        Screen::NoSpace => "no-space".into(),
        Screen::Damaged => "damaged".into(),
        Screen::Off => "off".into(),
    }
}

/// The first word of `done`: why the link ended.
pub fn done_reason(root: &Root) -> Option<String> {
    let text = fs::read_to_string(root.done()).ok()?;
    Some(text.split_whitespace().next().unwrap_or("").to_string())
}

struct Daemon {
    root: Root,
    opts: Options,
    programs: Programs,
    desc: Descriptor,
    phone: Phone,
    rng: OsRng,
    clock: Clock,
    tx: Sender<Ev>,
    bus: Option<Bus>,
    agent: Option<(u64, UnixStream)>,
    screen: Option<(u64, UnixStream)>,
    /// What the screen shows now: for a screen program that connects later.
    last_screen: Option<String>,
    image: Option<File>,
    /// The partitions as the last `backup-list` found them.
    parts: Vec<PartFound>,
    /// A partition's next chunk, read when the bus has taken the one before
    /// (§5.7, item 4).
    pending_read: Option<(Ticket, String, u64, u32)>,
    watch: Arc<Watch>,
    /// `omamobile-link-gadget add` ran, and `remove` has not.
    offered: bool,
    /// FunctionFS: the function is enabled (the machine was told `LinkUp`).
    enabled: bool,
    exit: Option<i32>,
}

/// Run the daemon; its exit status.
pub fn run(opts: Options) -> i32 {
    let root = Root::new(&opts.root);
    if let Some(file) = opts.die_file.clone().filter(|_| root.scratch()) {
        durable::arm_kill_points(file);
    }
    let programs = Programs::new(&root);
    // A daemon that was killed in this boot left the gadget changed, with
    // nothing behind the link's function: put it back before anything else,
    // so that `add` never meets a gadget that already has the function
    if root.offered().exists() {
        log("the gadget was left changed by a daemon that died: putting it back");
        if let Err(e) = programs.gadget("remove") {
            log(&format!("omamobile-link-gadget remove: {e}"));
            return 1;
        }
        let _ = fs::remove_file(root.offered());
    }
    // The unit's condition, again: no marker, nothing to do, nothing touched
    if !root.marker().exists() {
        log("no marker: the link is not offered on this phone");
        return 0;
    }
    let boot_id = found::boot_id(&root);
    // `omamobile-install-link reopen` takes effect at the next boot (§2.8):
    // a start in the boot it ran in ends here, before anything is counted
    // or changed
    if let Ok(reopened) = fs::read_to_string(root.reopened()) {
        if boot_id.is_empty() {
            log("reopened, and this boot's id cannot be read: nothing is offered");
            return 1;
        }
        if reopened.trim() == boot_id {
            log("reopened in this boot: the link is offered from the next boot");
            return 0;
        }
        if let Err(e) = durable::remove(&root.reopened()) {
            log(&format!("the reopen record cannot be removed: {e}"));
            return 1;
        }
    }
    // Closed for this boot (§2.2): the next boot opens it again
    if root.closed().exists() {
        log("closed for this boot");
        return 0;
    }
    let desc = Descriptor::parse(&fs::read_to_string(root.descriptor()).unwrap_or_default());
    let seen = found::found(&root, &programs);
    let serial = found::serial(&root);
    let mut cfg = PhoneConfig::new(
        env!("CARGO_PKG_VERSION"),
        desc.get("CODENAME").unwrap_or(""),
        serial.as_deref().unwrap_or(b""),
    );
    if let Some(optional) = desc.get("PROVISION_OPTIONAL") {
        cfg.optional = optional.split_whitespace().map(str::to_string).collect();
    }
    if root.scratch() {
        cfg.max_data = opts.max_data.unwrap_or(cfg.max_data);
        cfg.ack_every = opts.ack_every.unwrap_or(cfg.ack_every);
        cfg.window = opts.window.unwrap_or(cfg.window);
    }
    let clock = Clock {
        start: Instant::now(),
        scale: opts.clock_scale.max(1),
    };
    let mut rng = OsRng;
    let (phone, out) = Phone::start(cfg, &seen, clock.now(), &mut rng);
    if !phone.stopped() {
        // This boot would offer the link. Not without what it needs:
        if serial.is_none() || desc.get("CODENAME").is_none() {
            log("no serial in bootconfig, or no codename in the descriptor: nothing is offered");
            return 1;
        }
        if opts.transport == Transport::None {
            log("no transport: --transport ffs (FunctionFS) or stdio (a socket pair)");
            return 1;
        }
    }
    let (tx, rx) = mpsc::channel();
    let mut d = Daemon {
        root,
        opts,
        programs,
        desc,
        phone,
        rng,
        clock,
        tx,
        bus: None,
        agent: None,
        screen: None,
        last_screen: None,
        image: None,
        parts: Vec::new(),
        pending_read: None,
        watch: Arc::new(Watch::default()),
        offered: false,
        enabled: false,
        exit: None,
    };
    d.carry_out(out);
    if d.exit.is_none() && d.phone.stopped() {
        d.exit = Some(0);
    }
    if d.exit.is_none() {
        d.after_start();
        log("link: offered");
    }
    d.main_loop(&rx)
}

impl Daemon {
    fn main_loop(&mut self, rx: &Receiver<Ev>) -> i32 {
        loop {
            if let Some(code) = self.exit {
                self.leave();
                return code;
            }
            self.read_pending();
            let wait = match self.phone.deadline() {
                Some(t) => self.clock.real(t.saturating_sub(self.clock.now())),
                None => Duration::from_secs(3600),
            };
            match rx.recv_timeout(wait) {
                Ok(ev) => self.on_event(ev),
                Err(RecvTimeoutError::Timeout) => self.feed(PhoneInput::Tick),
                Err(RecvTimeoutError::Disconnected) => return 1,
            }
        }
    }

    /// The daemon stops, for whatever reason: the gadget goes back as the
    /// initramfs made it (§3.3).
    fn leave(&mut self) {
        if self.offered {
            self.flush();
            // FunctionFS: the function's files are closed before `remove`
            // takes the function out and unmounts it
            if self.opts.transport == Transport::Ffs {
                if let Some(bus) = self.bus.take() {
                    bus.stop();
                }
            }
            match self.programs.gadget("remove") {
                Ok(()) => {
                    let _ = fs::remove_file(self.root.offered());
                }
                Err(e) => log(&format!("omamobile-link-gadget remove: {e}")),
            }
            self.offered = false;
        }
    }

    fn feed(&mut self, input: PhoneInput<'_>) {
        let out = self.phone.handle(self.clock.now(), input, &mut self.rng);
        self.carry_out(out);
    }

    fn on_event(&mut self, ev: Ev) {
        match ev {
            Ev::Link(up) => {
                log(if up { "link: ENABLE" } else { "link: DISABLE" });
                self.feed(if up {
                    PhoneInput::LinkUp
                } else {
                    PhoneInput::LinkDown
                });
                self.handled();
            }
            Ev::Frame(bytes) => {
                self.feed(PhoneInput::Frame(&bytes));
                self.handled();
            }
            Ev::Function(event) => {
                use crate::ffs::Event;
                match event {
                    Event::Enable | Event::Disable | Event::Unbind => {
                        log(&format!("link: {}", event.name()));
                        let up = event == Event::Enable;
                        // DISABLE then UNBIND is one link going down
                        if up != self.enabled {
                            self.enabled = up;
                            self.feed(if up {
                                PhoneInput::LinkUp
                            } else {
                                PhoneInput::LinkDown
                            });
                        }
                    }
                    // §4: logged, and nothing changes. A host that sleeps
                    // comes back into the same session
                    Event::Bind | Event::Suspend | Event::Resume => {
                        log(&format!("link: {}", event.name()))
                    }
                    Event::Setup {
                        request_type,
                        request,
                        ..
                    } => log(&format!(
                        "link: SETUP {request_type:#04x} {request:#04x}, stalled"
                    )),
                    Event::Unknown(t) => log(&format!("link: an unknown event {t}")),
                }
            }
            Ev::Gone => {
                log("link: gone");
                self.feed(PhoneInput::LinkDown);
            }
            Ev::WriteStalled => {
                // §5.7: the session ends; the link itself is still there,
                // and the next `hello` is answered
                log("link: a write was not taken in time; the session ends");
                self.feed(PhoneInput::LinkDown);
                if let Some(bus) = &self.bus {
                    bus.session_ended();
                }
                self.feed(PhoneInput::LinkUp);
            }
            Ev::QueueEmpty => {}
            Ev::Done(ticket, outcome) => self.feed(PhoneInput::Done(ticket, outcome)),
            Ev::FirstbootEnded(failed) => self.feed(PhoneInput::FirstbootEnded { failed }),
            Ev::ProvisionEnded(result) => self.feed(PhoneInput::ProvisionEnded { result }),
            Ev::Progress(p) => self.feed(PhoneInput::Progress(p)),
            Ev::SessionUp => self.feed(PhoneInput::SessionUp),
            Ev::Peer(Who::Agent, id, stream) => {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                self.agent = Some((id, stream));
                self.watch.session.store(true, Ordering::SeqCst);
                log("agent: connected");
                self.feed(PhoneInput::AgentUp);
            }
            Ev::Peer(Who::Screen, id, mut stream) => {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                if let Some(line) = &self.last_screen {
                    sockets::say(&mut stream, line);
                }
                self.screen = Some((id, stream));
            }
            Ev::PeerGone(Who::Agent, id) => {
                if self.agent.as_ref().is_some_and(|a| a.0 == id) {
                    self.agent = None;
                    log("agent: gone");
                    self.feed(PhoneInput::AgentDown);
                }
            }
            Ev::PeerGone(Who::Screen, id) => {
                if self.screen.as_ref().is_some_and(|s| s.0 == id) {
                    self.screen = None;
                }
            }
            Ev::Line(Who::Agent, id, line) => {
                if self.agent.as_ref().is_some_and(|a| a.0 == id) {
                    self.feed(match line {
                        Line::Show => PhoneInput::AgentShow,
                        _ => PhoneInput::AgentHide,
                    });
                }
            }
            Ev::Line(Who::Screen, id, _) => {
                if self.screen.as_ref().is_some_and(|s| s.0 == id) {
                    self.feed(PhoneInput::Key);
                }
            }
        }
    }

    fn handled(&self) {
        if let Some(bus) = &self.bus {
            bus.handled();
        }
    }

    fn flush(&self) {
        if let Some(bus) = &self.bus {
            bus.flush(Duration::from_millis(self.opts.write_timeout_ms));
        }
    }

    /// Carry out a list the machine returned, in order. An effect with a
    /// ticket that is answered at once has its answer's own list carried
    /// out after what is already there. An effect that cannot be carried
    /// out ends the list: the machine is told (`Fault`) and stops.
    fn carry_out(&mut self, out: Vec<PhoneOutput>) {
        let mut queue: VecDeque<PhoneOutput> = out.into();
        while let Some(o) = queue.pop_front() {
            kill_point();
            let effect = match o {
                PhoneOutput::Send(frame) => {
                    if let Some(bus) = &self.bus {
                        bus.send(frame);
                    }
                    continue;
                }
                PhoneOutput::Do(e) => e,
            };
            match self.effect(effect) {
                Ok(Some((ticket, outcome))) => {
                    let now = self.clock.now();
                    let more =
                        self.phone
                            .handle(now, PhoneInput::Done(ticket, outcome), &mut self.rng);
                    queue.extend(more);
                }
                Ok(None) => {}
                Err(e) => {
                    log(&format!("an effect could not be carried out: {e}"));
                    queue.clear();
                    let now = self.clock.now();
                    queue.extend(self.phone.handle(now, PhoneInput::Fault, &mut self.rng));
                    if self.exit.is_none() && queue.is_empty() {
                        self.exit = Some(1);
                    }
                }
            }
        }
    }

    /// After the start's list, when the link is offered: what a daemon that
    /// was killed may have left undone.
    fn after_start(&mut self) {
        #[allow(clippy::single_match)]
        match self.phone.stage() {
            Stage::Firstboot => {
                // A daemon killed between placing the answers and starting
                // the unit: nothing else starts it before the next boot
                if self.programs.unit_state(FIRSTBOOT_UNIT) == Some(UnitState::Inactive)
                    && self.root.firstboot_conf().exists()
                {
                    if let Err(e) = self.programs.start_unit(FIRSTBOOT_UNIT) {
                        log(&format!("first boot's unit: {e}"));
                    }
                }
                self.watch.firstboot.store(true, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    fn start_offer(&mut self) -> io::Result<()> {
        fs::create_dir_all(self.root.run_dir())?;
        fs::write(self.root.offered(), b"")?;
        let write_timeout = Duration::from_millis(self.opts.write_timeout_ms);
        if self.opts.transport == Transport::Ffs {
            self.offered = true;
            let function = self.add_function()?;
            self.bus = Some(Bus::start_ffs(
                function,
                self.phone.max_data(),
                write_timeout,
                self.tx.clone(),
            )?);
        } else {
            // Set before `add` runs: one that fails is followed by `remove`
            // when the daemon leaves, which finds nothing to put back and
            // clears the record
            self.offered = true;
            self.programs.gadget("add")?;
            // The link itself: the socket on standard input
            let sock = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
            self.bus = Some(Bus::start(
                sock,
                self.phone.max_data(),
                write_timeout,
                self.tx.clone(),
            )?);
        }
        // The owner's session agent: the account first boot made, nobody else
        let root = self.root.clone();
        sockets::listen(
            &self.root.agent_socket(),
            Who::Agent,
            0o666,
            move |uid| found::firstboot_uid(&root) == Some(uid),
            self.tx.clone(),
        )?;
        // The first-start screen program: root (under a scratch root, the
        // user the daemon runs as)
        let me = rustix::process::geteuid().as_raw();
        sockets::listen(
            &self.root.screen_socket(),
            Who::Screen,
            0o600,
            move |uid| uid == 0 || uid == me,
            self.tx.clone(),
        )?;
        self.start_watcher();
        Ok(())
    }

    /// `omamobile-link-gadget add` (§3.3), with the daemon's part in the
    /// middle of it: the script mounts FunctionFS and waits; the daemon
    /// writes the descriptors and the strings to `ep0`, and the endpoint
    /// files appear; the script links the function and binds the UDC.
    fn add_function(&mut self) -> io::Result<crate::ffs::Function> {
        let dir = self.root.ffs_dir();
        let mut child = self.programs.gadget_spawn("add")?;
        let end = Instant::now() + GADGET_WAIT;
        let failed = |what: &str| io::Error::other(format!("omamobile-link-gadget add: {what}"));
        // The script has mounted FunctionFS when ep0 is there
        let ep0 = loop {
            if dir.join("ep0").exists() {
                match crate::ffs::open_ep0(&dir) {
                    Ok(ep0) => break ep0,
                    Err(e) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(failed(&format!("the descriptors were refused: {e}")));
                    }
                }
            }
            if let Some(status) = child.try_wait()? {
                crate::programs::gadget_status(status)?;
                return Err(failed("ended before FunctionFS was mounted"));
            }
            if Instant::now() >= end {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed("FunctionFS was not mounted in time"));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Then it links the function and binds the UDC
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= end {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed("the function was not put on the bus in time"));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        crate::programs::gadget_status(status)?;
        crate::ffs::open_endpoints(&dir, ep0)
    }

    fn start_watcher(&self) {
        let (root, programs, watch, tx) = (
            self.root.clone(),
            self.programs.clone(),
            self.watch.clone(),
            self.tx.clone(),
        );
        let poll = Duration::from_millis(self.opts.poll_ms.max(1));
        std::thread::spawn(move || {
            let mut last_progress: Option<Progress> = None;
            let mut session_said = false;
            loop {
                std::thread::sleep(poll);
                if watch.firstboot.load(Ordering::SeqCst) {
                    let ended = if root.firstboot_done().exists() {
                        Some(Vec::new())
                    } else {
                        match programs.unit_state(FIRSTBOOT_UNIT) {
                            Some(UnitState::Failed) => {
                                let mut failed = found::firstboot_failed(&root);
                                if failed.is_empty() {
                                    failed.push("unknown".to_string());
                                }
                                Some(failed)
                            }
                            // It deletes firstboot.conf, then writes `done`:
                            // between the two, the account is the witness
                            Some(UnitState::Inactive)
                                if !root.firstboot_conf().exists()
                                    && found::firstboot_uid(&root).is_some() =>
                            {
                                Some(Vec::new())
                            }
                            _ => None,
                        }
                    };
                    if let Some(failed) = ended {
                        watch.firstboot.store(false, Ordering::SeqCst);
                        if tx.send(Ev::FirstbootEnded(failed)).is_err() {
                            return;
                        }
                    }
                }
                if watch.provision.load(Ordering::SeqCst) {
                    let progress = found::provision_progress(&root);
                    if progress.is_some() && progress != last_progress {
                        last_progress = progress.clone();
                        if let Some(p) = progress {
                            let _ = tx.send(Ev::Progress(p));
                        }
                    }
                    if programs.unit_state(PROVISION_UNIT) != Some(UnitState::Running) {
                        watch.provision.store(false, Ordering::SeqCst);
                        last_progress = None;
                        if tx
                            .send(Ev::ProvisionEnded(found::provision_result(&root)))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                if !session_said && root.firstboot_done().exists() {
                    if watch.session.load(Ordering::SeqCst) {
                        session_said = true;
                    } else if let Some(since) = programs.active_since_us(DISPLAY_MANAGER) {
                        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
                        let now_us = now.tv_sec as u64 * 1_000_000 + now.tv_nsec as u64 / 1000;
                        if now_us.saturating_sub(since) >= SESSION_AFTER_US {
                            session_said = true;
                            watch.session.store(true, Ordering::SeqCst);
                            if tx.send(Ev::SessionUp).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
    }

    /// A partition's next chunk, once the bus has taken the frame before.
    fn read_pending(&mut self) {
        if self.pending_read.is_none() || !self.bus.as_ref().is_none_or(Bus::idle) {
            return;
        }
        let Some((ticket, name, offset, len)) = self.pending_read.take() else {
            return;
        };
        let data = self.parts.iter().find(|p| p.name == name).and_then(|p| {
            let f = File::open(self.root.at("/dev").join(&p.dev)).ok()?;
            let mut buf = vec![0u8; len as usize];
            f.read_exact_at(&mut buf, offset).ok()?;
            Some(buf)
        });
        self.feed(PhoneInput::Done(
            ticket,
            data.map_or(Outcome::Failed, Outcome::Data),
        ));
    }

    /// A slow answer, on a thread of its own: the machine goes on answering
    /// `ping`, `status` and a new `hello` meanwhile.
    fn later(&self, ticket: Ticket, work: impl FnOnce() -> Outcome + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Ev::Done(ticket, work()));
        });
    }

    fn open_image(&mut self) -> io::Result<&File> {
        if self.image.is_none() {
            self.image = Some(OpenOptions::new().write(true).open(self.root.image())?);
        }
        self.image
            .as_ref()
            .ok_or_else(|| io::Error::other("no image file"))
    }

    fn delete_image(&mut self) -> io::Result<()> {
        self.image = None;
        durable::remove(&self.root.image()).map(|_| ())
    }

    fn remove_work_dirs(&self) -> io::Result<()> {
        for dir in found::work_dirs_of(&self.root) {
            fs::remove_dir_all(dir)?;
        }
        if self.root.work_parent().exists() {
            durable::sync_dir(&self.root.work_parent())?;
        }
        Ok(())
    }

    fn write_boot_update_conf(&self) -> io::Result<()> {
        // The descriptor's value, which no message can set (the plan's Q3)
        let auto = match self.desc.get("BOOT_UPDATE_AUTO_WRITE") {
            Some("no") => "no",
            _ => "yes",
        };
        let text = format!(
            "# Written by the install link: a release install takes kernel updates through\n\
             # omamobile-boot-update's pacman hook (docs/RELEASES.md §5.3). AUTO_WRITE=no stops it.\n\
             AUTO_WRITE={auto}\n"
        );
        durable::write(&self.root.boot_update_conf(), text.as_bytes(), 0o644)
    }

    /// The end, in an order a kill at any point leaves completable: `done`
    /// first (from then on every start ends at §6.5's row 1), then what the
    /// end deletes, and the marker last, so that the unit's condition lets
    /// one more start through to finish the job.
    fn end(&mut self, done: &str, finished: bool) -> io::Result<()> {
        fs::create_dir_all(self.root.state_dir())?;
        durable::write(&self.root.done(), format!("{done}\n").as_bytes(), 0o644)?;
        kill_point();
        self.clean_up(finished)?;
        kill_point();
        durable::remove(&self.root.marker())?;
        self.exit = Some(0);
        Ok(())
    }

    /// What an end deletes. `finished`: the token and the state too. The
    /// other ends keep the state file (the stage, no token) and the pending
    /// checksums, which is what `reopen` starts from.
    fn clean_up(&mut self, finished: bool) -> io::Result<()> {
        self.delete_image()?;
        self.remove_work_dirs()?;
        durable::remove(&self.root.result())?;
        if finished {
            durable::remove(&self.root.state())?;
            durable::remove(&self.root.pending_sums())?;
        }
        Ok(())
    }

    /// One effect. `Ok(Some(..))`: its answer, at once.
    fn effect(&mut self, effect: Effect) -> io::Result<Option<(Ticket, Outcome)>> {
        let root = self.root.clone();
        match effect {
            Effect::Offer => self.start_offer()?,
            Effect::Screen(s) => {
                // The line with the code goes to the screen and nowhere else
                let line = screen_line(&s);
                if let Some((_, stream)) = &mut self.screen {
                    sockets::say(stream, &line);
                }
                self.last_screen = Some(line);
            }
            Effect::DropUnsent => {
                if let Some(bus) = &self.bus {
                    bus.drop_unsent();
                }
            }
            Effect::Agent(line) => {
                if let Some((_, stream)) = &mut self.agent {
                    sockets::say(stream, &line.line());
                }
            }
            Effect::Persist(p) => self.persist(&p)?,
            Effect::ReadFacts { ticket } => {
                return Ok(Some((ticket, Outcome::Facts(found::facts(&root)))))
            }
            Effect::CheckAnswers { ticket, conf } => {
                fs::create_dir_all(root.run_dir())?;
                let written = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(root.candidate())
                    .and_then(|f| {
                        f.set_permissions(fs::Permissions::from_mode(0o600))?;
                        f.write_all_at(conf.as_bytes(), 0)
                    });
                if written.is_err() {
                    return Ok(Some((ticket, Outcome::Failed)));
                }
                let programs = self.programs.clone();
                self.later(ticket, move || {
                    programs
                        .check_answers()
                        .map_or(Outcome::Failed, Outcome::Problems)
                });
            }
            Effect::StartFirstboot { ticket, retry } => {
                let placed = (|| -> io::Result<()> {
                    if !retry {
                        // §6.5's order: boot-update.conf, the candidate
                        // copied (it is on a tmpfs: not renamed), the unit
                        let candidate = fs::read(root.candidate())?;
                        self.write_boot_update_conf()?;
                        kill_point();
                        durable::write(&root.firstboot_conf(), &candidate, 0o600)?;
                        kill_point();
                    }
                    self.programs.start_unit(FIRSTBOOT_UNIT)?;
                    if !retry {
                        let _ = fs::remove_file(root.candidate());
                    }
                    Ok(())
                })();
                if placed.is_ok() {
                    self.watch.firstboot.store(true, Ordering::SeqCst);
                }
                return Ok(Some((
                    ticket,
                    placed.map_or(Outcome::Failed, |()| Outcome::Done),
                )));
            }
            Effect::BootUpdateConf => self.write_boot_update_conf()?,
            Effect::BackupList { ticket } => {
                let outcome = match found::backup_list(&root, &self.desc) {
                    Ok(parts) => {
                        self.parts = parts.clone();
                        Outcome::Parts(parts)
                    }
                    Err(bad) => Outcome::Precondition(bad),
                };
                return Ok(Some((ticket, outcome)));
            }
            Effect::BackupRead {
                ticket,
                name,
                offset,
                len,
            } => self.pending_read = Some((ticket, name, offset, len)),
            Effect::BackupDigest { ticket, name } => {
                let Some(part) = self.parts.iter().find(|p| p.name == name).cloned() else {
                    return Ok(Some((ticket, Outcome::Failed)));
                };
                let path = root.at("/dev").join(&part.dev);
                self.later(ticket, move || {
                    digest_of(&path, Some(part.size)).map_or(Outcome::Failed, Outcome::Digest)
                });
            }
            Effect::BackupKeep { ticket, sums } => {
                let kept = durable::write(&root.pending_sums(), sums.as_bytes(), 0o600);
                return Ok(Some((
                    ticket,
                    kept.map_or(Outcome::Failed, |()| Outcome::Done),
                )));
            }
            Effect::SumsDelete => {
                durable::remove(&root.pending_sums())?;
            }
            Effect::FactoryOpen {
                ticket,
                size,
                resume_at,
            } => {
                let len = fs::metadata(root.image()).map_or(0, |m| m.len());
                let have = resume_at.min(len);
                let working = if root.scratch() {
                    self.opts.working_space
                } else {
                    WORKING_SPACE
                };
                let need = size.saturating_sub(have).saturating_add(working);
                let free = found::free_bytes(&root);
                if need > free {
                    return Ok(Some((ticket, Outcome::NoSpace { need, free })));
                }
                let opened = (|| -> io::Result<File> {
                    let f = OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .mode(0o600)
                        .open(root.image())?;
                    f.set_len(have)?;
                    f.sync_all()?;
                    durable::sync_dir(&root.state_dir())?;
                    Ok(f)
                })();
                return Ok(Some((
                    ticket,
                    match opened {
                        Ok(f) => {
                            self.image = Some(f);
                            Outcome::FactoryOpened { have }
                        }
                        Err(_) => Outcome::Failed,
                    },
                )));
            }
            Effect::FactoryWrite { offset, data, sync } => {
                let f = self.open_image()?;
                f.write_all_at(&data, offset)?;
                if sync {
                    // The state file's `synced` and the `ack` behind this
                    // rely on it (§6.5)
                    f.sync_all()?;
                }
            }
            Effect::FactoryTruncate { len } => {
                self.image = None;
                let f = OpenOptions::new().write(true).open(root.image())?;
                f.set_len(len)?;
                f.sync_all()?;
            }
            Effect::FactoryDigest { ticket } => {
                let path = root.image();
                self.later(ticket, move || {
                    digest_of(&path, None).map_or(Outcome::Failed, Outcome::Digest)
                });
            }
            Effect::FactoryDelete => self.delete_image()?,
            Effect::StartProvision => {
                let _ = fs::remove_file(root.progress());
                self.programs.start_provision()?;
                self.watch.provision.store(true, Ordering::SeqCst);
            }
            Effect::ProvisionWatch => self.watch.provision.store(true, Ordering::SeqCst),
            Effect::WorkDirRemove => self.remove_work_dirs()?,
            Effect::Sweep => {
                for path in found::leftovers(&root) {
                    fs::remove_file(path)?;
                }
            }
            Effect::Smoke { ticket } => {
                let (programs, watch) = (self.programs.clone(), self.watch.clone());
                let wait = self.clock.real(SMOKE_WAIT_MS);
                self.later(ticket, move || {
                    let end = Instant::now() + wait;
                    while !watch.session.load(Ordering::SeqCst) && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    programs.smoke().map_or(Outcome::Failed, Outcome::Report)
                });
            }
            Effect::Restart => {
                // After the frames before it are written
                self.flush();
                self.programs.reboot()?;
            }
            Effect::Close => {
                self.flush();
                fs::create_dir_all(root.run_dir())?;
                fs::write(root.closed(), b"")?;
                self.exit = Some(0);
            }
            Effect::Finish => {
                self.flush();
                self.end("finished", true)?;
            }
            Effect::Expire { missing } => {
                let mut done = String::from("expired");
                for m in missing {
                    done.push(' ');
                    done.push_str(m.as_str());
                }
                self.end(&done, false)?;
            }
            Effect::Refuse { reason } => self.end(reason.as_str(), false)?,
            Effect::DeleteMarker => {
                // `done` is there: an end that a kill cut short is finished
                let finished = done_reason(&root).as_deref() == Some("finished");
                self.clean_up(finished)?;
                durable::remove(&root.marker())?;
                self.exit = Some(0);
            }
            Effect::Stop => self.exit = Some(1),
            // The kind of thing, never content (§9)
            Effect::Note(note) => log(&format!("{note:?}")),
        }
        Ok(None)
    }

    fn persist(&self, p: &Persistent) -> io::Result<()> {
        fs::create_dir_all(self.root.state_dir())?;
        durable::write(&self.root.state(), p.to_json().as_bytes(), 0o600)
    }
}

/// SHA-256 of a whole file, or of its first `len` bytes exactly.
fn digest_of(path: &std::path::Path, len: Option<u64>) -> Option<[u8; 32]> {
    let mut f = File::open(path).ok()?;
    let mut h = Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len.unwrap_or(u64::MAX);
    while left > 0 {
        let want = buf.len().min(left.min(usize::MAX as u64) as usize);
        match f.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => {
                h.update(&buf[..n]);
                left -= n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    if len.is_some() && left != 0 {
        return None;
    }
    Some(h.finish())
}
