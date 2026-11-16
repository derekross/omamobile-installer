// SPDX-License-Identifier: MIT
//! Stand-ins for tests: both shells with their worlds in memory, and a
//! loopback that carries frames between the two state machines and can
//! lose, repeat, swap or damage any of them, or cut the link.
//!
//! Behind the `sim` feature: a shipping build has none of it. Nothing here
//! opens a file or a device.
use crate::answers::Problem;
use crate::digest::sha256;
use crate::host::{
    Command, Event, Host, HostConfig, HostEffect, HostInput, HostOutcome, HostOutput,
};
use crate::json::Value;
use crate::msg::{BackupPart, JobState, Stage, StepResult, Token};
use crate::phone::{
    Account, AgentLine, Effect, Facts, FirstbootUnit, Found, Note, Outcome, PartFound, Persistent,
    Phone, PhoneConfig, PhoneInput, PhoneOutput, Screen, StateFile, Ticket,
};
use crate::Entropy;
use std::collections::{BTreeMap, VecDeque};

/// Numbers that look random and are the same every run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRng(pub u64);

impl TestRng {
    pub fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        self.fill(&mut v);
        v
    }
}

impl Entropy for TestRng {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let n = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&n[..chunk.len()]);
        }
    }
}

/// The phone's daemon as a test sees it: the machine, and a pretend phone
/// behind it, with the files a start of the daemon looks at. Every effect
/// is carried out at once unless `hold` is set.
#[derive(Clone)]
pub struct SimPhone {
    pub phone: Phone,
    pub cfg: PhoneConfig,
    pub rng: TestRng,
    /// Which boot this is: its number names it ([`SimPhone::boot_id`]).
    pub boot_no: u64,
    /// `/etc/omamobile/install-link`.
    pub marker: bool,
    /// `done`, with its word (and what is missing).
    pub done: Option<String>,
    /// The state file as last written, and whether it is there and readable.
    pub state_file: Persistent,
    pub state_exists: bool,
    pub state_unreadable: bool,
    /// The radio partitions: what the checks find, and their bytes.
    pub parts: Vec<(PartFound, Vec<u8>)>,
    /// Partitions that fail `backup-list`'s checks.
    pub precondition: Vec<String>,
    /// This many of the next partition reads return a damaged byte.
    pub bad_reads: u32,
    /// The factory image's file, and whether there is one.
    pub factory_file: Vec<u8>,
    pub factory_exists: bool,
    /// A byte to damage in the file after it is written, before the check.
    pub damage_factory: Option<u64>,
    pub free: u64,
    /// The working space `factory-begin` wants beside the image (§6.4).
    pub working_space: u64,
    pub facts: Facts,
    /// What first boot's `--check --porcelain` objects to.
    pub porcelain: Vec<Problem>,
    pub smoke: Value,
    /// The candidate file, and `firstboot.conf` (which first boot deletes
    /// when it is done), and the file first boot was last started with.
    pub candidate: Option<String>,
    pub conf: Option<String>,
    pub started_with: Option<String>,
    /// What first boot left: its `done`, the account, its unit in this boot.
    pub fb_done: bool,
    pub account: Account,
    pub fb_unit: FirstbootUnit,
    pub boot_update_conf: bool,
    /// The pending `modem-nv.sha256`.
    pub sums: Option<String>,
    /// The provisioning unit, its result file, its working directory.
    pub provision_unit: bool,
    pub provision_result: Option<Vec<StepResult>>,
    pub work_dir: bool,
    pub screens: Vec<Screen>,
    /// The lines sent to the session agent.
    pub agent: Vec<AgentLine>,
    pub notes: Vec<Note>,
    pub firstboot_starts: u32,
    pub provision_starts: u32,
    pub restarts: u32,
    /// `omamobile-link-gadget add` ran in this boot.
    pub offered: bool,
    /// The link was closed for this boot.
    pub closed: bool,
    pub finished: bool,
    pub stopped: bool,
    /// Keep the answers to effects with a ticket in `held` until [`SimPhone::release`].
    pub hold: bool,
    pub held: Vec<(Ticket, Outcome)>,
    /// Every effect, in order, for tests that look at the order.
    pub log: Vec<Effect>,
    /// The daemon is killed when it has carried out this many more outputs
    /// (effects and frames written): what is left of the list is never done.
    pub die_after: Option<u64>,
    pub dead: bool,
    /// Outputs carried out since the phone was made.
    pub carried: u64,
}

