// SPDX-License-Identifier: MIT
//! A pretend phone that speaks the install link, for the tests, the
//! command-line tool's `fake-phone` build and the page's self-test. Never in
//! a shipping build (feature `pretend-link`; `tools/check-shipping`).
//!
//! It is the protocol crate's own stand-in for the phone's daemon
//! (`omamobile_link::sim::SimPhone`: the daemon's state machine, with its
//! files in memory), behind a [`LinkTransport`], with a pretend world
//! around it: the first start that takes a while, first-boot setup and
//! provisioning that end by themselves, a restart that takes the link off
//! the bus and brings it back, a cable that can be pulled. Time is its own:
//! a clock that moves when the installer waits, so a whole install takes no
//! real time. Nothing here opens a device or a file.
use super::{LinkShell, LinkTransport};
use crate::{Error, Result};
use omamobile_link::msg::{Progress, Stage, StepResult, StepState, What};
use omamobile_link::phone::{PartFound, PhoneConfig, PhoneInput};
use omamobile_link::sim::{SimPhone, TestRng};
use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

/// The pretend world's clock, shared with the shell that waits on it.
#[derive(Clone, Default)]
pub struct Clock(Rc<Cell<u64>>);

impl Clock {
    pub fn now(&self) -> u64 {
        self.0.get()
    }
    pub fn pass(&self, ms: u64) {
        self.0.set(self.0.get() + ms);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Happening {
    /// The link function is on the bus (a first start or a boot reached it).
    CableIn,
    FirstbootEnds,
    SessionUp,
    ProvisionStep(u32),
    ProvisionEnds,
    /// The planned restart: the link leaves, the phone starts again.
    Restart,
}

/// The steps a pretend provisioning reports, in order.
pub const PROVISION_STEPS: [&str; 6] = [
    "firmware",
    "sensors",
    "gnss",
    "modem",
    "fingerprint",
    "camera",
];

pub struct PretendPhone {
    /// The daemon and its files. Public so a test can look at what it has.
    pub sim: SimPhone,
    clock: Clock,
    up: bool,
    connected: bool,
    to_host: VecDeque<Vec<u8>>,
    schedule: Vec<(u64, Happening)>,
    seen: (u32, u32, u32),
    /// How long things take on the pretend phone, in its clock's ms.
    pub first_start_ms: u64,
    pub firstboot_ms: u64,
    pub provision_ms: u64,
    pub boot_ms: u64,
    /// First boot's setup fails at this step, once.
    pub firstboot_fails: Option<String>,
    /// Provisioning's camera step fails (twice, as the worker retries it).
    pub camera_fails: bool,
    /// The cable is pulled after this many frames reached the phone, and
    /// put back `replug_ms` later.
    pub unplug_after: Option<u64>,
    pub replug_ms: u64,
    /// Frames that reached the phone.
    pub frames_in: u64,
    /// Times the cable was pulled.
    pub pulls: u32,
    /// The pretend owner taps "Setup is not finished" in the unlocked
    /// session whenever the phone shows no code (every 5 s of its clock).
    pub owner_taps: bool,
    last_tap: u64,
}

/// Small stand-in radio partitions: made-up bytes, never a phone's.
pub fn parts() -> Vec<(PartFound, Vec<u8>)> {
    let mut rng = TestRng(11);
    [
        ("efs", 9000usize, true),
        ("efs_backup", 3000, true),
        ("modem_userdata", 2048, true),
        ("persist", 4096, true),
        ("modem_a", 5000, true),
        ("devinfo", 10, false),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, len, checked))| {
        (
            PartFound {
                name: name.into(),
                size: len as u64,
                checked,
                dev: format!("sda{}", i + 1),
            },
            rng.bytes(len),
        )
    })
    .collect()
}

