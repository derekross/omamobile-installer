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

/// A phone written out by hand.
struct Fake {
    host: Host,
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
                        if self.last_t == "hello" {
                            assert_eq!(f.seq, 0, "hello is frame 0");
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
        let out = self.host.handle(self.now, i);
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
    fn hello(&mut self, pair: PairMode) {
        let h = PhoneHello {
            proto: 1,
            link: "0.0.1".into(),
            device: "cheetah".into(),
            max_data: 1024,
            nonce: NONCE,
            stage: Stage::FirstbootDone,
            pair,
            wait_s: 0,
        };
        self.answer(PhoneMsg::Hello(h));
    }
    fn ticket(&self) -> Ticket {
        match self.effects.last() {
            Some(
                HostEffect::BackupOpen { ticket, .. }
                | HostEffect::BackupDigest { ticket, .. }
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
        serial_proof: serial_proof(&NONCE, SERIAL),
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
            f.answer(PhoneMsg::FactoryReady { offset: 0 });
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
        return state != "hello sent";
    }
    if re != "right" {
        // (`data-end` before the partition's last byte is out of turn too)
        return matches!(
            (state, t, re),
            (
                "the image is being read" | "factory-end sent",
                "ack",
                "none"
            )
        );
    }
    match state {
        "hello sent" => matches!(t, "hello" | "hello-locked" | "error-version"),
        "pair sent" => matches!(t, "paired" | "error-bad-code" | "error-locked"),
        "status sent" => t == "status" || t.starts_with("error-"),
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
                let hellos = f.sent.iter().filter(|t| **t == "hello").count();
                let was_busy = f.host.busy();
                let re_v = re_value(&f);
                f.say(re_v, sample.msg.clone());
                n += 1;
                let lost = f.events.contains(&Event::LinkLost) || f.host.stopped();
                let again = f.sent.iter().filter(|t| **t == "hello").count() > hellos;
                let name = name.as_str();
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
                if state == "hello sent" && re != "right" {
                    // Before the phone's hello, what does not answer ours is
                    // from a session that is over: passed over in silence
                    assert!(
                        f.events.is_empty() && !again,
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
        let hellos = f.sent.iter().filter(|t| **t == "hello").count();
        assert_eq!(f.host.deadline(), Some(SILENCE_MS), "{state}");
        f.now = SILENCE_MS - 1;
        f.input(HostInput::Tick);
        assert!(!f.events.contains(&Event::LinkLost), "{state}");
        f.now = SILENCE_MS;
        f.input(HostInput::Tick);
        assert!(f.events.contains(&Event::LinkLost), "{state}");
        assert_eq!(
            f.sent.iter().filter(|t| **t == "hello").count(),
            hellos + 1,
            "{state}: starts again from hello"
        );
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
        serial_proof: serial_proof(&NONCE, SERIAL),
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
    // Locked at hello: the host says hello again when the wait is over
    let mut f = build("hello sent");
    let h = PhoneHello {
        proto: 1,
        link: "1".into(),
        device: "cheetah".into(),
        max_data: 1024,
        nonce: NONCE,
        stage: Stage::Waiting,
        pair: PairMode::Locked,
        wait_s: 30,
    };
    f.answer(PhoneMsg::Hello(h));
    assert!(f.events.contains(&Event::Locked { wait_s: 30 }));
    assert_eq!(f.host.deadline(), Some(30_500));
    f.now = 30_500;
    f.input(HostInput::Tick);
    assert_eq!(f.sent, ["hello", "hello"]);
    // After the restart with no token in hand: nothing can pair
    let mut f = build("hello sent");
    f.hello(PairMode::Token);
    assert_eq!(f.events.last(), Some(&Event::Stopped(Stop::CannotPair)));
}
