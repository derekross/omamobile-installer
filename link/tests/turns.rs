// SPDX-License-Identifier: MIT
//! The phone's side driven by hand, frame by frame, with no installer
//! machine in between: every message in every state, in turn and out of it,
//! and every way a frame can be wrong in a session.
//!
//! "Anything unexpected changes nothing" (INSTALL-LINK-PLAN §5.2): a message
//! out of turn gets its refusal and the phone stays where it was. Only a
//! wrong frame ends the session.
mod common;
use common::rig::*;
use common::*;
use omamobile_link::digest::{serial_proof, sha256};
use omamobile_link::frame::{self, FrameError, Kind};
use omamobile_link::msg::*;
use omamobile_link::phone::*;
use omamobile_link::sim::SimPhone;

/// A host written out by hand: it numbers its frames and messages and reads
/// what comes back.
struct Client {
    seq: u32,
    id: u64,
    rx: u32,
    nonce: [u8; 32],
    token: Option<Token>,
    now: u64,
}

#[derive(Debug, PartialEq)]
enum Got {
    Msg(PhoneFrame),
    Data { offset: u64, len: usize },
}

impl Got {
    /// The message's type, or `error:<code>`.
    fn name(&self) -> String {
        match self {
            Got::Msg(PhoneFrame {
                msg: PhoneMsg::Error(e),
                ..
            }) => format!("error:{}", e.code.as_str()),
            Got::Msg(f) => f.msg.t().to_string(),
            Got::Data { .. } => "data".to_string(),
        }
    }
}

impl Client {
    fn new() -> Client {
        Client {
            seq: 0,
            id: 0,
            rx: 0,
            nonce: [0; 32],
            token: None,
            now: 0,
        }
    }

    fn read(&mut self, frames: Vec<Vec<u8>>) -> Vec<Got> {
        frames
            .iter()
            .map(|b| {
                let f = frame::decode(b, MAX_DATA).expect("the phone sends valid frames");
                assert_eq!(f.seq, self.rx, "the phone's frames are numbered one by one");
                self.rx += 1;
                match f.kind {
                    Kind::Message => Got::Msg(
                        PhoneFrame::parse(f.payload).expect("the phone sends valid messages"),
                    ),
                    Kind::Data => Got::Data {
                        offset: f.offset,
                        len: f.payload.len(),
                    },
                }
            })
            .collect()
    }

    fn raw(&mut self, p: &mut SimPhone, bytes: &[u8]) -> Vec<Got> {
        let frames = p.feed(self.now, PhoneInput::Frame(bytes));
        self.read(frames)
    }

    fn payload(&mut self, p: &mut SimPhone, json: &str) -> Vec<Got> {
        let bytes = frame::encode(Kind::Message, self.seq, 0, json.as_bytes(), MAX_DATA).unwrap();
        self.seq += 1;
        self.raw(p, &bytes)
    }

    fn send(&mut self, p: &mut SimPhone, msg: HostMsg) -> Vec<Got> {
        self.id += 1;
        let id = self.id;
        let json = HostFrame { id, msg }.to_json().unwrap();
        let got = self.payload(p, &json);
        // An answer names the message it answers
        for g in &got {
            if let Got::Msg(PhoneFrame { re: Some(re), .. }) = g {
                assert_eq!(*re, id);
            }
        }
        got
    }

    fn data(&mut self, p: &mut SimPhone, offset: u64, bytes: &[u8]) -> Vec<Got> {
        let f = frame::encode(Kind::Data, self.seq, offset, bytes, MAX_DATA).unwrap();
        self.seq += 1;
        self.raw(p, &f)
    }

    fn hello(&mut self, p: &mut SimPhone) -> PhoneHello {
        self.seq = 0;
        self.rx = 0;
        let h = HostHello {
            proto: vec![1],
            installer_name: "test".into(),
            installer_version: "1".into(),
            max_data: 4096,
        };
        match self.send(p, HostMsg::Hello(h)).pop() {
            Some(Got::Msg(PhoneFrame {
                msg: PhoneMsg::Hello(h),
                ..
            })) => {
                self.nonce = h.nonce;
                h
            }
            other => panic!("no hello: {other:?}"),
        }
    }

    fn pair(&mut self, p: &mut SimPhone) {
        let code = p.phone.code().expect("a code on the screen");
        match self.send(p, HostMsg::Pair { code }).pop() {
            Some(Got::Msg(PhoneFrame {
                msg:
                    PhoneMsg::Paired {
                        token: Some(t),
                        serial_proof: proof,
                    },
                ..
            })) => {
                assert_eq!(proof, serial_proof(&self.nonce, SERIAL));
                self.token = Some(t);
            }
            other => panic!("not paired: {other:?}"),
        }
    }