impl SimPhone {
    /// A phone whose files are what a link at `state` left, and the daemon
    /// started on it.
    pub fn new(
        cfg: PhoneConfig,
        state: Persistent,
        new_boot: bool,
        now: u64,
        seed: u64,
    ) -> SimPhone {
        let mut me = SimPhone::blank(cfg, seed);
        me.settle(&state);
        me.state_file = state;
        me.state_exists = true;
        if new_boot {
            // The file is from a boot before this one
            me.boot_no = 0;
        } else {
            me.state_file.boot_id = me.boot_id();
        }
        me.start(new_boot, now);
        me
    }

    /// A freshly installed phone at its first start: the marker and nothing
    /// else.
    pub fn first_start(cfg: PhoneConfig, now: u64, seed: u64) -> SimPhone {
        let mut me = SimPhone::blank(cfg, seed);
        me.boot_no = 0;
        me.start(true, now);
        me
    }

    /// The kernel's name for this boot.
    pub fn boot_id(&self) -> String {
        format!("boot-{}", self.boot_no)
    }

    /// The phone with no daemon started on it yet: for a test that arranges
    /// the files itself and then calls [`SimPhone::start`].
    pub fn blank(cfg: PhoneConfig, seed: u64) -> SimPhone {
        let mut rng = TestRng(seed);
        // A machine that has ended, until `start`: `done` is all it finds
        let found = Found {
            done: true,
            marker: false,
            ..Found::first_start("boot-1")
        };
        let (phone, _) = Phone::start(cfg.clone(), &found, 0, &mut rng);
        SimPhone {
            phone,
            cfg,
            rng,
            boot_no: 1,
            marker: true,
            done: None,
            // A state file written in this boot, for a test that says there
            // is one (`state_exists`) and changes what it needs
            state_file: Persistent {
                boot_id: "boot-1".into(),
                ..Persistent::default()
            },
            state_exists: false,
            state_unreadable: false,
            parts: Vec::new(),
            precondition: Vec::new(),
            bad_reads: 0,
            factory_file: Vec::new(),
            factory_exists: false,
            damage_factory: None,
            free: 100 << 30,
            working_space: 24 << 30,
            facts: Facts {
                release: "2026.01.01".into(),
                kernel: "7.0.0-test".into(),
                battery: 80,
                charging: true,
                free: 100 << 30,
                ..Facts::default()
            },
            porcelain: Vec::new(),
            smoke: Value::Obj(vec![("ok".into(), Value::Bool(true))]),
            candidate: None,
            conf: None,
            started_with: None,
            fb_done: false,
            account: Account::None,
            fb_unit: FirstbootUnit::NotRun,
            boot_update_conf: false,
            sums: None,
            provision_unit: false,
            provision_result: None,
            work_dir: false,
            screens: Vec::new(),
            agent: Vec::new(),
            notes: Vec::new(),
            firstboot_starts: 0,
            provision_starts: 0,
            restarts: 0,
            offered: false,
            closed: false,
            finished: false,
            stopped: false,
            hold: false,
            held: Vec::new(),
            log: Vec::new(),
            die_after: None,
            dead: false,
            carried: 0,
        }
    }

    /// Make the files those a link at `state` left behind.
    fn settle(&mut self, state: &Persistent) {
        match state.stage {
            Stage::Waiting => {}
            Stage::Firstboot | Stage::FirstbootFailed => {
                self.conf = Some(String::new());
                self.boot_update_conf = true;
                self.fb_unit = if state.stage == Stage::Firstboot {
                    FirstbootUnit::Running
                } else {
                    FirstbootUnit::Failed(state.firstboot_failed.clone())
                };
            }
            Stage::FirstbootDone | Stage::Provisioned | Stage::Finished => {
                self.fb_done = true;
                self.account = Account::Firstboot;
                self.boot_update_conf = true;
            }
        }
        if let Some(f) = &state.factory {
            self.factory_exists = true;
            self.factory_file.resize(f.synced as usize, 0);
        }
        if state.backup_done {
            self.sums = Some(String::new());
        }
        if state.provision == JobState::Running {
            self.provision_unit = true;
            self.work_dir = true;
        }
    }

