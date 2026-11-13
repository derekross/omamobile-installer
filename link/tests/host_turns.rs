// SPDX-License-Identifier: MIT
//! The computer's side driven by hand against a phone that does not keep to
//! the protocol: every message of the phone's in every state of the host,
//! with the right `re`, a wrong one and none. The host takes an answer only
//! where it asked the question; anything else ends the session, and a phone
//! that keeps at it is given up on.
mod common;
use common::rig::*;
use common::*;
use omamobile_link::digest::serial_proof;
use omamobile_link::frame::{self, Kind};
use omamobile_link::host::*;
use omamobile_link::msg::*;
use omamobile_link::sim::TestRng;

/// A phone written out by hand.
#[derive(Clone)]
struct Fake {
    host: Host,
    rng: TestRng,
    /// The nonce of the host's last `hello`.
    host_nonce: [u8; 32],
    /// The next frame number the fake phone sends.
    seq: u32,
    /// The id of the host's last message.
    last_id: u64,
    last_t: &'static str,
    events: Vec<Event>,
    effects: Vec<HostEffect>,
    sent: Vec<&'static str>,
    now: u64,
}

const NONCE: [u8; 32] = [3; 32];
const MAX: u32 = omamobile_link::frame::DEFAULT_MAX_DATA;

impl Fake {
    fn new() -> Fake {
        let cfg = host_config();
        Fake {
            host: Host::new(cfg),
            rng: TestRng(77),
            host_nonce: [0; 32],
            seq: 0,
            last_id: 0,
            last_t: "",
            events: vec![],
            effects: vec![],
            sent: vec![],
            now: 0,
        }
    }
    fn take(&mut self, out: Vec<HostOutput>) {
        for o in out {
            match o {
                HostOutput::Send(bytes) => {
                    let f = frame::decode(&bytes, MAX).expect("the host sends valid frames");
                    if f.kind == Kind::Message {
                        let m = HostFrame::parse(f.payload).expect("the host sends valid messages");
                        self.last_id = m.id;
                        self.last_t = m.msg.t();
                        self.sent.push(m.msg.t());
                        if let HostMsg::Hello(h) = &m.msg {
                            assert_eq!(f.seq, 0, "hello is frame 0");
                            assert_ne!(h.nonce, self.host_nonce, "a nonce drawn anew");
                            self.host_nonce = h.nonce;
                            self.seq = 0;
                        }
                    } else {
                        self.sent.push("data");
                    }
                }
                HostOutput::Do(e) => self.effects.push(e),
                HostOutput::Event(e) => self.events.push(e),
            }
        }
    }
    fn input(&mut self, i: HostInput<'_>) {
        let out = self.host.handle(self.now, i, &mut self.rng);
        self.take(out);
    }
    fn say(&mut self, re: Option<u64>, msg: PhoneMsg) {
        let json = PhoneFrame { re, msg }.to_json().unwrap();
        let bytes = frame::encode(Kind::Message, self.seq, 0, json.as_bytes(), MAX).unwrap();
        self.seq += 1;
        self.input(HostInput::Frame(&bytes));
    }
    fn answer(&mut self, msg: PhoneMsg) {
        self.say(Some(self.last_id), msg);
    }
    fn hello_msg(&self, pair: PairMode, wait_s: u32) -> PhoneMsg {
        PhoneMsg::Hello(PhoneHello {
            proto: 1,
            link: "0.0.1".into(),
            device: "cheetah".into(),
            max_data: 1024,
            nonce: NONCE,
            host_nonce: self.host_nonce,
            stage: Stage::FirstbootDone,
            pair,
            wait_s,
        })
    }
    fn hello(&mut self, pair: PairMode) {
        let h = self.hello_msg(pair, 0);
        self.answer(h);
    }
    fn proof(&self) -> [u8; 32] {
        serial_proof(&self.host_nonce, &NONCE, SERIAL)
    }
    fn hellos(&self) -> usize {
        self.sent.iter().filter(|t| **t == "hello").count()
    }
    fn ticket(&self) -> Ticket {
        match self.effects.last() {
            Some(
                HostEffect::BackupOpen { ticket, .. }
                | HostEffect::BackupDigest { ticket, .. }
                | HostEffect::BackupFinish { ticket, .. }
                | HostEffect::FactoryRead { ticket, .. },
            ) => *ticket,
            other => panic!("no ticket: {other:?}"),
        }
    }
}