    fn one(&mut self, p: &mut SimPhone, msg: HostMsg) -> String {
        let got = self.send(p, msg);
        got.first().map_or("nothing".to_string(), Got::name)
    }
}

fn up() -> (SimPhone, Client) {
    let mut p = sim_phone(Persistent::default(), true, true, 1);
    p.feed(0, PhoneInput::LinkUp);
    (p, Client::new())
}

fn paired() -> (SimPhone, Client) {
    let (mut p, mut c) = up();
    c.hello(&mut p);
    c.pair(&mut p);
    (p, c)
}

fn begin(image: &[u8]) -> HostMsg {
    HostMsg::FactoryBegin(factory_begin(image))
}

fn backup_all(p: &mut SimPhone, c: &mut Client) {
    assert_eq!(c.one(p, HostMsg::BackupList), "backup-parts");
    for (found, bytes) in parts() {
        let got = c.send(
            p,
            HostMsg::BackupRead {
                name: found.name.clone(),
                offset: 0,
            },
        );
        assert_eq!(got.first().unwrap().name(), "data-begin");
        assert_eq!(got.last().unwrap().name(), "data-end");
        let read: usize = got
            .iter()
            .map(|g| {
                if let Got::Data { len, .. } = g {
                    *len
                } else {
                    0
                }
            })
            .sum();
        assert_eq!(read, bytes.len());
        assert_eq!(
            c.one(
                p,
                HostMsg::BackupVerify {
                    name: found.name,
                    sha256: sha256(&bytes)
                }
            ),
            "backup-verified"
        );
    }
}

fn send_image(p: &mut SimPhone, c: &mut Client) {
    let img = image();
    assert_eq!(c.one(p, begin(&img)), "factory-ready");
    for (i, chunk) in img.chunks(MAX_DATA as usize).enumerate() {
        c.data(p, i as u64 * MAX_DATA as u64, chunk);
    }
}

/// The states a phone can be found in, by name, each built from nothing.
const STATES: [&str; 17] = [
    "no session",
    "unpaired",
    "locked",
    "waiting",
    "firstboot",
    "firstboot-failed",
    "firstboot-done",
    "backup listed",
    "image stream open",
    "image all sent",
    "ready to provision",
    "provisioning",
    "provisioned",
    "after the restart",
    "after the restart, unpaired",
    "a step is running",
    "backup stream open",
];

fn build(state: &str) -> (SimPhone, Client) {
    let through = |n: usize| -> (SimPhone, Client) {
        let (mut p, mut c) = paired();
        if n >= 1 {
            assert_eq!(
                c.one(&mut p, HostMsg::Answers(answers().get().clone())),
                "answers-ok"
            );
        }
        if n >= 2 {
            p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
        }
        if n >= 3 {
            backup_all(&mut p, &mut c);
            assert_eq!(c.one(&mut p, HostMsg::BackupCommit), "backup-done");
            send_image(&mut p, &mut c);
            assert_eq!(c.one(&mut p, HostMsg::FactoryEnd), "factory-verified");
        }
        if n >= 4 {
            assert_eq!(c.one(&mut p, HostMsg::Provision), "provision-started");
        }
        if n >= 5 {
            let f = p.feed(
                0,
                PhoneInput::ProvisionEnded {
                    steps: vec![],
                    failed: None,
                },
            );
            assert_eq!(c.read(f)[0].name(), "provision-done");
        }
        (p, c)
    };
    match state {
        "no session" => up(),
        "unpaired" => {
            let (mut p, mut c) = up();
            c.hello(&mut p);
            (p, c)
        }
        "locked" => {
            let (mut p, mut c) = up();
            c.hello(&mut p);
            let right = p.phone.code().unwrap();
            let wrong = Code::parse(if right.digits() == "111111" {
                "222222"
            } else {
                "111111"
            })
            .unwrap();
            for _ in 0..5 {
                c.send(&mut p, HostMsg::Pair { code: wrong });
            }
            (p, c)
        }
        "waiting" => through(0),
        "firstboot" => through(1),
        "firstboot-failed" => {
            let (mut p, c) = through(1);
            p.feed(
                0,
                PhoneInput::FirstbootEnded {
                    failed: vec!["SSH keys".into()],
                },
            );
            (p, c)
        }
        "firstboot-done" => through(2),
        "backup listed" => {
            let (mut p, mut c) = through(2);
            assert_eq!(c.one(&mut p, HostMsg::BackupList), "backup-parts");
            (p, c)
        }
        "image stream open" => {
            let (mut p, mut c) = through(2);
            assert_eq!(c.one(&mut p, begin(&image())), "factory-ready");
            (p, c)
        }
        "image all sent" => {
            let (mut p, mut c) = through(2);
            send_image(&mut p, &mut c);
            (p, c)
        }
        "ready to provision" => through(3),
        "provisioning" => through(4),
        "provisioned" => through(5),
        "after the restart" | "after the restart, unpaired" => {
            let (p, mut c) = through(5);
            let mut p2 = sim_phone(p.state_file.clone(), true, false, 3);
            p2.feed(0, PhoneInput::LinkUp);
            let h = c.hello(&mut p2);
            assert_eq!(h.pair, PairMode::Token);
            if state == "after the restart" {
                assert_eq!(
                    c.one(
                        &mut p2,
                        HostMsg::Resume {
                            token: c.token.unwrap()
                        }
                    ),
                    "paired"
                );
            }
            (p2, c)
        }
        "a step is running" => {
            let (mut p, mut c) = through(2);
            p.hold = true;
            assert_eq!(c.one(&mut p, HostMsg::BackupList), "nothing");
            (p, c)
        }
        "backup stream open" => {
            let (mut p, mut c) = through(2);
            assert_eq!(c.one(&mut p, HostMsg::BackupList), "backup-parts");
            p.hold = true;
            assert_eq!(
                c.one(
                    &mut p,
                    HostMsg::BackupRead {
                        name: "efs".into(),
                        offset: 0
                    }
                ),
                "data-begin"
            );
            (p, c)
        }
        other => panic!("{other}"),
    }
}