    /// What a start of the daemon finds on this phone now.
    pub fn found(&self) -> Found {
        Found {
            boot_id: self.boot_id(),
            done: self.done.is_some(),
            marker: self.marker,
            state: if self.state_unreadable {
                StateFile::Unreadable
            } else if self.state_exists {
                // As the daemon reads it: from the file's bytes
                StateFile::read(Some(self.state_file.to_json().as_bytes()))
            } else {
                StateFile::Missing
            },
            firstboot_conf: self.conf.is_some(),
            firstboot_done: self.fb_done,
            account: self.account,
            firstboot_unit: self.fb_unit.clone(),
            boot_update_conf: self.boot_update_conf,
            image_len: self
                .factory_exists
                .then_some(self.factory_file.len() as u64),
            pending_sums: self.sums.is_some(),
            provision_unit: self.provision_unit,
            provision_result: self.provision_result.clone(),
            work_dir: self.work_dir,
            leftovers: self.candidate.is_some(),
        }
    }

    /// The daemon starts (again): a new machine from what is on the phone.
    /// In a new boot, first boot's unit has not run yet, and nothing of the
    /// boot before is in memory.
    pub fn start(&mut self, new_boot: bool, now: u64) {
        if new_boot {
            self.boot_no += 1;
            if self.fb_unit != FirstbootUnit::NotRun {
                self.fb_unit = FirstbootUnit::NotRun;
            }
            self.provision_unit = false;
            self.offered = false;
            self.closed = false;
        }
        self.dead = false;
        self.stopped = false;
        self.held.clear();
        let found = self.found();
        let (phone, out) = Phone::start(self.cfg.clone(), &found, now, &mut self.rng);
        self.phone = phone;
        let frames = self.carry_out(now, out);
        debug_assert!(frames.is_empty());
    }

    /// Give the machine one input; the frames it wants sent, in order.
    pub fn feed(&mut self, now: u64, input: PhoneInput<'_>) -> Vec<Vec<u8>> {
        // What first boot and the worker leave on the phone when they end,
        // whether the daemon is there to hear of it or not
        match &input {
            PhoneInput::FirstbootEnded { failed } if self.conf.is_some() => {
                self.account = Account::Firstboot;
                if failed.is_empty() {
                    self.conf = None;
                    self.fb_done = true;
                    self.fb_unit = FirstbootUnit::NotRun;
                } else {
                    self.fb_unit = FirstbootUnit::Failed(failed.clone());
                }
            }
            PhoneInput::ProvisionEnded { result } if self.provision_unit => {
                self.provision_unit = false;
                self.provision_result = result.clone();
                self.work_dir = false;
            }
            _ => {}
        }
        if self.dead {
            return Vec::new();
        }
        let out = self.phone.handle(now, input, &mut self.rng);
        self.carry_out(now, out)
    }