const STATES: [&str; 10] = [
    "hello sent",
    "the code is wanted",
    "pair sent",
    "paired, idle",
    "status sent",
    "backup-list sent",
    "a partition is streaming",
    "the image is being read",
    "factory-end sent",
    "provisioning",
];

fn build(state: &str) -> Fake {
    let mut f = Fake::new();
    f.input(HostInput::LinkUp);
    assert_eq!(f.last_t, "hello");
    if state == "hello sent" {
        return f;
    }
    f.hello(PairMode::Code);
    assert!(f.events.contains(&Event::NeedCode));
    f.events.clear();
    if state == "the code is wanted" {
        return f;
    }
    f.input(HostInput::Command(Command::Pair(
        Code::parse("123456").unwrap(),
    )));
    assert_eq!(f.last_t, "pair");
    if state == "pair sent" {
        return f;
    }
    f.answer(PhoneMsg::Paired {
        token: Some(Token([5; 32])),
        serial_proof: f.proof(),
    });
    assert!(f.host.ready());
    let parts = vec![BackupPart {
        name: "efs".into(),
        size: 3000,
        checked: true,
    }];
    match state {
        "paired, idle" => {}
        "status sent" => f.input(HostInput::Command(Command::Status)),
        "backup-list sent" => f.input(HostInput::Command(Command::Backup)),
        "a partition is streaming" => {
            f.input(HostInput::Command(Command::Backup));
            f.answer(PhoneMsg::BackupParts { parts });
            let t = f.ticket();
            f.input(HostInput::Done(t, HostOutcome::Have(0)));
            assert_eq!(f.last_t, "backup-read");
            f.answer(PhoneMsg::DataBegin {
                name: "efs".into(),
                size: 3000,
                offset: 0,
            });
        }
        "the image is being read" | "factory-end sent" => {
            let size = if state == "factory-end sent" {
                1000
            } else {
                9000
            };
            let mut b = factory_begin(&image()[..size]);
            b.size = size as u64;
            f.input(HostInput::Command(Command::Factory(b)));
            f.answer(PhoneMsg::FactoryReady {
                offset: 0,
                ack_every: 4096,
                window: 8192,
            });
            if state == "factory-end sent" {
                let t = f.ticket();
                f.input(HostInput::Done(
                    t,
                    HostOutcome::Data(image()[..1000].to_vec()),
                ));
                assert_eq!(f.last_t, "factory-end");
            }
        }
        "provisioning" => {
            f.input(HostInput::Command(Command::Provision));
            assert_eq!(f.last_t, "status");
            f.answer(PhoneMsg::Status(Box::new(status())));
            assert_eq!(f.last_t, "provision");
            f.answer(PhoneMsg::ProvisionStarted);
        }
        other => panic!("{other}"),
    }
    f.events.clear();
    f
}

/// The one answer each state waits for, and what may come by itself in it.
fn in_turn(state: &str, t: &str, re: &str) -> bool {
    // These come by themselves in any paired session (§5.2)
    if re == "none"
        && matches!(
            t,
            "progress"
                | "provision-done"
                | "error-provision-failed"
                | "error-protocol"
                | "error-internal"
        )
    {
        return true;
    }
    if re != "right" {
        // A data frame of ours that met no open stream (§5.1): the image is
        // begun again, or the answer to `factory-end` follows
        return matches!(
            (state, t, re),
            (
                "the image is being read" | "factory-end sent",
                "error-not-allowed",
                "none"
            )
        );
    }
    match state {
        "pair sent" => matches!(
            t,
            "paired" | "error-bad-code" | "error-locked" | "error-not-allowed"
        ),
        "status sent" => t == "status" || t == "status-provisioned" || t.starts_with("error-"),
        "backup-list sent" => t == "backup-parts" || t.starts_with("error-"),
        "factory-end sent" => t == "factory-verified" || t.starts_with("error-"),
        _ => false,
    }
}