/// What §5 says the phone answers: written out here from the "Allowed"
/// column of its tables, not from the code.
fn expected(state: &str, t: &str) -> &'static str {
    let paired = !matches!(
        state,
        "no session" | "unpaired" | "locked" | "after the restart, unpaired"
    );
    let before_provisioned = !matches!(state, "provisioned" | "after the restart");
    let image_stream = matches!(state, "image stream open" | "image all sent");
    let has_list = matches!(
        state,
        "backup listed" | "ready to provision" | "provisioning" | "backup stream open"
    );
    let backed_up = matches!(state, "ready to provision" | "provisioning");
    if state == "no session" {
        // Nothing is answered before `hello`
        return if t == "hello" { "hello" } else { "nothing" };
    }
    match t {
        "hello" => return "hello",
        "ping" => return "pong",
        "bye" => return "bye",
        // Its facts are read by the shell, which is held in two of the states
        "status" => {
            return if matches!(state, "a step is running" | "backup stream open") {
                "nothing"
            } else {
                "status"
            }
        }
        "pair" => {
            return match state {
                "unpaired" => "paired",
                "locked" => "error:locked",
                _ => "error:not-allowed",
            }
        }
        "resume" => {
            return match state {
                "after the restart, unpaired" => "paired",
                "locked" => "error:locked",
                _ => "error:not-allowed",
            }
        }
        _ => {}
    }
    if !paired {
        return "error:not-paired";
    }
    if t == "abort" {
        return if image_stream || state == "backup stream open" {
            "aborted"
        } else {
            "error:not-allowed"
        };
    }
    if matches!(state, "a step is running" | "backup stream open") {
        return "error:busy";
    }
    if image_stream {
        // LINK.md D15: with the image's stream open, only its own messages
        return match (t, state) {
            ("factory-end", "image all sent") => "factory-verified",
            _ => "error:not-allowed",
        };
    }
    let ok = |yes: bool, reply: &'static str| if yes { reply } else { "error:not-allowed" };
    match t {
        "answers" => ok(state == "waiting", "answers-ok"),
        "firstboot-retry" => ok(state == "firstboot-failed", "answers-ok"),
        "backup-list" => ok(before_provisioned, "backup-parts"),
        "backup-read" => ok(has_list, "data-begin"),
        "backup-verify" => ok(backed_up, "backup-verified"),
        "backup-commit" => ok(backed_up, "backup-done"),
        "factory-begin" => ok(
            before_provisioned && state != "provisioning",
            "factory-ready",
        ),
        "factory-end" => "error:not-allowed",
        "provision" => match state {
            "firstboot-failed" => "error:firstboot-failed",
            _ => ok(state == "ready to provision", "provision-started"),
        },
        "restart" => ok(!before_provisioned, "restarting"),
        "smoke" => ok(state == "after the restart", "smoke-report"),
        "finish" => ok(!before_provisioned, "finished"),
        other => panic!("{other}"),
    }
}