    /// Hand back the answers that were held.
    pub fn release(&mut self, now: u64) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for (t, o) in core::mem::take(&mut self.held) {
            frames.extend(self.feed(now, PhoneInput::Done(t, o)));
        }
        frames
    }

    fn carry_out(&mut self, now: u64, out: Vec<PhoneOutput>) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut queue: VecDeque<PhoneOutput> = out.into();
        while let Some(o) = queue.pop_front() {
            if let Some(n) = &mut self.die_after {
                if *n == 0 {
                    // Killed: the rest of the list is never carried out
                    self.dead = true;
                    self.die_after = None;
                    return frames;
                }
                *n -= 1;
            }
            self.carried += 1;
            let effect = match o {
                PhoneOutput::Send(f) => {
                    frames.push(f);
                    continue;
                }
                PhoneOutput::Do(e) => e,
            };
            self.log.push(effect.clone());
            let answer = match effect {
                Effect::Offer => {
                    self.offered = true;
                    None
                }
                Effect::Screen(s) => {
                    self.screens.push(s);
                    None
                }
                Effect::DropUnsent => None,
                Effect::Agent(line) => {
                    self.agent.push(line);
                    None
                }
                Effect::Note(n) => {
                    self.notes.push(n);
                    None
                }
                Effect::Persist(p) => {
                    self.state_file = p;
                    self.state_exists = true;
                    self.state_unreadable = false;
                    None
                }
                Effect::ReadFacts { ticket } => Some((ticket, Outcome::Facts(self.facts.clone()))),
                Effect::CheckAnswers { ticket, conf } => {
                    self.candidate = Some(conf);
                    Some((ticket, Outcome::Problems(self.porcelain.clone())))
                }
                Effect::StartFirstboot { ticket, retry } => {
                    if !retry {
                        self.boot_update_conf = true;
                        self.conf = self.candidate.take();
                    }
                    self.started_with = self.conf.clone();
                    self.fb_unit = FirstbootUnit::Running;
                    self.firstboot_starts += 1;
                    Some((ticket, Outcome::Done))
                }
                Effect::BootUpdateConf => {
                    self.boot_update_conf = true;
                    None
                }
                Effect::BackupList { ticket } => Some((
                    ticket,
                    if self.precondition.is_empty() {
                        Outcome::Parts(self.parts.iter().map(|p| p.0.clone()).collect())
                    } else {
                        Outcome::Precondition(self.precondition.clone())
                    },
                )),
                Effect::BackupRead {
                    ticket,
                    name,
                    offset,
                    len,
                } => {
                    let data = self.parts.iter().find(|p| p.0.name == name).and_then(|p| {
                        p.1.get(offset as usize..offset as usize + len as usize)
                            .map(<[u8]>::to_vec)
                    });
                    Some((
                        ticket,
                        match data {
                            Some(mut d) => {
                                if self.bad_reads > 0 {
                                    self.bad_reads -= 1;
                                    d[0] ^= 0x40;
                                }
                                Outcome::Data(d)
                            }
                            None => Outcome::Failed,
                        },
                    ))
                }
                Effect::BackupDigest { ticket, name } => Some((
                    ticket,
                    match self.parts.iter().find(|p| p.0.name == name) {
                        Some(p) => Outcome::Digest(sha256(&p.1)),
                        None => Outcome::Failed,
                    },
                )),
                Effect::BackupKeep { ticket, sums } => {
                    self.sums = Some(sums);
                    Some((ticket, Outcome::Done))
                }
                Effect::SumsDelete => {
                    self.sums = None;
                    None
                }
                Effect::FactoryOpen {
                    ticket,
                    size,
                    resume_at,
                } => {
                    let have = resume_at.min(self.factory_file.len() as u64);
                    let need = size - have + self.working_space;
                    Some((
                        ticket,
                        if need > self.free {
                            Outcome::NoSpace {
                                need,
                                free: self.free,
                            }
                        } else {
                            self.factory_exists = true;
                            self.factory_file.truncate(have as usize);
                            Outcome::FactoryOpened { have }
                        },
                    ))
                }
                Effect::FactoryWrite { offset, data, .. } => {
                    self.factory_file.truncate(offset as usize);
                    self.factory_file.extend_from_slice(&data);
                    None
                }
                Effect::FactoryTruncate { len } => {
                    self.factory_file.truncate(len as usize);
                    None
                }
                Effect::FactoryDigest { ticket } => {
                    if let Some(at) = self.damage_factory.take() {
                        if let Some(b) = self.factory_file.get_mut(at as usize) {
                            *b ^= 1;
                        }
                    }
                    Some((ticket, Outcome::Digest(sha256(&self.factory_file))))
                }
                Effect::FactoryDelete => {
                    self.factory_file.clear();
                    self.factory_exists = false;
                    None
                }
                Effect::StartProvision => {
                    self.provision_starts += 1;
                    self.provision_unit = true;
                    self.provision_result = None;
                    self.work_dir = true;
                    None
                }
                Effect::ProvisionWatch => None,
                Effect::WorkDirRemove => {
                    self.work_dir = false;
                    None
                }
                Effect::Sweep => {
                    self.candidate = None;
                    None
                }
                Effect::Smoke { ticket } => Some((ticket, Outcome::Report(self.smoke.clone()))),
                Effect::Restart => {
                    self.restarts += 1;
                    None
                }
                Effect::Close => {
                    self.closed = true;
                    self.offered = false;
                    None
                }
                Effect::Finish => {
                    self.finished = true;
                    self.marker = false;
                    self.state_exists = false;
                    self.done = Some("finished".into());
                    self.offered = false;
                    None
                }
                Effect::Expire { missing } => {
                    self.marker = false;
                    self.factory_file.clear();
                    self.factory_exists = false;
                    self.work_dir = false;
                    let mut word = String::from("expired");
                    for m in missing {
                        word.push(' ');
                        word.push_str(m.as_str());
                    }
                    self.done = Some(word);
                    None
                }
                Effect::Refuse { reason } => {
                    self.marker = false;
                    self.factory_file.clear();
                    self.factory_exists = false;
                    self.work_dir = false;
                    self.done = Some(reason.as_str().into());
                    None
                }
                Effect::DeleteMarker => {
                    self.marker = false;
                    None
                }
                Effect::Stop => {
                    self.stopped = true;
                    None
                }
            };
            if let Some((ticket, outcome)) = answer {
                if self.hold {
                    self.held.push((ticket, outcome));
                } else {
                    // The answer's own outputs come after what is queued
                    // already: order is kept.
                    let more =
                        self.phone
                            .handle(now, PhoneInput::Done(ticket, outcome), &mut self.rng);
                    queue.extend(more);
                }
            }
        }
        frames
    }
}