#[test]
fn every_phone_message_in_every_host_state() {
    let mut n = 0;
    for state in STATES {
        for (name, sample) in phone_samples() {
            for re in ["right", "wrong", "none"] {
                let rule = PhoneMsg::re_rule(sample.msg.t());
                let re_value = |f: &Fake| match re {
                    "right" => Some(f.last_id),
                    "wrong" => Some(f.last_id + 7),
                    _ => None,
                };
                // Only what the reader takes can be said at all
                if (re == "none") == (rule == Re::Reply) || (re != "none" && rule == Re::Event) {
                    continue;
                }
                let mut f = build(state);
                let hellos = f.hellos();
                let was_busy = f.host.busy();
                let re_v = re_value(&f);
                f.say(re_v, sample.msg.clone());
                n += 1;
                let lost = f.events.contains(&Event::LinkLost) || f.host.stopped();
                let again = f.hellos() > hellos;
                let name = name.as_str();
                if state == "hello sent" {
                    // Until the host's own nonce comes back, whatever
                    // arrives is from another run or a session that is
                    // over: passed over in silence, whatever its `re`. (The
                    // samples carry a nonce that is not this host's.)
                    assert!(
                        f.events.is_empty() && !again && f.host.discarded() == 1,
                        "{name} ({re} re) in \"{state}\": {:?}",
                        f.events
                    );
                    continue;
                }
                if in_turn(state, name, re) {
                    // Taken: the session goes on, unless what was said ends it
                    let ends = matches!(
                        name,
                        "error-protocol" | "error-internal" | "error-not-paired" | "error-version"
                    ) || (state == "pair sent" && name == "paired"); // another phone's proof
                    assert_eq!(
                        lost, ends,
                        "{name} ({re} re) in \"{state}\": {:?}",
                        f.events
                    );
                    continue;
                }
                // Out of turn: the session is over, a new hello goes out,
                // and nothing was taken from it
                assert!(
                    lost && again,
                    "{name} ({re} re) in \"{state}\": {:?} {:?}",
                    f.events,
                    f.sent
                );
                let good: Vec<&Event> = f
                    .events
                    .iter()
                    .filter(|e| {
                        !matches!(
                            e,
                            Event::LinkLost | Event::Stopped(_) | Event::Refused { .. }
                        )
                    })
                    .collect();
                assert!(good.is_empty(), "{name} ({re} re) in \"{state}\": {good:?}");
                assert_eq!(
                    f.host.busy(),
                    was_busy && !matches!(state, "status sent"),
                    "the step waits for the next session"
                );
            }
        }
    }
    assert!(n > 500, "{n}");
}