#[test]
fn every_message_in_every_state() {
    let mut refused = 0;
    let mut taken = 0;
    for state in STATES {
        for (name, sample) in host_samples() {
            if name == "answers-least" {
                continue;
            }
            let (mut p, mut c) = build(state);
            let msg = match sample.msg {
                HostMsg::Pair { .. } => HostMsg::Pair {
                    code: p.phone.code().unwrap_or(Code::parse("000000").unwrap()),
                },
                HostMsg::Resume { .. } => HostMsg::Resume {
                    token: c.token.unwrap_or(Token([9; 32])),
                },
                HostMsg::BackupRead { .. } => HostMsg::BackupRead {
                    name: "efs".into(),
                    offset: 0,
                },
                HostMsg::BackupVerify { .. } => HostMsg::BackupVerify {
                    name: "efs".into(),
                    sha256: sha256(&parts()[0].1),
                },
                HostMsg::FactoryBegin(_) => begin(&image()),
                HostMsg::Hello(_) => {
                    // By the client's own hello, which starts the numbering again
                    c.hello(&mut p);
                    assert!(
                        !p.phone.paired(),
                        "{state}: a new hello is a new session, unpaired"
                    );
                    taken += 1;
                    continue;
                }
                other => other,
            };
            let t = msg.t();
            let before = (
                p.state_file.clone(),
                p.phone.stage(),
                p.phone.paired(),
                p.phone.wrong_tries(),
            );
            p.log.clear();
            if state == "no session" {
                c.seq = 3; // whatever: there is no session to number
            }
            let got = c.one(&mut p, msg);
            let want = expected(state, t);
            assert_eq!(got, want, "{t} in state \"{state}\"");
            if want.starts_with("error:") || (want == "nothing" && state == "no session") {
                refused += 1;
                // Nothing changed: the stage, the state file, the pairing,
                // the count of wrong tries; and nothing was asked of the shell
                let after = (
                    p.state_file.clone(),
                    p.phone.stage(),
                    p.phone.paired(),
                    p.phone.wrong_tries(),
                );
                assert_eq!(before, after, "{t} in state \"{state}\"");
                let did: Vec<&Effect> = p
                    .log
                    .iter()
                    .filter(|e| !matches!(e, Effect::Note(_)))
                    .collect();
                assert!(did.is_empty(), "{t} in state \"{state}\": {did:?}");
                // ... and the session goes on
                if state != "no session" {
                    assert_eq!(
                        c.one(&mut p, HostMsg::Ping),
                        "pong",
                        "{t} in state \"{state}\""
                    );
                }
            } else {
                taken += 1;
            }
        }
    }
    assert_eq!(refused + taken, 17 * 19);
    assert!(
        refused > 150 && taken > 80,
        "{refused} refused, {taken} taken"
    );
}