/// What a start of the daemon finds on a phone whose files are those a
/// link at `state` left behind: nothing for the start-up rules to square.
/// `new_boot`: in the boot after the one the file was written in.
pub fn found_after(state: &Persistent, new_boot: bool) -> Found {
    let mut p = SimPhone::blank(PhoneConfig::new("0", "sim", b""), 0);
    p.settle(state);
    p.state_file = state.clone();
    p.state_exists = true;
    if new_boot {
        p.fb_unit = FirstbootUnit::NotRun;
        p.provision_unit = false;
    }
    let mut found = p.found();
    found.boot_id = if new_boot {
        format!("{}+", state.boot_id)
    } else {
        state.boot_id.clone()
    };
    found
}

/// The installer's shell as a test sees it.
pub struct SimHost {
    pub host: Host,
    /// Where the nonces of its `hello`s come from.
    pub rng: TestRng,
    /// The backup directory: `NAME.img` by name.
    pub backup: BTreeMap<String, Vec<u8>>,
    /// What `BackupFinish` wrote down.
    pub backup_finished: Option<Vec<(BackupPart, [u8; 32])>>,
    /// The factory image the download step verified.
    pub factory: Vec<u8>,
    /// The work directory's token.
    pub token: Option<Token>,
    pub events: Vec<Event>,
    /// Frames written to the link, and how many it took to be paired.
    pub sent: u64,
    pub paired_after: Option<u64>,
}

impl SimHost {
    pub fn new(cfg: HostConfig) -> SimHost {
        SimHost::with_seed(cfg, 0x4057)
    }

    /// Another run of the installer draws other nonces.
    pub fn with_seed(cfg: HostConfig, seed: u64) -> SimHost {
        SimHost {
            token: cfg.token,
            rng: TestRng(seed),
            host: Host::new(cfg),
            backup: BTreeMap::new(),
            backup_finished: None,
            factory: Vec::new(),
            events: Vec::new(),
            sent: 0,
            paired_after: None,
        }
    }

    pub fn feed(&mut self, now: u64, input: HostInput<'_>) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut queue: VecDeque<HostOutput> = self.host.handle(now, input, &mut self.rng).into();
        while let Some(o) = queue.pop_front() {
            let answer = match o {
                HostOutput::Send(f) => {
                    frames.push(f);
                    self.sent += 1;
                    None
                }
                HostOutput::Event(e) => {
                    if matches!(e, Event::Paired { .. }) && self.paired_after.is_none() {
                        self.paired_after = Some(self.sent);
                    }
                    self.events.push(e);
                    None
                }
                HostOutput::Do(HostEffect::RecordToken(t)) => {
                    self.token = Some(t);
                    None
                }
                HostOutput::Do(HostEffect::ForgetToken) => {
                    self.token = None;
                    None
                }
                HostOutput::Do(HostEffect::BackupOpen { ticket, name, .. }) => Some((
                    ticket,
                    HostOutcome::Have(self.backup.get(&name).map_or(0, |f| f.len() as u64)),
                )),
                HostOutput::Do(HostEffect::BackupWrite { name, offset, data }) => {
                    let f = self.backup.entry(name).or_default();
                    f.truncate(offset as usize);
                    f.extend_from_slice(&data);
                    None
                }
                HostOutput::Do(HostEffect::BackupDigest { ticket, name }) => Some((
                    ticket,
                    match self.backup.get(&name) {
                        Some(f) => HostOutcome::Digest(sha256(f)),
                        None => HostOutcome::Failed,
                    },
                )),
                HostOutput::Do(HostEffect::BackupDiscard { name }) => {
                    self.backup.remove(&name);
                    None
                }
                HostOutput::Do(HostEffect::BackupFinish { ticket, parts }) => {
                    self.backup_finished = Some(parts);
                    Some((ticket, HostOutcome::Done))
                }
                HostOutput::Do(HostEffect::FactoryRead {
                    ticket,
                    offset,
                    len,
                }) => Some((
                    ticket,
                    match self
                        .factory
                        .get(offset as usize..offset as usize + len as usize)
                    {
                        Some(d) => HostOutcome::Data(d.to_vec()),
                        None => HostOutcome::Failed,
                    },
                )),
            };
            if let Some((ticket, outcome)) = answer {
                let done = HostInput::Done(ticket, outcome);
                queue.extend(self.host.handle(now, done, &mut self.rng));
            }
        }
        frames
    }

