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
use crate::msg::{BackupPart, Token};
use crate::phone::{
    Effect, Facts, Note, Outcome, PartFound, Persistent, Phone, PhoneConfig, PhoneInput,
    PhoneOutput, Screen, Ticket,
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
/// behind it. Every effect is carried out at once unless `hold` is set.
pub struct SimPhone {
    pub phone: Phone,
    pub rng: TestRng,
    /// The state file as last written.
    pub state_file: Persistent,
    /// The radio partitions: what the checks find, and their bytes.
    pub parts: Vec<(PartFound, Vec<u8>)>,
    /// Partitions that fail `backup-list`'s checks.
    pub precondition: Vec<String>,
    /// This many of the next partition reads return a damaged byte.
    pub bad_reads: u32,
    /// The factory image's file.
    pub factory_file: Vec<u8>,
    /// A byte to damage in the file after it is written, before the check.
    pub damage_factory: Option<u64>,
    pub free: u64,
    /// The working space `factory-begin` wants beside the image (§6.4).
    pub working_space: u64,
    pub facts: Facts,
    /// What first boot's `--check --porcelain` objects to.
    pub porcelain: Vec<Problem>,
    pub smoke: Value,
    /// The candidate file, and the file first boot was started with.
    pub candidate: Option<String>,
    pub conf: Option<String>,
    /// The pending `modem-nv.sha256`.
    pub sums: Option<String>,
    pub screens: Vec<Screen>,
    pub notes: Vec<Note>,
    pub firstboot_starts: u32,
    pub provision_starts: u32,
    pub restarts: u32,
    pub finished: bool,
    pub stopped: bool,
    /// Keep the answers to effects with a ticket in `held` until [`SimPhone::release`].
    pub hold: bool,
    pub held: Vec<(Ticket, Outcome)>,
    /// Every effect, in order, for tests that look at the order.
    pub log: Vec<Effect>,
}

impl SimPhone {
    pub fn new(
        cfg: PhoneConfig,
        state: Persistent,
        new_boot: bool,
        now: u64,
        seed: u64,
    ) -> SimPhone {
        let mut rng = TestRng(seed);
        let (phone, out) = Phone::start(cfg, state.clone(), new_boot, now, &mut rng);
        let mut me = SimPhone {
            phone,
            rng,
            state_file: state,
            parts: Vec::new(),
            precondition: Vec::new(),
            bad_reads: 0,
            factory_file: Vec::new(),
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
            sums: None,
            screens: Vec::new(),
            notes: Vec::new(),
            firstboot_starts: 0,
            provision_starts: 0,
            restarts: 0,
            finished: false,
            stopped: false,
            hold: false,
            held: Vec::new(),
            log: Vec::new(),
        };
        let frames = me.carry_out(now, out);
        debug_assert!(frames.is_empty());
        me
    }

    /// Give the machine one input; the frames it wants sent, in order.
    pub fn feed(&mut self, now: u64, input: PhoneInput<'_>) -> Vec<Vec<u8>> {
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
            let effect = match o {
                PhoneOutput::Send(f) => {
                    frames.push(f);
                    continue;
                }
                PhoneOutput::Do(e) => e,
            };
            self.log.push(effect.clone());
            let answer = match effect {
                Effect::Screen(s) => {
                    self.screens.push(s);
                    None
                }
                Effect::Note(n) => {
                    self.notes.push(n);
                    None
                }
                Effect::Persist(p) => {
                    self.state_file = p;
                    None
                }
                Effect::ReadFacts { ticket } => Some((ticket, Outcome::Facts(self.facts.clone()))),
                Effect::CheckAnswers { ticket, conf } => {
                    self.candidate = Some(conf);
                    Some((ticket, Outcome::Problems(self.porcelain.clone())))
                }
                Effect::StartFirstboot { ticket, retry } => {
                    if !retry {
                        self.conf = self.candidate.take();
                    }
                    self.firstboot_starts += 1;
                    Some((ticket, Outcome::Done))
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
                    None
                }
                Effect::StartProvision => {
                    self.provision_starts += 1;
                    None
                }
                Effect::Smoke { ticket } => Some((ticket, Outcome::Report(self.smoke.clone()))),
                Effect::Restart => {
                    self.restarts += 1;
                    None
                }
                Effect::Finish => {
                    self.finished = true;
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

/// The installer's shell as a test sees it.
pub struct SimHost {
    pub host: Host,
    /// The backup directory: `NAME.img` by name.
    pub backup: BTreeMap<String, Vec<u8>>,
    /// What `BackupFinish` wrote down.
    pub backup_finished: Option<Vec<(BackupPart, [u8; 32])>>,
    /// The factory image the download step verified.
    pub factory: Vec<u8>,
    /// The work directory's token.
    pub token: Option<Token>,
    pub events: Vec<Event>,
}

impl SimHost {
    pub fn new(cfg: HostConfig) -> SimHost {
        SimHost {
            token: cfg.token,
            host: Host::new(cfg),
            backup: BTreeMap::new(),
            backup_finished: None,
            factory: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn feed(&mut self, now: u64, input: HostInput<'_>) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut queue: VecDeque<HostOutput> = self.host.handle(now, input).into();
        while let Some(o) = queue.pop_front() {
            let answer = match o {
                HostOutput::Send(f) => {
                    frames.push(f);
                    None
                }
                HostOutput::Event(e) => {
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
                HostOutput::Do(HostEffect::BackupFinish { parts }) => {
                    self.backup_finished = Some(parts);
                    None
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
                queue.extend(self.host.handle(now, HostInput::Done(ticket, outcome)));
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
        self.swapped = [None, None];
        let _ = self.phone.feed(self.now, PhoneInput::LinkDown);
        let _ = self.host.feed(self.now, HostInput::LinkDown);
    }

    fn push(&mut self, dir: Dir, frames: Vec<Vec<u8>>) {
        if self.up {
            self.queue.extend(frames.into_iter().map(|f| (dir, f)));
        }
    }

    /// Carry frames until none is under way.
    pub fn run(&mut self) {
        let mut guard = 0u32;
        while let Some((dir, mut bytes)) = self.queue.pop_front() {
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