impl PretendPhone {
    /// A phone just written, restarting into its first start. `serial` is
    /// what its bootloader handed the kernel: the one the write pinned, for
    /// the right phone.
    pub fn first_start(serial: &str, device: &str, clock: Clock, seed: u64) -> PretendPhone {
        let mut cfg = PhoneConfig::new("0.0.1-pretend", device, serial.as_bytes());
        // Small frames, so a small factory image is many of them
        cfg.max_data = 4096;
        cfg.ack_every = 16384;
        cfg.window = 32768;
        let mut sim = SimPhone::first_start(cfg, clock.now(), seed);
        sim.parts = parts();
        let mut me = PretendPhone {
            sim,
            clock,
            up: false,
            connected: false,
            to_host: VecDeque::new(),
            schedule: Vec::new(),
            seen: (0, 0, 0),
            first_start_ms: 4_000,
            firstboot_ms: 6_000,
            provision_ms: 12_000,
            boot_ms: 8_000,
            firstboot_fails: None,
            camera_fails: false,
            unplug_after: None,
            replug_ms: 3_000,
            frames_in: 0,
            pulls: 0,
            owner_taps: true,
            last_tap: 0,
        };
        let at = me.clock.now() + me.first_start_ms;
        me.schedule.push((at, Happening::CableIn));
        me
    }

    pub fn clock(&self) -> Clock {
        self.clock.clone()
    }

    /// What the phone's screen shows to type, if anything: the code.
    pub fn shows(&self) -> Option<String> {
        self.sim.phone.code().map(|c| c.digits().to_string())
    }

    /// The owner taps "Setup is not finished" in the unlocked session: the
    /// window shows a new code.
    pub fn tap(&mut self) -> Option<String> {
        self.feed(PhoneInput::AgentShow);
        self.shows()
    }

    /// The owner presses a volume key at the first-start screen.
    pub fn key(&mut self) {
        self.feed(PhoneInput::Key);
    }

    /// The cable is pulled now, and put back after `replug_ms`.
    pub fn pull(&mut self) {
        self.pulls += 1;
        self.up = false;
        self.connected = false;
        self.to_host.clear();
        let _ = self.sim.feed(self.clock.now(), PhoneInput::LinkDown);
        let at = self.clock.now() + self.replug_ms;
        self.schedule.push((at, Happening::CableIn));
    }

    fn feed(&mut self, input: PhoneInput<'_>) {
        let frames = self.sim.feed(self.clock.now(), input);
        if self.connected {
            self.to_host.extend(frames);
        }
        self.notice_jobs();
    }

    /// What the daemon started: first boot, provisioning, the restart.
    fn notice_jobs(&mut self) {
        let now = self.clock.now();
        let (fb, pr, rs) = self.seen;
        if self.sim.firstboot_starts > fb {
            self.schedule
                .push((now + self.firstboot_ms, Happening::FirstbootEnds));
        }
        if self.sim.provision_starts > pr {
            let step = self.provision_ms / (PROVISION_STEPS.len() as u64 + 1);
            for n in 0..PROVISION_STEPS.len() as u32 {
                self.schedule
                    .push((now + step * (n as u64 + 1), Happening::ProvisionStep(n)));
            }
            self.schedule
                .push((now + self.provision_ms, Happening::ProvisionEnds));
        }
        if self.sim.restarts > rs {
            self.schedule.push((now + 50, Happening::Restart));
        }
        self.seen = (
            self.sim.firstboot_starts,
            self.sim.provision_starts,
            self.sim.restarts,
        );
    }

    /// Everything due by the clock: the world's happenings and the daemon's
    /// own deadlines.
    fn catch_up(&mut self) {
        loop {
            let now = self.clock.now();
            let due = self
                .schedule
                .iter()
                .enumerate()
                .filter(|(_, (at, _))| *at <= now)
                .min_by_key(|(_, (at, _))| *at)
                .map(|(i, _)| i);
            if let Some(i) = due {
                let (_, h) = self.schedule.remove(i);
                self.happen(h);
                continue;
            }
            if self.sim.phone.deadline().is_some_and(|d| d <= now) {
                let before = self.sim.phone.deadline();
                self.feed(PhoneInput::Tick);
                if self.sim.phone.deadline() == before {
                    break;
                }
                continue;
            }
            break;
        }
    }