#[test]
fn only_the_frame_that_echoes_the_nonce_answers_hello() {
    // What answers this `hello` is known by its nonce, not by `re` or `seq`
    for pair in [
        PairMode::Code,
        PairMode::Ask,
        PairMode::Token,
        PairMode::Locked,
    ] {
        for re in [Some(1u64), Some(99), None] {
            let mut f = build("hello sent");
            let Some(re) = re else {
                // A hello with no `re` is not a message the reader takes
                let json = PhoneFrame {
                    re: Some(1),
                    msg: f.hello_msg(pair, 0),
                }
                .to_json()
                .unwrap()
                .replace("\"re\":1,", "");
                let b = frame::encode(Kind::Message, 0, 0, json.as_bytes(), MAX).unwrap();
                f.input(HostInput::Frame(&b));
                assert_eq!((f.host.discarded(), f.events.len()), (1, 0));
                continue;
            };
            let h = f.hello_msg(pair, 0);
            f.say(Some(re), h);
            assert!(
                matches!(f.events.first(), Some(Event::Hello { .. })),
                "{pair:?} re {re}: {:?}",
                f.events
            );
            assert_eq!(f.host.discarded(), 0);
        }
    }
    // Not frame 0: not the answer
    let mut f = build("hello sent");
    f.seq = 5;
    let h = f.hello_msg(PairMode::Code, 0);
    f.answer(h);
    assert_eq!((f.host.discarded(), f.events.len()), (1, 0));
    // A version error is known the same way
    let version = |nonce: [u8; 32]| {
        PhoneMsg::Error(ErrorMsg::new(
            ErrorCode::Version {
                proto: vec![2],
                host_nonce: nonce,
            },
            Stage::Waiting,
        ))
    };
    let mut f = build("hello sent");
    let stale = version([9; 32]);
    f.answer(stale);
    assert!(!f.host.stopped() && f.host.discarded() == 1);
    f.seq = 0;
    let mine = version(f.host_nonce);
    f.answer(mine);
    assert_eq!(
        f.events,
        vec![Event::Stopped(Stop::Version { phone: vec![2] })]
    );
    // Noise, a damaged frame and a data frame are passed over too, and the
    // answer is still taken after them
    let mut f = build("hello sent");
    f.input(HostInput::Frame(b"noise"));
    let d = frame::encode(Kind::Data, 3, 0, &[1; 100], MAX).unwrap();
    f.input(HostInput::Frame(&d));
    let mut bad = frame::encode(Kind::Message, 0, 0, b"{}", MAX).unwrap();
    bad[30] ^= 1;
    f.input(HostInput::Frame(&bad));
    assert_eq!((f.host.discarded(), f.events.len(), f.hellos()), (3, 0, 1));
    f.seq = 0;
    f.hello(PairMode::Code);
    assert!(f.events.contains(&Event::NeedCode));
}

#[test]
fn a_phone_that_keeps_breaking_the_protocol_is_given_up_on() {
    let mut f = build("paired, idle");
    for strike in 1..=3 {
        // An answer to nothing, then an answer that `resume` does not have
        f.say(Some(f.last_id), PhoneMsg::Pong);
        assert_eq!(f.host.stopped(), strike == 3, "strike {strike}");
        if strike < 3 {
            assert_eq!(f.last_t, "hello");
            f.hello(PairMode::Code);
            assert_eq!(f.last_t, "resume", "the token pairs again");
        }
    }
    assert!(f.events.contains(&Event::Stopped(Stop::Protocol)));
    // Stopped is stopped
    let sent = f.sent.len();
    f.input(HostInput::LinkUp);
    f.input(HostInput::Command(Command::Status));
    assert_eq!(f.sent.len(), sent);
}