#[test]
fn a_message_type_protocol_1_does_not_have() {
    for state in ["unpaired", "waiting", "provisioned"] {
        let (mut p, mut c) = build(state);
        let before = p.state_file.clone();
        let got = c.payload(&mut p, r#"{"t":"shell","id":77,"command":"id"}"#);
        assert!(
            matches!(&got[..], [Got::Msg(PhoneFrame { re: Some(77), msg: PhoneMsg::Error(e) })] if e.code == ErrorCode::Unknown)
        );
        assert_eq!(p.state_file, before);
        assert_eq!(
            p.phone.wrong_tries(),
            0,
            "an unknown type is not a wrong try"
        );
        assert_eq!(
            c.one(&mut p, HostMsg::Ping),
            "pong",
            "and the session goes on"
        );
    }
}

/// Every way a frame can be wrong in a session, as (name, bytes).
fn wrong_frames(c: &Client) -> Vec<(&'static str, Vec<u8>)> {
    let ping = br#"{"t":"ping","id":900}"#;
    let good = frame::encode(Kind::Message, c.seq, 0, ping, MAX_DATA).unwrap();
    let with = |at: usize, b: u8| {
        let mut v = good.clone();
        v[at] = b;
        v
    };
    let msg =
        |json: &str| frame::encode(Kind::Message, c.seq, 0, json.as_bytes(), MAX_DATA).unwrap();
    vec![
        ("magic", with(0, b'X')),
        ("version", with(4, 2)),
        ("kind", with(5, 9)),
        ("a reserved bit", with(7, 1)),
        (
            "seq behind",
            frame::encode(Kind::Message, c.seq.wrapping_sub(1), 0, ping, MAX_DATA).unwrap(),
        ),
        (
            "seq ahead",
            frame::encode(Kind::Message, c.seq + 1, 0, ping, MAX_DATA).unwrap(),
        ),
        ("len over the limit", with(14, 0xff)),
        ("len not what arrived", good[..good.len() - 1].to_vec()),
        ("digest", with(30, good[30] ^ 1)),
        ("payload changed", with(60, b'X')),
        ("short", good[..40].to_vec()),
        ("empty", vec![]),
        (
            "data with no stream",
            frame::encode(Kind::Data, c.seq, 0, b"abc", MAX_DATA).unwrap(),
        ),
        (
            "data over max_data",
            frame::encode(Kind::Data, c.seq, 0, &[0; 2048], 4096).unwrap(),
        ),
        ("not JSON", msg("ping")),
        ("not an object", msg("[1]")),
        ("no type", msg(r#"{"id":1}"#)),
        ("a key twice", msg(r#"{"t":"ping","id":1,"id":2}"#)),
        ("an unknown key", msg(r#"{"t":"ping","id":1,"x":1}"#)),
        ("a missing key", msg(r#"{"t":"pair","id":1}"#)),
        ("another type", msg(r#"{"t":"pair","id":1,"code":123456}"#)),
        (
            "a code that is not six digits",
            msg(r#"{"t":"pair","id":1,"code":"12345"}"#),
        ),
        (
            "a name that is a path",
            msg(r#"{"t":"backup-read","id":1,"name":"../../etc/shadow","offset":0}"#),
        ),
        (
            "a hello that is not frame 0",
            msg(
                r#"{"t":"hello","id":1,"proto":[1],"installer":{"name":"n","version":"v"},"max_data":4096}"#,
            ),
        ),
    ]
}

#[test]
fn every_wrong_frame_ends_the_session_and_counts() {
    for state in [
        "unpaired",
        "waiting",
        "ready to provision",
        "image stream open",
    ] {
        let (_, c) = build(state);
        for (what, bytes) in wrong_frames(&c) {
            if what == "data with no stream" && state == "image stream open" {
                continue;
            }
            let (mut p, mut c) = build(state);
            let before = (p.state_file.clone(), p.phone.stage());
            let got = c.raw(&mut p, &bytes);
            // `error`, code `protocol`, by itself; then nothing until a hello
            assert!(
                matches!(&got[..], [Got::Msg(PhoneFrame { re: None, msg: PhoneMsg::Error(e) })] if e.code == ErrorCode::Protocol),
                "{what} in \"{state}\": {got:?}"
            );
            assert!(!p.phone.paired(), "{what}: the session is over");
            assert_eq!(p.phone.wrong_tries(), 1, "{what}: a wrong try is counted");
            assert_eq!(
                (p.state_file.clone(), p.phone.stage()),
                before,
                "{what}: the stage stays"
            );
            assert!(
                p.notes
                    .iter()
                    .any(|n| matches!(n, Note::Frame(_) | Note::Message(_))),
                "{what}: named in the journal"
            );
            c.seq += 1;
            assert_eq!(
                c.one(&mut p, HostMsg::Ping),
                "nothing",
                "{what}: no session, no answer"
            );
            assert_eq!(
                p.phone.wrong_tries(),
                1,
                "{what}: what follows without a session is not counted again"
            );
            // A hello starts again; a token pairs again where one exists
            let h = c.hello(&mut p);
            assert_eq!(h.stage, before.1);
            if let Some(token) = c.token {
                assert_eq!(c.one(&mut p, HostMsg::Resume { token }), "paired", "{what}");
            }
        }
    }
}

#[test]
fn five_wrong_frames_lock_pairing_like_five_wrong_codes() {
    let (mut p, mut c) = up();
    for n in 1..=5 {
        c.hello(&mut p);
        c.raw(&mut p, b"garbage");
        assert_eq!(p.phone.wrong_tries(), n);
    }
    let h = c.hello(&mut p);
    assert_eq!((h.pair, h.wait_s), (PairMode::Locked, 30));
    assert!(p.screens.contains(&Screen::Locked { wait_s: 30 }));
    // The lock ends by the clock: the screen shows the new code again
    assert_eq!(p.phone.deadline(), Some(30_000));
    p.feed(30_000, PhoneInput::Tick);
    c.now = 30_000;
    assert_eq!(c.hello(&mut p).pair, PairMode::Code);
    assert!(matches!(p.screens.last(), Some(Screen::Code(_))));
    c.pair(&mut p);
}

#[test]
fn the_image_stream_checks_every_data_frame() {
    let img = image();
    let cases: [(&str, u64, usize); 4] = [
        ("behind", 0, 1024),
        ("ahead", 2048, 1024),
        ("past the end", 1024, 1024),
        ("off by one", 1025, 10),
    ];
    for (what, offset, len) in cases {
        let (mut p, mut c) = build("firstboot-done");
        let size = if what == "past the end" {
            1500
        } else {
            img.len()
        };
        let mut b = factory_begin(&img[..size]);
        b.size = size as u64;
        assert_eq!(c.one(&mut p, HostMsg::FactoryBegin(b)), "factory-ready");
        assert!(
            c.data(&mut p, 0, &img[..1024]).is_empty(),
            "written, not yet acknowledged"
        );
        let got = c.data(&mut p, offset, &img[..len]);
        assert_eq!(got[0].name(), "error:protocol", "{what}");
        let wanted = match what {
            "past the end" => FrameError::StreamOverrun {
                size: 1500,
                end: 2048,
            },
            _ => FrameError::StreamOffset {
                expected: 1024,
                got: offset,
            },
        };
        assert!(
            p.notes.contains(&Note::Frame(wanted)),
            "{what}: {:?}",
            p.notes
        );
        assert_eq!(
            p.factory_file.len(),
            1024,
            "{what}: nothing of the wrong frame was written"
        );
    }
    // Acknowledgements mark what is synced: at each boundary and at the end
    let (mut p, mut c) = build("firstboot-done");
    assert_eq!(c.one(&mut p, begin(&img)), "factory-ready");
    let mut acks = Vec::new();
    for (i, chunk) in img.chunks(1024).enumerate() {
        for g in c.data(&mut p, i as u64 * 1024, chunk) {
            match g {
                Got::Msg(PhoneFrame {
                    re: None,
                    msg: PhoneMsg::Ack { offset },
                }) => acks.push(offset),
                other => panic!("{other:?}"),
            }
        }
    }
    assert_eq!(acks, vec![4096, 8192, IMAGE_LEN as u64]);
    let synced: Vec<u64> = p
        .log
        .iter()
        .filter_map(|e| match e {
            Effect::FactoryWrite {
                offset,
                data,
                sync: true,
            } => Some(offset + data.len() as u64),
            _ => None,
        })
        .collect();
    assert_eq!(
        synced, acks,
        "an acknowledgement follows a sync, and only one"
    );
    // The state file had the offset before the acknowledgement left
    let order: Vec<&Effect> = p
        .log
        .iter()
        .filter(|e| matches!(e, Effect::Persist(_) | Effect::Note(Note::Sent("ack"))))
        .collect();
    assert!(
        matches!(
            order[order.len() - 2..],
            [Effect::Persist(_), Effect::Note(_)]
        ),
        "{order:?}"
    );
    // `factory-end` before every byte is there is out of turn
    let (mut p, mut c) = build("image stream open");
    c.data(&mut p, 0, &img[..1024]);
    assert_eq!(c.one(&mut p, HostMsg::FactoryEnd), "error:not-allowed");
    // The stream is still open, where it was
    assert!(c.data(&mut p, 1024, &img[1024..2048]).is_empty());
    assert_eq!(p.phone.wrong_tries(), 0);
}

#[test]
fn a_stream_with_no_frame_for_two_minutes_is_suspended() {
    let (mut p, mut c) = build("image stream open");
    let img = image();
    c.data(&mut p, 0, &img[..1024]);
    c.now = 119_999;
    p.feed(c.now, PhoneInput::Tick);
    assert_eq!(c.one(&mut p, HostMsg::Ping), "pong");
    c.now += 120_000;
    p.feed(c.now, PhoneInput::Tick);
    assert!(p.notes.contains(&Note::StreamIdle));
    // The session is still there (the ping kept it), the stream is not
    assert_eq!(c.one(&mut p, HostMsg::FactoryEnd), "error:not-allowed");
    // It goes on from the last acknowledged offset: nothing was, so from 0
    let got = c.send(&mut p, begin(&img));
    assert!(
        matches!(
            &got[..],
            [Got::Msg(PhoneFrame {
                msg: PhoneMsg::FactoryReady { offset: 0 },
                ..
            })]
        ),
        "{got:?}"
    );
}

#[test]
fn a_session_with_no_frame_for_ten_minutes_ends() {
    let (mut p, mut c) = build("waiting");
    assert_eq!(p.phone.deadline(), Some(SESSION_IDLE_MS));
    p.feed(SESSION_IDLE_MS - 1, PhoneInput::Tick);
    assert!(p.phone.paired());
    p.feed(SESSION_IDLE_MS, PhoneInput::Tick);
    assert!(!p.phone.paired());
    assert_eq!(
        p.phone.stage(),
        Stage::Waiting,
        "ending a session never changes the stage"
    );
    assert_eq!(c.one(&mut p, HostMsg::Ping), "nothing");
    assert_eq!(p.phone.wrong_tries(), 0);
}

#[test]
fn the_cable_ends_the_session_and_keeps_the_stage() {
    for state in STATES {
        let (mut p, mut c) = build(state);
        let before = (p.state_file.clone(), p.phone.stage());
        p.hold = false;
        assert!(p.feed(0, PhoneInput::LinkDown).is_empty());
        assert!(!p.phone.paired());
        assert_eq!((p.state_file.clone(), p.phone.stage()), before, "{state}");
        // Frames while the link is down are nothing
        assert!(c.raw(&mut p, b"x").is_empty());
        p.feed(0, PhoneInput::LinkUp);
        let h = c.hello(&mut p);
        assert_eq!(h.stage, before.1, "{state}");
    }
}

#[test]
fn an_answer_that_comes_after_its_session_is_not_sent_into_the_next() {
    // The phone is at a second read when the cable goes; the installer
    // comes back, and the old answer must not land in the new session.
    let (mut p, mut c) = build("backup listed");
    let efs = parts()[0].1.clone();
    c.send(
        &mut p,
        HostMsg::BackupRead {
            name: "efs".into(),
            offset: 0,
        },
    );
    p.hold = true;
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::BackupVerify {
                name: "efs".into(),
                sha256: sha256(&efs)
            }
        ),
        "nothing"
    );
    p.feed(0, PhoneInput::LinkDown);
    p.feed(0, PhoneInput::LinkUp);
    c.hello(&mut p);
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::Resume {
                token: c.token.unwrap()
            }
        ),
        "paired"
    );
    // Still at it: another step is told so
    assert_eq!(c.one(&mut p, HostMsg::BackupList), "error:busy");
    p.hold = false;
    let late = p.release(0);
    assert!(
        late.is_empty(),
        "nothing is sent for the old session's question"
    );
    // What it found is kept all the same
    assert_eq!(c.one(&mut p, HostMsg::BackupList), "backup-parts");
    let got = c.send(&mut p, HostMsg::Status);
    assert!(
        matches!(&got[..], [Got::Msg(PhoneFrame { msg: PhoneMsg::Status(s), .. })] if s.backup_parts[0].verified)
    );
    // An answer for a ticket nobody holds is noted and dropped
    assert!(p.feed(0, PhoneInput::Done(9999, Outcome::Done)).is_empty());
    assert!(p.notes.contains(&Note::StaleTicket));
}

#[test]
fn what_the_shell_cannot_do_is_internal_and_a_fault_stops_the_daemon() {
    let (mut p, mut c) = build("backup listed");
    p.hold = true;
    c.send(&mut p, HostMsg::BackupList);
    let (ticket, _) = p.held.pop().unwrap();
    let f = p.feed(0, PhoneInput::Done(ticket, Outcome::Failed));
    assert_eq!(c.read(f)[0].name(), "error:internal");
    // An answer that is not one the step has
    c.send(&mut p, HostMsg::BackupList);
    let (ticket, _) = p.held.pop().unwrap();
    let f = p.feed(0, PhoneInput::Done(ticket, Outcome::Digest([0; 32])));
    assert_eq!(c.read(f)[0].name(), "error:internal");
    assert!(p.notes.contains(&Note::BadOutcome));
    // A listing that no message could carry
    c.send(&mut p, HostMsg::BackupList);
    let (ticket, _) = p.held.pop().unwrap();
    let bad = PartFound {
        name: "../x".into(),
        size: 1,
        checked: true,
        dev: "sda1".into(),
    };
    let f = p.feed(0, PhoneInput::Done(ticket, Outcome::Parts(vec![bad])));
    assert_eq!(c.read(f)[0].name(), "error:internal");
    // A write that failed: the daemon says so, ends the session and stops
    let f = p.feed(0, PhoneInput::Fault);
    assert_eq!(c.read(f)[0].name(), "error:internal");
    assert!(p.stopped && p.phone.stopped());
    assert!(p.feed(0, PhoneInput::LinkUp).is_empty());
}

#[test]
fn backup_read_takes_only_offsets_the_plan_allows() {
    let (mut p, mut c) = build("backup listed");
    // A multiple of max_data, at most the size
    for (offset, want) in [
        (1024u64, "data-begin"),
        (2048, "data-begin"),
        (1, "error:not-allowed"),
        (1000, "error:not-allowed"),
        (4096, "error:not-allowed"),
    ] {
        assert_eq!(
            c.one(
                &mut p,
                HostMsg::BackupRead {
                    name: "efs".into(),
                    offset
                }
            ),
            want,
            "{offset}"
        );
    }
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::BackupRead {
                name: "userdata".into(),
                offset: 0
            }
        ),
        "error:not-allowed",
        "not a listed partition"
    );
    // At the size exactly: nothing left to read
    let got = c.send(
        &mut p,
        HostMsg::BackupRead {
            name: "persist".into(),
            offset: 1024,
        },
    );
    assert_eq!(
        got.iter().map(Got::name).collect::<Vec<_>>(),
        ["data-begin", "data-end"]
    );
    // Verify needs the read's end, and a mismatch needs a new read
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::BackupVerify {
                name: "modem_a".into(),
                sha256: [0; 32]
            }
        ),
        "error:not-allowed"
    );
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::BackupVerify {
                name: "persist".into(),
                sha256: [0; 32]
            }
        ),
        "error:backup-mismatch"
    );
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::BackupVerify {
                name: "persist".into(),
                sha256: sha256(&parts()[1].1)
            }
        ),
        "error:not-allowed"
    );
    assert_eq!(p.phone.wrong_tries(), 0);
}

#[test]
fn the_state_file_carries_the_install_across_a_daemon_restart() {
    // Every state file the straight path writes, read back as a file and
    // started from: the stage is the one written, and never goes back.
    let (mut p, mut c) = build("provisioned");
    assert_eq!(c.one(&mut p, HostMsg::Finish), "finished");
    let files: Vec<Persistent> = p
        .log
        .iter()
        .chain(build("ready to provision").0.log.iter())
        .filter_map(|e| match e {
            Effect::Persist(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(files.len() > 5);
    for s in files {
        let text = s.to_json();
        assert_eq!(Persistent::from_json(text.as_bytes()).unwrap(), s, "{text}");
        assert!(
            !text.contains(&omamobile_link::digest::to_hex(&c.token.unwrap().0)),
            "the token is not in the file, only its digest"
        );
        let again = sim_phone(s.clone(), false, false, 8);
        assert_eq!(again.phone.stage(), s.stage);
        assert_eq!(again.finished, s.stage == Stage::Finished);
    }
    // A file that is not one is refused, not guessed at
    for bad in [
        "",
        "{}",
        r#"{"v":2,"stage":"waiting"}"#,
        r#"{"v":1,"stage":"ready","boot":1,"late_boots":0,"backup_done":false,"provision":"none","provision_failed":"","firstboot_failed":[]}"#,
    ] {
        assert!(Persistent::from_json(bad.as_bytes()).is_err(), "{bad}");
    }
    let ok = r#"{"v":1,"stage":"waiting","boot":1,"late_boots":0,"backup_done":false,"provision":"none","provision_failed":"","firstboot_failed":[]}"#;
    assert_eq!(
        Persistent::from_json(ok.as_bytes()).unwrap(),
        Persistent::default()
    );
    assert_eq!(Persistent::default().to_json(), ok);
}

#[test]
fn hello_starts_the_numbering_again_both_ways() {
    let (mut p, mut c) = build("waiting");
    for _ in 0..3 {
        assert_eq!(c.one(&mut p, HostMsg::Ping), "pong");
    }
    assert!(c.seq > 3 && c.rx > 3);
    // An installer that was killed and started again knows no numbers
    let mut again = Client::new();
    again.token = c.token;
    again.hello(&mut p);
    assert_eq!((again.seq, again.rx), (1, 1));
    assert_eq!(
        again.one(
            &mut p,
            HostMsg::Resume {
                token: again.token.unwrap()
            }
        ),
        "paired"
    );
    assert_eq!(p.phone.wrong_tries(), 0);
    // Each session has its own nonce, so its own serial proof
    assert_ne!(again.nonce, c.nonce);
}

#[test]
fn the_smaller_max_data_is_the_sessions() {
    let (mut p, mut c) = up();
    let h = c.hello(&mut p); // asks for 4096; the phone's own is 1024
    assert_eq!(h.max_data, 1024);
    assert_eq!(p.phone.max_data(), 1024);
    let mut cfg = phone_config();
    cfg.max_data = 1 << 20;
    let mut p = SimPhone::new(cfg, Persistent::default(), true, 0, 1);
    p.feed(0, PhoneInput::LinkUp);
    assert_eq!(c.hello(&mut p).max_data, 4096);
}