    fn happen(&mut self, h: Happening) {
        let now = self.clock.now();
        match h {
            Happening::CableIn => {
                self.up = !self.sim.dead && self.sim.offered;
            }
            Happening::FirstbootEnds => {
                let failed: Vec<String> = self.firstboot_fails.take().into_iter().collect();
                let ok = failed.is_empty();
                self.feed(PhoneInput::FirstbootEnded { failed });
                if ok {
                    self.schedule.push((now + 500, Happening::SessionUp));
                }
            }
            Happening::SessionUp => {
                self.feed(PhoneInput::SessionUp);
                self.feed(PhoneInput::AgentUp);
            }
            Happening::ProvisionStep(n) => {
                let step = PROVISION_STEPS[n as usize];
                self.feed(PhoneInput::Progress(Progress {
                    what: What::Provision,
                    step: step.into(),
                    done: n as u64 + 1,
                    total: PROVISION_STEPS.len() as u64,
                }));
            }
            Happening::ProvisionEnds => {
                let result: Vec<StepResult> = PROVISION_STEPS
                    .iter()
                    .map(|s| StepResult {
                        step: (*s).into(),
                        state: if self.camera_fails && *s == "camera" {
                            StepState::Failed
                        } else {
                            StepState::Ok
                        },
                    })
                    .collect();
                self.feed(PhoneInput::ProvisionEnded {
                    result: Some(result),
                });
            }
            Happening::Restart => {
                self.up = false;
                self.connected = false;
                self.to_host.clear();
                let _ = self.sim.feed(now, PhoneInput::LinkDown);
                self.sim.start(true, now);
                self.seen = (
                    self.sim.firstboot_starts,
                    self.sim.provision_starts,
                    self.sim.restarts,
                );
                self.schedule.push((now + self.boot_ms, Happening::CableIn));
                self.schedule
                    .push((now + self.boot_ms + 1_000, Happening::SessionUp));
            }
        }
    }

    fn lost() -> Error {
        Error::Transport("the pretend phone's cable is out".into())
    }
}

impl LinkTransport for PretendPhone {
    async fn connect(&mut self) -> Result<bool> {
        self.catch_up();
        if !self.up {
            return Ok(false);
        }
        self.connected = true;
        self.feed(PhoneInput::LinkUp);
        Ok(true)
    }

    async fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.catch_up();
        if !self.connected {
            return Err(Self::lost());
        }
        self.frames_in += 1;
        if self.unplug_after.is_some_and(|n| self.frames_in > n) {
            self.unplug_after = None;
            self.pull();
            return Err(Self::lost());
        }
        self.feed(PhoneInput::Frame(frame));
        Ok(())
    }

    async fn recv(&mut self, wait_ms: u64) -> Result<Option<Vec<u8>>> {
        self.catch_up();
        if !self.connected {
            return Err(Self::lost());
        }
        if let Some(f) = self.to_host.pop_front() {
            return Ok(Some(f));
        }
        let now = self.clock.now();
        if self.owner_taps
            && self.shows().is_none()
            && matches!(
                self.sim.phone.stage(),
                Stage::FirstbootDone | Stage::Provisioned
            )
            && now >= self.last_tap + 5_000
        {
            self.last_tap = now;
            self.feed(PhoneInput::AgentShow);
        }
        // Nothing yet: the pretend world moves on to the next thing due,
        // or by the wait, whichever comes first
        let end = self.clock.now() + wait_ms;
        let next = self
            .schedule
            .iter()
            .map(|(at, _)| *at)
            .chain(self.sim.phone.deadline())
            .filter(|t| *t <= end)
            .min()
            .unwrap_or(end);
        let now = self.clock.now();
        if next > now {
            self.clock.pass(next - now);
        }
        self.catch_up();
        if !self.connected {
            return Err(Self::lost());
        }
        Ok(self.to_host.pop_front())
    }

    fn disconnect(&mut self) {
        if self.connected {
            self.connected = false;
            self.to_host.clear();
            let _ = self.sim.feed(self.clock.now(), PhoneInput::LinkDown);
        }
    }
}