#[test]
fn frames_that_are_wrong_end_the_session_too() {
    for what in [
        "noise",
        "seq",
        "data out of place",
        "data past the end",
        "data-begin for another partition",
    ] {
        let mut f = build("a partition is streaming");
        let bytes = match what {
            "noise" => b"not a frame".to_vec(),
            "seq" => frame::encode(Kind::Data, f.seq + 1, 0, &[1; 1024], MAX).unwrap(),
            "data out of place" => frame::encode(Kind::Data, f.seq, 1024, &[1; 1024], MAX).unwrap(),
            "data past the end" => {
                for i in 0..2u64 {
                    let b = frame::encode(Kind::Data, f.seq, i * 1024, &[1; 1024], MAX).unwrap();
                    f.seq += 1;
                    f.input(HostInput::Frame(&b));
                }
                frame::encode(Kind::Data, f.seq, 2048, &[1; 1024], MAX).unwrap()
            }
            _ => {
                let mut g = build("a partition is streaming");
                g.events.clear();
                g.say(
                    Some(g.last_id),
                    PhoneMsg::DataBegin {
                        name: "persist".into(),
                        size: 3000,
                        offset: 0,
                    },
                );
                assert!(g.events.contains(&Event::LinkLost));
                continue;
            }
        };
        f.input(HostInput::Frame(&bytes));
        assert!(
            f.events.contains(&Event::LinkLost),
            "{what}: {:?}",
            f.events
        );
        let wrote = f
            .effects
            .iter()
            .filter(|e| matches!(e, HostEffect::BackupWrite { .. }))
            .count();
        assert_eq!(
            wrote,
            if what == "data past the end" { 2 } else { 0 },
            "{what}: the wrong frame was not written"
        );
    }
    // A hello that answers with a protocol number the host does not speak,
    // or a larger max_data than it offered
    for (proto, max_data) in [(2u32, 1024u32), (1, 2 << 20)] {
        let mut f = build("hello sent");
        let h = PhoneHello {
            proto,
            link: "1".into(),
            device: "cheetah".into(),
            max_data,
            nonce: NONCE,
            host_nonce: f.host_nonce,
            stage: Stage::Waiting,
            pair: PairMode::Code,
            wait_s: 0,
        };
        f.answer(PhoneMsg::Hello(h));
        assert!(f.events.contains(&Event::LinkLost) && !f.events.contains(&Event::NeedCode));
    }
}

#[test]
fn silence_for_a_minute_is_a_lost_link() {
    for state in [
        "hello sent",
        "pair sent",
        "status sent",
        "a partition is streaming",
        "factory-end sent",
    ] {
        let mut f = build(state);
        let hellos = f.hellos();
        assert_eq!(f.host.deadline(), Some(SILENCE_MS), "{state}");
        f.now = SILENCE_MS - 1;
        f.input(HostInput::Tick);
        assert!(!f.events.contains(&Event::LinkLost), "{state}");
        f.now = SILENCE_MS;
        f.input(HostInput::Tick);
        assert!(f.events.contains(&Event::LinkLost), "{state}");
        assert_eq!(f.hellos(), hellos + 1, "{state}: starts again from hello");
        assert!(!f.host.stopped(), "silence is not a strike");
    }
    // Not while it is the computer's turn, or nobody's
    for state in [
        "the code is wanted",
        "paired, idle",
        "the image is being read",
    ] {
        assert_eq!(build(state).host.deadline(), None, "{state}");
    }
}

#[test]
fn commands_out_of_turn() {
    let mut f = build("hello sent");
    f.input(HostInput::Command(Command::Pair(
        Code::parse("000000").unwrap(),
    )));
    assert_eq!(
        f.events,
        vec![Event::NotNow { step: "pair" }],
        "no code was asked for"
    );
    let mut f = build("status sent");
    f.input(HostInput::Command(Command::Backup));
    assert_eq!(
        f.events,
        vec![Event::NotNow { step: "backup" }],
        "one step at a time"
    );
    // A step asked for before pairing waits for it
    let mut f = build("the code is wanted");
    f.input(HostInput::Command(Command::Backup));
    assert_eq!(f.last_t, "hello");
    f.input(HostInput::Command(Command::Pair(
        Code::parse("123456").unwrap(),
    )));
    f.answer(PhoneMsg::Paired {
        token: Some(Token([5; 32])),
        serial_proof: f.proof(),
    });
    assert_eq!(f.last_t, "backup-list");
    // Abort with a stream open says so to the phone; without one it only ends the step
    let mut f = build("a partition is streaming");
    f.input(HostInput::Command(Command::Abort));
    assert_eq!(f.last_t, "abort");
    let b = frame::encode(Kind::Data, f.seq, 0, &[1; 1024], MAX).unwrap();
    f.seq += 1;
    f.input(HostInput::Frame(&b)); // under way when abort left: passed over
    f.answer(PhoneMsg::Aborted);
    assert_eq!(f.events, vec![Event::Aborted]);
    assert!(!f.host.busy() && f.host.ready());
    // What the shell cannot do ends the step and the stream
    let mut f = build("the image is being read");
    let t = f.ticket();
    f.input(HostInput::Done(t, HostOutcome::Failed));
    assert_eq!(f.events, vec![Event::Failed { step: "factory" }]);
    assert_eq!(f.last_t, "abort");
    // A source that gives the wrong number of bytes is the same
    let mut f = build("the image is being read");
    let t = f.ticket();
    f.input(HostInput::Done(t, HostOutcome::Data(vec![0; 5])));
    assert_eq!(f.events, vec![Event::Failed { step: "factory" }]);
    assert!(!f.sent.contains(&"data"));
}