    /// The events since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }
}

/// Which way a frame goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    ToPhone,
    ToHost,
}

/// What becomes of a frame on the loopback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    Deliver,
    /// It never arrives.
    Drop,
    /// It arrives twice.
    Duplicate,
    /// It arrives after the next frame in its direction.
    Swap,
    /// The link goes down instead: this frame and all under way are lost.
    Cut,
}

/// Decides a frame's fate: the direction, the frame's number in that
/// direction since the loopback was made, and its bytes, which it may
/// change (cut short, flip a bit).
pub type Fault = Box<dyn FnMut(Dir, u64, &mut Vec<u8>) -> Fate>;

/// The two machines joined in memory.
pub struct Loopback {
    pub phone: SimPhone,
    pub host: SimHost,
    pub now: u64,
    pub up: bool,
    pub fault: Option<Fault>,
    /// False: the host does not read. What the phone sends waits, in order,
    /// until [`Loopback::host_reads_again`]. (Real USB would also hold the
    /// phone's writes back; this loopback cannot, see docs/LINK.md.)
    pub host_reads: bool,
    /// The frames that wait for a host that does not read.
    pub unread: VecDeque<Vec<u8>>,
    /// Stop carrying frames when this many have been delivered in all,
    /// with the rest left under way: the moment to replace a machine.
    pub pause_at: Option<u64>,
    queue: VecDeque<(Dir, Vec<u8>)>,
    swapped: [Option<Vec<u8>>; 2],
    count: [u64; 2],
    /// Frames delivered, by direction.
    pub delivered: [u64; 2],
}

impl Loopback {
    pub fn new(phone: SimPhone, host: SimHost) -> Loopback {
        Loopback {
            phone,
            host,
            now: 0,
            up: false,
            fault: None,
            host_reads: true,
            unread: VecDeque::new(),
            pause_at: None,
            queue: VecDeque::new(),
            swapped: [None, None],
            count: [0, 0],
            delivered: [0, 0],
        }
    }

    /// The cable goes in: the phone's function is enabled, the host finds it.
    pub fn connect(&mut self) {
        self.up = true;
        let f = self.phone.feed(self.now, PhoneInput::LinkUp);
        self.push(Dir::ToHost, f);
        let f = self.host.feed(self.now, HostInput::LinkUp);
        self.push(Dir::ToPhone, f);
        self.run();
    }

    /// The cable comes out: whatever was under way is lost.
    pub fn cut(&mut self) {
        self.up = false;
        self.queue.clear();
        self.unread.clear();
        self.swapped = [None, None];
        let _ = self.phone.feed(self.now, PhoneInput::LinkDown);
        let _ = self.host.feed(self.now, HostInput::LinkDown);
    }

    fn push(&mut self, dir: Dir, frames: Vec<Vec<u8>>) {
        if !self.up {
            return;
        }
        if dir == Dir::ToHost && !self.host_reads {
            self.unread.extend(frames);
        } else {
            self.queue.extend(frames.into_iter().map(|f| (dir, f)));
        }
    }

    /// The host reads again: what waited arrives, in order.
    pub fn host_reads_again(&mut self) {
        self.host_reads = true;
        let waited: Vec<Vec<u8>> = self.unread.drain(..).collect();
        self.push(Dir::ToHost, waited);
        self.run();
    }