/// A shell for the pretend phone that keeps everything in memory: the
/// backup directory, the record, the screens it was given. For the tests
/// and the page's self-test.
pub struct MemoryShell {
    pub clock: Clock,
    pub rng: TestRng,
    /// The owner's factory image, as the file on the computer.
    pub factory: Vec<u8>,
    /// The backup directory: `NAME.img` by name.
    pub backup: std::collections::BTreeMap<String, Vec<u8>>,
    pub finished: Option<super::BackupFiles>,
    pub record: Option<super::LinkRecord>,
    pub screens: Vec<crate::flow::Screen>,
    pub progress: Vec<(&'static str, u64, u64)>,
    /// What the shell was asked to do, in order.
    pub log: Vec<String>,
    /// The front end leaves (the window is closed) once it was given a
    /// screen in this state.
    pub leave_at: Option<&'static str>,
    pub left: bool,
    /// The largest piece of the factory image read at once.
    pub largest_read: usize,
    /// The backup's last files cannot be written.
    pub finish_fails: bool,
    /// Turns of the run, and whether it was ended for taking too many.
    pub turns: u64,
    pub stuck: bool,
}

impl MemoryShell {
    pub fn new(clock: Clock, factory: Vec<u8>) -> MemoryShell {
        MemoryShell {
            clock,
            rng: TestRng(0x5eed),
            factory,
            backup: Default::default(),
            finished: None,
            record: None,
            screens: Vec::new(),
            progress: Vec::new(),
            log: Vec::new(),
            leave_at: None,
            left: false,
            largest_read: 0,
            finish_fails: false,
            turns: 0,
            stuck: false,
        }
    }

    /// The states of the screens given, each once in a row.
    pub fn states(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for s in &self.screens {
            if out.last() != Some(&s.state) {
                out.push(s.state);
            }
        }
        out
    }
}

impl LinkShell for MemoryShell {
    fn now_ms(&mut self) -> u64 {
        self.clock.now()
    }
    fn wall_clock_s(&mut self) -> u64 {
        // 2026-01-01, a made-up date
        1_767_225_600 + self.clock.now() / 1000
    }
    async fn sleep(&mut self, ms: u64) {
        self.clock.pass(ms);
    }
    fn random(&mut self, buf: &mut [u8]) {
        use omamobile_link::Entropy;
        self.rng.fill(buf);
    }
    fn go_on(&mut self) -> bool {
        self.turns += 1;
        if self.turns > 200_000 {
            // A test's run that never ends is a failure, not a hang
            self.left = true;
            self.stuck = true;
        }
        !self.left
    }
    fn screen(&mut self, screen: &crate::flow::Screen) {
        self.screens.push(screen.clone());
        if self.leave_at == Some(screen.state) {
            self.left = true;
        }
    }
    fn progress(&mut self, what: &'static str, done: u64, total: u64) {
        self.progress.push((what, done, total));
    }
    fn keep(&mut self, record: &super::LinkRecord) -> Result<()> {
        self.record = Some(record.clone());
        Ok(())
    }
    async fn backup_open(&mut self, name: &str, _size: u64) -> Result<u64> {
        Ok(self.backup.get(name).map_or(0, |f| f.len() as u64))
    }
    async fn backup_write(&mut self, name: &str, offset: u64, data: &[u8]) -> Result<()> {
        let f = self.backup.entry(name.to_string()).or_default();
        f.truncate(offset as usize);
        f.extend_from_slice(data);
        Ok(())
    }
    async fn backup_digest(&mut self, name: &str) -> Result<[u8; 32]> {
        self.backup
            .get(name)
            .map(|f| omamobile_link::digest::sha256(f))
            .ok_or_else(|| Error::Refused(format!("no {name}.img")))
    }
    async fn backup_discard(&mut self, name: &str) -> Result<()> {
        self.log.push(format!("backup-discard {name}"));
        self.backup.remove(name);
        Ok(())
    }
    async fn backup_finish(&mut self, files: &super::BackupFiles) -> Result<()> {
        if self.finish_fails {
            return Err(Error::Refused("the backup directory is full".into()));
        }
        self.log.push("backup-finish".into());
        self.finished = Some(files.clone());
        Ok(())
    }
    async fn factory_read(&mut self, offset: u64, len: u32) -> Result<Vec<u8>> {
        self.largest_read = self.largest_read.max(len as usize);
        self.factory
            .get(offset as usize..offset as usize + len as usize)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Error::Refused("the factory image is shorter".into()))
    }
}