#[test]
fn busy_is_asked_again_and_locked_is_waited_out() {
    let mut f = build("status sent");
    f.answer(PhoneMsg::Error(ErrorMsg::new(
        ErrorCode::Busy,
        Stage::Waiting,
    )));
    assert!(f.events.is_empty() && f.host.busy());
    assert_eq!(f.host.deadline(), Some(BUSY_WAIT_MS));
    f.now = BUSY_WAIT_MS;
    let asked = f.sent.len();
    f.input(HostInput::Tick);
    assert_eq!(f.sent[asked..], ["status"]);
    // No token in hand and no code on show: the host tells the flow what
    // the owner can do, once, and says hello again every five seconds
    for (pair, wait_s, event) in [
        (PairMode::Locked, 30, Event::Locked { wait_s: 30 }),
        (PairMode::Locked, 0, Event::Locked { wait_s: 0 }),
        (PairMode::Ask, 0, Event::NeedTap),
        (PairMode::Token, 0, Event::NeedSession),
    ] {
        let mut f = build("hello sent");
        let h = f.hello_msg(pair, wait_s);
        f.answer(h);
        assert_eq!(f.events[1..], [event.clone()], "{pair:?}");
        assert!(!f.host.stopped() && !f.host.ready());
        for round in 1..=3u64 {
            assert_eq!(f.host.deadline(), Some(f.now + REHELLO_MS), "{pair:?}");
            f.now += REHELLO_MS;
            f.input(HostInput::Tick);
            assert_eq!(f.hellos(), 1 + round as usize, "{pair:?}");
            let h = f.hello_msg(pair, wait_s);
            f.answer(h);
        }
        let told = f.events.iter().filter(|e| **e == event).count();
        assert_eq!(told, 1, "{pair:?}: said once, not every five seconds");
        // A code is there at last
        f.now += REHELLO_MS;
        f.input(HostInput::Tick);
        f.hello(PairMode::Code);
        assert_eq!(f.events.last(), Some(&Event::NeedCode), "{pair:?}");
        assert_eq!(f.host.deadline(), None, "{pair:?}: no more asking");
    }
    // With a token the host resumes in every one of the four
    for pair in [
        PairMode::Code,
        PairMode::Ask,
        PairMode::Token,
        PairMode::Locked,
    ] {
        let mut cfg = host_config();
        cfg.token = Some(Token([5; 32]));
        let mut f = Fake::new();
        f.host = Host::new(cfg);
        f.input(HostInput::LinkUp);
        let h = f.hello_msg(pair, 0);
        f.answer(h);
        assert_eq!(f.last_t, "resume", "{pair:?}");
        // A token the phone no longer takes is forgotten, and a code asked for
        f.answer(PhoneMsg::Error(ErrorMsg::new(
            ErrorCode::BadToken,
            Stage::FirstbootDone,
        )));
        assert!(f.effects.contains(&HostEffect::ForgetToken));
        assert_eq!(f.host.token(), None);
        let want = match pair {
            PairMode::Code => Event::NeedCode,
            PairMode::Ask => Event::NeedTap,
            PairMode::Token => Event::NeedSession,
            PairMode::Locked => Event::Locked { wait_s: 0 },
        };
        assert_eq!(f.events.last(), Some(&want), "{pair:?}");
        assert!(!f.host.stopped(), "{pair:?}: a code can still pair");
    }
    // `pair` answered `locked`: the same waiting
    let mut f = build("pair sent");
    f.answer(PhoneMsg::Error(ErrorMsg::new(
        ErrorCode::Locked { wait_s: 30 },
        Stage::Waiting,
    )));
    assert_eq!(f.events, vec![Event::Locked { wait_s: 30 }]);
    assert_eq!(f.host.deadline(), Some(REHELLO_MS));
    // `pair` answered `not-allowed`: the code went away; ask what there is now
    let mut f = build("pair sent");
    f.answer(PhoneMsg::Error(ErrorMsg::new(
        ErrorCode::NotAllowed,
        Stage::FirstbootDone,
    )));
    assert!(f.events.is_empty());
    assert_eq!(f.sent.last(), Some(&"hello"));
}

