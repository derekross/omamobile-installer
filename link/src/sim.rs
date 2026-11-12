// SPDX-License-Identifier: MIT
//! Stand-ins for tests: the phone's shell with its world in memory.
//!
//! Behind the `sim` feature: a shipping build has none of it. Nothing here
//! opens a file or a device.
use crate::answers::Problem;
use crate::digest::sha256;
use crate::json::Value;
use crate::phone::{
    Effect, Facts, Note, Outcome, PartFound, Persistent, Phone, PhoneConfig, PhoneInput,
    PhoneOutput, Screen, Ticket,
};
use crate::Entropy;
use std::collections::VecDeque;

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