    /// The installer was killed and is run again, with the cable untouched:
    /// a new machine in the old one's place, the link left up, and whatever
    /// was under way still under way.
    pub fn rerun_host(&mut self, host: SimHost) {
        self.host = host;
        if self.up {
            let f = self.host.feed(self.now, HostInput::LinkUp);
            self.push(Dir::ToPhone, f);
        }
    }

    /// One frame arrives at the phone that no machine here sent: what
    /// another run of the installer left under way.
    pub fn to_phone(&mut self, bytes: &[u8]) {
        let f = self.phone.feed(self.now, PhoneInput::Frame(bytes));
        self.push(Dir::ToHost, f);
        self.run();
    }

    /// Frames under way, both directions.
    pub fn under_way(&self) -> usize {
        self.queue.len()
    }

    /// Carry frames until none is under way.
    pub fn run(&mut self) {
        let mut guard = 0u32;
        loop {
            if self
                .pause_at
                .is_some_and(|n| self.delivered[0] + self.delivered[1] >= n)
            {
                return;
            }
            let Some((dir, mut bytes)) = self.queue.pop_front() else {
                return;
            };
            guard += 1;
            assert!(guard < 1_000_000, "the two machines never stop talking");
            let i = dir as usize;
            let n = self.count[i];
            self.count[i] += 1;
            let fate = match &mut self.fault {
                Some(f) => f(dir, n, &mut bytes),
                None => Fate::Deliver,
            };
            let mut arrive = Vec::new();
            match fate {
                Fate::Deliver => arrive.push(bytes),
                Fate::Drop => {}
                Fate::Duplicate => {
                    arrive.push(bytes.clone());
                    arrive.push(bytes);
                }
                Fate::Swap => {
                    self.swapped[i] = Some(bytes);
                    continue;
                }
                Fate::Cut => {
                    self.cut();
                    return;
                }
            }
            if !arrive.is_empty() {
                arrive.extend(self.swapped[i].take());
            }
            for b in arrive {
                if !self.up {
                    return;
                }
                self.delivered[i] += 1;
                match dir {
                    Dir::ToPhone => {
                        let f = self.phone.feed(self.now, PhoneInput::Frame(&b));
                        self.push(Dir::ToHost, f);
                    }
                    Dir::ToHost => {
                        let f = self.host.feed(self.now, HostInput::Frame(&b));
                        self.push(Dir::ToPhone, f);
                    }
                }
            }
        }
    }

    /// The flow asks for a step.
    pub fn command(&mut self, cmd: Command) {
        let f = self.host.feed(self.now, HostInput::Command(cmd));
        self.push(Dir::ToPhone, f);
        self.run();
    }

    /// Something happened on the phone (first boot ended, provisioning ended).
    pub fn phone_input(&mut self, input: PhoneInput<'_>) {
        let f = self.phone.feed(self.now, input);
        self.push(Dir::ToHost, f);
        self.run();
    }

    /// Hand back the phone's held answers.
    pub fn release(&mut self) {
        let f = self.phone.release(self.now);
        self.push(Dir::ToHost, f);
        self.run();
    }

    /// Let `ms` pass, with every deadline on the way kept.
    pub fn advance(&mut self, ms: u64) {
        let end = self.now + ms;
        loop {
            let next = [self.phone.phone.deadline(), self.host.host.deadline()]
                .into_iter()
                .flatten()
                .filter(|t| *t <= end)
                .min();
            let Some(t) = next else { break };
            self.now = self.now.max(t);
            let before = (self.phone.phone.deadline(), self.host.host.deadline());
            let f = self.phone.feed(self.now, PhoneInput::Tick);
            self.push(Dir::ToHost, f);
            let f = self.host.feed(self.now, HostInput::Tick);
            self.push(Dir::ToPhone, f);
            self.run();
            if before == (self.phone.phone.deadline(), self.host.host.deadline()) {
                // A deadline that a tick does not move: step past it
                self.now += 1;
                if self.now > end {
                    break;
                }
            }
        }
        self.now = end;
    }

    /// Does the host's event list hold one that `f` takes?
    pub fn saw(&self, f: impl Fn(&Event) -> bool) -> bool {
        self.host.events.iter().any(f)
    }
}