#[test]
fn the_host_keeps_to_the_window() {
    // §5.7: never a byte sent beyond the last acknowledgement read plus the
    // window. 9000 bytes, frames of 1024, acknowledged every 4096, window 8192.
    let img = image();
    let mut f = build("the image is being read");
    let feed = |f: &mut Fake| -> u64 {
        // The shell hands over the bytes asked for, until none is asked
        let mut sent = 0;
        while let Some(HostEffect::FactoryRead {
            ticket,
            offset,
            len,
        }) = f.effects.last().cloned()
        {
            f.effects.clear();
            let d = img[offset as usize..(offset + len as u64) as usize].to_vec();
            f.input(HostInput::Done(ticket, HostOutcome::Data(d)));
            sent = offset + len as u64;
        }
        sent
    };
    assert_eq!(feed(&mut f), 8192, "stops at the window's edge");
    assert_eq!(f.sent.iter().filter(|t| **t == "data").count(), 8);
    assert!(f.host.busy() && f.events.iter().all(|e| *e != Event::LinkLost));
    // It waits for the phone there: a minute of that is a lost link
    assert_eq!(f.host.deadline(), Some(SILENCE_MS));
    let mut g = build("the image is being read");
    feed(&mut g);
    g.now = SILENCE_MS;
    g.input(HostInput::Tick);
    assert!(g.events.contains(&Event::LinkLost));
    assert_eq!(g.sent.last(), Some(&"hello"), "then hello");
    // An acknowledgement read moves the edge by exactly what it says
    f.say(None, PhoneMsg::Ack { offset: 4096 });
    assert_eq!(feed(&mut f), 9000, "the rest fits: 9000 <= 4096 + 8192");
    assert_eq!(f.last_t, "factory-end");
    f.say(None, PhoneMsg::Ack { offset: 8192 });
    f.say(None, PhoneMsg::Ack { offset: 9000 });
    f.answer(PhoneMsg::FactoryVerified);
    assert!(f.events.contains(&Event::FactoryVerified));
    // An acknowledgement that goes back, repeats, or is past what was sent
    // is not one: it would move the window where no byte is
    for (bad, after_one) in [(0u64, false), (9000, false), (4096, true), (2048, true)] {
        let mut f = build("the image is being read");
        feed(&mut f);
        if after_one {
            f.say(None, PhoneMsg::Ack { offset: 4096 });
            f.effects.clear();
        }
        f.events.clear();
        f.say(None, PhoneMsg::Ack { offset: bad });
        assert!(f.events.contains(&Event::LinkLost), "ack {bad}");
    }
    // A window that does not reach the next acknowledgement could never be
    // left: not taken
    let mut f = build("paired, idle");
    let mut b = factory_begin(&img[..9000]);
    b.size = 9000;
    f.input(HostInput::Command(Command::Factory(b)));
    f.answer(PhoneMsg::FactoryReady {
        offset: 0,
        ack_every: 4096,
        window: 4096,
    });
    assert!(f.events.contains(&Event::LinkLost));
    assert!(!f.sent.contains(&"data"));
}

#[test]
fn a_suspended_stream_is_begun_again_not_struck() {
    // The phone answers a late data frame `not-allowed`, once, with no `re`
    let late = || PhoneMsg::Error(ErrorMsg::new(ErrorCode::NotAllowed, Stage::FirstbootDone));
    let mut f = build("the image is being read");
    f.say(None, late());
    assert_eq!(f.last_t, "factory-begin", "{:?}", f.events);
    assert!(f.events.is_empty() && f.host.busy());
    // The read that was under way is nobody's now
    f.effects.clear();
    f.answer(PhoneMsg::FactoryReady {
        offset: 4096,
        ack_every: 4096,
        window: 8192,
    });
    assert!(
        matches!(
            f.effects.last(),
            Some(HostEffect::FactoryRead { offset: 4096, .. })
        ),
        "on from the offset the phone answers: {:?}",
        f.effects
    );
    // `factory-end` that met a suspended stream: the note first, which
    // changes nothing, then its own answer, which begins again
    let mut f = build("factory-end sent");
    let end_id = f.last_id;
    f.say(None, late());
    assert!(f.events.is_empty() && f.last_t == "factory-end");
    f.say(Some(end_id), late());
    assert_eq!(f.last_t, "factory-begin");
    assert!(f.events.is_empty());
    // A phone that suspends every time is given up on as a failed step,
    // not as a protocol error
    let mut f = build("the image is being read");
    for _ in 0..FACTORY_TRIES {
        f.say(None, late());
        if f.last_t == "factory-begin" {
            f.answer(PhoneMsg::FactoryReady {
                offset: 0,
                ack_every: 4096,
                window: 8192,
            });
        }
    }
    assert!(f.events.contains(&Event::Failed { step: "factory" }));
    assert!(!f.host.stopped() && !f.events.contains(&Event::LinkLost));
    // Anywhere else it is out of turn
    let mut f = build("status sent");
    f.say(None, late());
    assert!(f.events.contains(&Event::LinkLost));
}

#[test]
fn the_backups_files_are_written_before_the_commit() {
    // §5.5: `SHA256SUMS`, `partitions.txt` and `NV-BACKUP` are on disk and
    // synced before `backup-commit` is sent
    let mut f = build("a partition is streaming");
    for i in 0..3u64 {
        let len = if i == 2 { 952 } else { 1024 };
        let b = frame::encode(Kind::Data, f.seq, i * 1024, &vec![1; len], MAX).unwrap();
        f.seq += 1;
        f.input(HostInput::Frame(&b));
    }
    f.say(None, PhoneMsg::DataEnd { name: "efs".into() });
    let t = f.ticket();
    f.input(HostInput::Done(t, HostOutcome::Digest([7; 32])));
    assert_eq!(f.last_t, "backup-verify");
    f.answer(PhoneMsg::BackupVerified);
    assert!(
        matches!(f.effects.last(), Some(HostEffect::BackupFinish { parts, .. }) if parts.len() == 1 && parts[0].1 == [7; 32]),
        "{:?}",
        f.effects
    );
    assert_ne!(f.last_t, "backup-commit", "not before the files are there");
    assert_eq!(
        f.host.deadline(),
        None,
        "the computer's turn: no silence counted"
    );
    let t = f.ticket();
    // Written and synced: now the commit, and `backup-done` ends the step
    let mut ok = f.clone();
    ok.input(HostInput::Done(t, HostOutcome::Done));
    assert_eq!(ok.last_t, "backup-commit");
    ok.answer(PhoneMsg::BackupDone);
    assert_eq!(ok.events.last(), Some(&Event::BackupDone));
    // The shell could not write them: nothing is committed
    f.input(HostInput::Done(t, HostOutcome::Failed));
    assert_eq!(f.events.last(), Some(&Event::Failed { step: "backup" }));
    assert!(!f.sent.contains(&"backup-commit"));
}
