// SPDX-License-Identifier: MIT
//! Both state machines walked at random: frames in and out of turn, noise,
//! the link coming and going, the clock, and a shell that answers anything
//! to anything. Nothing panics, everything a machine sends is a frame and a
//! message the other side's reader takes, and the stage never goes back.
mod common;
use common::rig::*;
use common::*;
use omamobile_link::digest::{serial_proof, sha256};
use omamobile_link::frame::{self, Kind, MAX_DATA_LIMIT};
use omamobile_link::host::*;
use omamobile_link::json::Value;
use omamobile_link::msg::*;
use omamobile_link::phone::*;
use omamobile_link::sim::TestRng;

fn rank(s: Stage) -> u8 {
    match s {
        Stage::Waiting => 0,
        Stage::Firstboot | Stage::FirstbootFailed => 1,
        Stage::FirstbootDone => 2,
        Stage::Provisioned => 3,
        Stage::Finished => 4,
    }
}

fn pick<T: Clone>(rng: &mut TestRng, from: &[T]) -> T {
    from[(rng.next_u64() % from.len() as u64) as usize].clone()
}

fn outcome(rng: &mut TestRng) -> Outcome {
    let n = (rng.next_u64() % 2000) as usize;
    match rng.next_u64() % 14 {
        0 => Outcome::Failed,
        1 | 2 => Outcome::Done,
        3 => Outcome::Facts(Facts {
            battery: rng.next_u64() as u8,
            charging: rng.next_u64().is_multiple_of(2),
            release: "r\u{e9}\n".into(),
            ..Facts::default()
        }),
        4 => Outcome::Problems(vec![]),
        5 => Outcome::Problems(vec![omamobile_link::answers::Problem::new(
            omamobile_link::answers::Field::Email,
            "x",
        )]),
        6 => Outcome::Parts(parts().into_iter().map(|p| p.0).collect()),
        7 => Outcome::Parts(vec![PartFound {
            name: "bad name".into(),
            size: u64::MAX,
            checked: true,
            dev: String::new(),
        }]),
        8 => Outcome::Precondition(vec!["efs".into(), "not a name".into()]),
        9 => Outcome::Data(rng.bytes(n)),
        10 => Outcome::Digest([rng.next_u64() as u8; 32]),
        11 => Outcome::FactoryOpened {
            have: rng.next_u64() % 20000,
        },
        12 => Outcome::NoSpace {
            need: u64::MAX,
            free: 0,
        },
        _ => Outcome::Report(Value::Arr(vec![Value::Num("1e9".into())])),
    }
}

/// What a shell that works would answer.
fn right_outcome(e: &Effect) -> Outcome {
    let part = |name: &str| {
        parts()
            .into_iter()
            .find(|p| p.0.name == name)
            .map(|p| p.1)
            .unwrap_or_default()
    };
    match e {
        Effect::ReadFacts { .. } => Outcome::Facts(Facts {
            battery: 90,
            charging: true,
            release: "1".into(),
            ..Facts::default()
        }),
        Effect::CheckAnswers { .. } => Outcome::Problems(vec![]),
        Effect::BackupList { .. } => Outcome::Parts(parts().into_iter().map(|p| p.0).collect()),
        Effect::BackupRead {
            name, offset, len, ..
        } => Outcome::Data(part(name)[*offset as usize..*offset as usize + *len as usize].to_vec()),
        Effect::BackupDigest { name, .. } => Outcome::Digest(sha256(&part(name))),
        Effect::FactoryOpen { resume_at, .. } => Outcome::FactoryOpened { have: *resume_at },
        Effect::FactoryDigest { .. } => Outcome::Digest(sha256(&image())),
        Effect::Smoke { .. } => Outcome::Report(Value::Obj(vec![])),
        _ => Outcome::Done,
    }
}

fn ticket_of(e: &Effect) -> Option<u64> {
    match e {
        Effect::ReadFacts { ticket }
        | Effect::CheckAnswers { ticket, .. }
        | Effect::StartFirstboot { ticket, .. }
        | Effect::BackupList { ticket }
        | Effect::BackupRead { ticket, .. }
        | Effect::BackupDigest { ticket, .. }
        | Effect::BackupKeep { ticket, .. }
        | Effect::FactoryOpen { ticket, .. }
        | Effect::FactoryDigest { ticket }
        | Effect::Smoke { ticket } => Some(*ticket),
        _ => None,
    }
}

#[test]
fn the_phone_walked_at_random() {
    let samples: Vec<HostMsg> = host_samples().into_iter().map(|(_, f)| f.msg).collect();
    let mut sent_types = std::collections::BTreeSet::new();
    let mut stages = std::collections::BTreeSet::new();
    let mut steps = 0u32;
    for seed in 0..60u64 {
        let mut rng = TestRng(seed);
        let mut entropy = TestRng(seed + 1000);
        let (mut phone, out) =
            Phone::start(phone_config(), Persistent::default(), true, 0, &mut entropy);
        let mut pending: Vec<PhoneOutput> = out;
        let (mut now, mut seq, mut id) = (0u64, 0u32, 0u64);
        let mut asked: Vec<Effect> = Vec::new();
        let mut code = phone.code();
        let mut token: Option<Token> = None;
        let mut top = 0u8;
        let mut stream: Option<u64> = None;
        let mut helloed = false;
        let mut backup_step = 0usize;
        let mut on_disk = Persistent::default();
        let mut restart: Option<bool> = None;
        for _ in 0..3000 {
            steps += 1;
            // Read what the machine put out
            for o in pending.drain(..) {
                match o {
                    PhoneOutput::Send(bytes) => {
                        let f = frame::decode(&bytes, MAX_DATA_LIMIT).expect("a valid frame");
                        if f.kind == Kind::Message {
                            let m = PhoneFrame::parse(f.payload).expect("a valid message");
                            sent_types.insert(m.msg.t());
                            match m.msg {
                                PhoneMsg::Paired { token: Some(t), .. } => token = Some(t),
                                PhoneMsg::FactoryReady { offset } => stream = Some(offset),
                                PhoneMsg::Error(_)
                                | PhoneMsg::FactoryVerified
                                | PhoneMsg::Aborted => stream = None,
                                _ => {}
                            }
                        }
                    }
                    PhoneOutput::Do(Effect::Screen(Screen::Code(c))) => code = Some(c),
                    PhoneOutput::Do(Effect::Persist(p)) => {
                        assert!(
                            rank(p.stage) >= top,
                            "seed {seed}: the stage went back to {:?}",
                            p.stage
                        );
                        top = rank(p.stage);
                        stages.insert(p.stage.as_str());
                        assert_eq!(Persistent::from_json(p.to_json().as_bytes()).unwrap(), p);
                        on_disk = p;
                    }
                    PhoneOutput::Do(Effect::Restart) => restart = Some(true),
                    PhoneOutput::Do(Effect::Stop) => restart = Some(false),
                    PhoneOutput::Do(e) => {
                        if ticket_of(&e).is_some() {
                            asked.push(e);
                        }
                    }
                }
            }
            if let Some(new_boot) = restart.take() {
                // The planned restart, or the daemon started again after a
                // fault: a new machine from the state file
                let mut cfg = phone_config();
                cfg.code_screen = !new_boot;
                let (p, out) = Phone::start(cfg, on_disk.clone(), new_boot, now, &mut entropy);
                phone = p;
                pending = out;
                asked.clear();
                stream = None;
                helloed = false;
                if new_boot {
                    code = None;
                }
                phone.handle(now, PhoneInput::LinkUp, &mut entropy);
                continue;
            }
            if phone.stopped() {
                break;
            }
            let send = |msg: HostMsg, seq: &mut u32, id: &mut u64| {
                *id += 1;
                let json = HostFrame { id: *id, msg }.to_json().unwrap();
                let bytes =
                    frame::encode(Kind::Message, *seq, 0, json.as_bytes(), MAX_DATA).unwrap();
                *seq += 1;
                bytes
            };
            let hello = HostMsg::Hello(HostHello {
                proto: vec![1],
                installer_name: "walk".into(),
                installer_version: "1".into(),
                max_data: MAX_DATA,
            });
            let input_bytes;
            let roll = rng.next_u64() % 100;
            // Most of the time: a shell that works and an installer that
            // does the next right thing. The rest: anything.
            let input = if roll < 30 && !asked.is_empty() {
                let e = asked.remove(0);
                PhoneInput::Done(ticket_of(&e).unwrap(), right_outcome(&e))
            } else if roll < 70 {
                let st = phone.state().clone();
                let msg = if !phone.paired() {
                    if !helloed {
                        helloed = true;
                        seq = 0;
                        stream = None;
                        Some(hello)
                    } else {
                        helloed = false;
                        match (code, token) {
                            (_, Some(token)) if rng.next_u64().is_multiple_of(2) => {
                                Some(HostMsg::Resume { token })
                            }
                            (Some(code), _) => Some(HostMsg::Pair { code }),
                            (None, Some(token)) => Some(HostMsg::Resume { token }),
                            _ => None,
                        }
                    }
                } else {
                    helloed = false;
                    match st.stage {
                        Stage::Waiting => Some(HostMsg::Answers(answers().get().clone())),
                        Stage::Firstboot => None,
                        Stage::FirstbootFailed => Some(HostMsg::FirstbootRetry),
                        Stage::FirstbootDone if !st.backup_done => {
                            let all = parts();
                            backup_step = (backup_step + 1) % (2 + 2 * all.len());
                            Some(match backup_step {
                                1 => HostMsg::BackupList,
                                0 => HostMsg::BackupCommit,
                                n if n % 2 == 0 => HostMsg::BackupRead {
                                    name: all[n / 2 - 1].0.name.clone(),
                                    offset: 0,
                                },
                                n => HostMsg::BackupVerify {
                                    name: all[n / 2 - 1].0.name.clone(),
                                    sha256: sha256(&all[n / 2 - 1].1),
                                },
                            })
                        }
                        Stage::FirstbootDone
                            if !st.factory.as_ref().is_some_and(|f| f.verified) =>
                        {
                            match stream {
                                None => Some(HostMsg::FactoryBegin(factory_begin(&image()))),
                                Some(at) if at as usize == IMAGE_LEN => Some(HostMsg::FactoryEnd),
                                Some(_) => None,
                            }
                        }
                        Stage::FirstbootDone if st.provision != JobState::Running => {
                            Some(HostMsg::Provision)
                        }
                        Stage::FirstbootDone => None,
                        _ if st.boot == 2 && rng.next_u64().is_multiple_of(2) => {
                            Some(HostMsg::Smoke)
                        }
                        _ if rng.next_u64().is_multiple_of(3) => Some(HostMsg::Finish),
                        _ => Some(HostMsg::Restart),
                    }
                };
                match (msg, st.stage, stream) {
                    (Some(msg), _, _) => {
                        input_bytes = send(msg, &mut seq, &mut id);
                        PhoneInput::Frame(&input_bytes)
                    }
                    (None, Stage::Firstboot, _) => PhoneInput::FirstbootEnded {
                        failed: if rng.next_u64().is_multiple_of(4) {
                            vec!["SSH keys".into()]
                        } else {
                            vec![]
                        },
                    },
                    (None, _, Some(at)) if (at as usize) < IMAGE_LEN && phone.paired() => {
                        let img = image();
                        let end = (at as usize + 1024).min(img.len());
                        input_bytes =
                            frame::encode(Kind::Data, seq, at, &img[at as usize..end], MAX_DATA)
                                .unwrap();
                        seq += 1;
                        stream = Some(end as u64);
                        PhoneInput::Frame(&input_bytes)
                    }
                    (None, Stage::FirstbootDone, _) if st.provision == JobState::Running => {
                        PhoneInput::ProvisionEnded {
                            steps: vec![StepResult {
                                step: "camera".into(),
                                state: StepState::Failed,
                            }],
                            failed: if rng.next_u64().is_multiple_of(4) {
                                Some("modem".into())
                            } else {
                                None
                            },
                        }
                    }
                    _ => PhoneInput::Tick,
                }
            } else {
                match rng.next_u64() % 40 {
                    0 if rng.next_u64().is_multiple_of(3) => PhoneInput::LinkUp,
                    1 if rng.next_u64().is_multiple_of(6) => PhoneInput::LinkDown,
                    2 => {
                        seq = 0;
                        input_bytes = send(hello, &mut seq, &mut id);
                        PhoneInput::Frame(&input_bytes)
                    }
                    4 if rng.next_u64().is_multiple_of(4) => {
                        let n = (rng.next_u64() % 100) as usize;
                        input_bytes = rng.bytes(n);
                        PhoneInput::Frame(&input_bytes)
                    }
                    5 | 6 => {
                        now += pick(&mut rng, &[1, 4_999, 5_000, 30_000, 119_000, 600_000]);
                        PhoneInput::Tick
                    }
                    7..=9 if !asked.is_empty() => {
                        let e = asked.remove((rng.next_u64() % asked.len() as u64) as usize);
                        PhoneInput::Done(ticket_of(&e).unwrap(), outcome(&mut rng))
                    }
                    14 => PhoneInput::Done(rng.next_u64() % 50, outcome(&mut rng)),
                    15 => PhoneInput::FirstbootEnded {
                        failed: if rng.next_u64().is_multiple_of(3) {
                            vec!["\u{1}".into()]
                        } else {
                            vec![]
                        },
                    },
                    16 if rng.next_u64().is_multiple_of(4) => PhoneInput::ProvisionEnded {
                        steps: vec![StepResult {
                            step: "not a step\n".into(),
                            state: StepState::Failed,
                        }],
                        failed: if rng.next_u64().is_multiple_of(3) {
                            Some("m\u{e9}".into())
                        } else {
                            None
                        },
                    },
                    17 => PhoneInput::Progress(Progress {
                        what: What::Provision,
                        step: "s".into(),
                        done: 1,
                        total: 2,
                    }),
                    18 => PhoneInput::DesktopUp,
                    19 => PhoneInput::CodeScreen(!rng.next_u64().is_multiple_of(8)),
                    20 if rng.next_u64().is_multiple_of(100) => PhoneInput::Fault,
                    21 | 22 => {
                        // A data frame, in or out of place
                        let img = image();
                        let at = rng.next_u64() % 9000;
                        input_bytes = frame::encode(
                            Kind::Data,
                            seq,
                            at,
                            &img[at as usize..at as usize + 100],
                            MAX_DATA,
                        )
                        .unwrap();
                        seq += 1;
                        PhoneInput::Frame(&input_bytes)
                    }
                    _ => {
                        let msg = match pick(&mut rng, &samples) {
                            HostMsg::Hello(_) => HostMsg::Ping,
                            HostMsg::Resume { token: t } => HostMsg::Resume {
                                token: token.unwrap_or(t),
                            },
                            HostMsg::BackupRead { name, .. } => HostMsg::BackupRead {
                                name,
                                offset: pick(&mut rng, &[0, 1024, 1, 4096]),
                            },
                            HostMsg::Pair { .. } | HostMsg::Finish => HostMsg::Status,
                            m => m,
                        };
                        input_bytes = send(msg, &mut seq, &mut id);
                        PhoneInput::Frame(&input_bytes)
                    }
                }
            };
            pending = phone.handle(now, input, &mut entropy);
        }
    }
    // The walk got everywhere: every stage was reached, and every message
    // of the phone's was sent
    assert_eq!(stages.len(), 6, "{stages:?}");
    let missing: Vec<&&str> = PHONE_TYPES
        .iter()
        .filter(|t| !sent_types.contains(**t))
        .collect();
    assert!(missing.is_empty(), "never sent: {missing:?}");
    assert!(steps > 50_000, "{steps}");
    eprintln!("{steps} steps");
}

/// What a phone that keeps to the protocol answers to a message.
fn right_answer(m: &HostFrame) -> Vec<PhoneFrame> {
    let re = Some(m.id);
    let one = |msg: PhoneMsg| vec![PhoneFrame { re, msg }];
    let sample = |name: &str| {
        phone_samples()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1
            .msg
    };
    match &m.msg {
        HostMsg::Hello(_) => {
            let PhoneMsg::Hello(mut h) = sample("hello") else {
                unreachable!()
            };
            h.max_data = MAX_DATA;
            one(PhoneMsg::Hello(h))
        }
        HostMsg::Ping => one(PhoneMsg::Pong),
        HostMsg::Pair { .. } => one(PhoneMsg::Paired {
            token: Some(Token([4; 32])),
            serial_proof: serial_proof(&digest(0x40), SERIAL),
        }),
        HostMsg::Resume { .. } => one(PhoneMsg::Paired {
            token: None,
            serial_proof: serial_proof(&digest(0x40), SERIAL),
        }),
        HostMsg::Bye => one(PhoneMsg::Bye),
        HostMsg::Status => {
            let mut s = status();
            let n = m.id % 6;
            s.stage = Stage::ALL[n as usize];
            s.firstboot.state = *JobState::ALL.get((m.id % 4) as usize).unwrap();
            s.provision_state = *JobState::ALL.get((m.id / 4 % 4) as usize).unwrap();
            s.boot = 1 + (m.id % 2) as u8;
            one(PhoneMsg::Status(Box::new(s)))
        }
        HostMsg::Answers(_) | HostMsg::FirstbootRetry => one(PhoneMsg::AnswersOk),
        HostMsg::BackupList => one(PhoneMsg::BackupParts {
            parts: parts()
                .into_iter()
                .map(|p| BackupPart {
                    name: p.0.name,
                    size: p.0.size,
                    checked: p.0.checked,
                })
                .collect(),
        }),
        HostMsg::BackupRead { name, offset } => {
            let size = parts()
                .into_iter()
                .find(|p| p.0.name == *name)
                .map_or(0, |p| p.0.size);
            vec![
                PhoneFrame {
                    re,
                    msg: PhoneMsg::DataBegin {
                        name: name.clone(),
                        size,
                        offset: *offset,
                    },
                },
                PhoneFrame {
                    re: None,
                    msg: PhoneMsg::DataEnd { name: name.clone() },
                },
            ]
        }
        HostMsg::BackupVerify { .. } => one(if m.id.is_multiple_of(5) {
            PhoneMsg::Error(ErrorMsg::new(
                ErrorCode::BackupMismatch,
                Stage::FirstbootDone,
            ))
        } else {
            PhoneMsg::BackupVerified
        }),
        HostMsg::BackupCommit => one(PhoneMsg::BackupDone),
        HostMsg::Abort => one(PhoneMsg::Aborted),
        HostMsg::FactoryBegin(_) => one(PhoneMsg::FactoryReady {
            offset: (m.id % 3) * 4096,
        }),
        HostMsg::FactoryEnd => one(PhoneMsg::FactoryVerified),
        HostMsg::Provision => vec![
            PhoneFrame {
                re,
                msg: PhoneMsg::ProvisionStarted,
            },
            PhoneFrame {
                re: None,
                msg: PhoneMsg::ProvisionDone { steps: vec![] },
            },
        ],
        HostMsg::Restart => one(PhoneMsg::Restarting),
        HostMsg::Smoke => one(PhoneMsg::SmokeReport {
            report: Value::Null,
        }),
        HostMsg::Finish => one(PhoneMsg::Finished),
    }
}

#[test]
fn the_host_walked_at_random() {
    let samples: Vec<PhoneFrame> = phone_samples().into_iter().map(|(_, f)| f).collect();
    let commands = [
        Command::Pair(Code::parse("123456").unwrap()),
        Command::Status,
        Command::Answers(answers()),
        Command::FirstbootRetry,
        Command::AwaitFirstboot,
        Command::Backup,
        Command::Factory(factory_begin(&image())),
        Command::Provision,
        Command::Restart,
        Command::Smoke,
        Command::Finish,
        Command::Abort,
        Command::Bye,
    ];
    let mut sent_types = std::collections::BTreeSet::new();
    let mut events = std::collections::BTreeSet::new();
    let mut steps = 0u32;
    for seed in 0..60u64 {
        let mut rng = TestRng(seed);
        let mut host = Host::new(host_config());
        let (mut now, mut seq, mut last_id) = (0u64, 0u32, 0u64);
        let mut asked: Vec<HostEffect> = Vec::new();
        let mut answers_due: std::collections::VecDeque<PhoneFrame> = Default::default();
        let mut pending: Vec<HostOutput> = Vec::new();
        for _ in 0..3000 {
            steps += 1;
            for o in pending.drain(..) {
                match o {
                    HostOutput::Send(bytes) => {
                        let f = frame::decode(&bytes, MAX_DATA_LIMIT).expect("a valid frame");
                        if f.kind == Kind::Message {
                            let m = HostFrame::parse(f.payload).expect("a valid message");
                            last_id = m.id;
                            sent_types.insert(m.msg.t());
                            if m.msg.t() == "hello" {
                                assert_eq!(f.seq, 0);
                                seq = 0;
                                answers_due.clear();
                            }
                            answers_due.extend(right_answer(&m));
                        }
                    }
                    HostOutput::Do(e) => {
                        if matches!(
                            e,
                            HostEffect::BackupOpen { .. }
                                | HostEffect::BackupDigest { .. }
                                | HostEffect::FactoryRead { .. }
                        ) {
                            asked.push(e);
                        }
                    }
                    HostOutput::Event(e) => {
                        events.insert(
                            format!("{e:?}")
                                .split([' ', '(', '{'])
                                .next()
                                .unwrap()
                                .to_string(),
                        );
                    }
                }
            }
            if host.stopped() {
                // Another run of the installer, with or without a token
                let mut cfg = host_config();
                cfg.first_id = last_id + 100;
                cfg.token = (seed % 2 == 0).then_some(Token([4; 32]));
                host = Host::new(cfg);
                asked.clear();
                answers_due.clear();
            }
            let bytes;
            let roll = rng.next_u64() % 100;
            let input = if roll < 45 && !answers_due.is_empty() {
                // A phone that keeps to the protocol. A partition's bytes go
                // before its data-end.
                let f = answers_due.pop_front().unwrap();
                if let (PhoneMsg::DataEnd { name }, true) =
                    (&f.msg, !rng.next_u64().is_multiple_of(4))
                {
                    let part = parts()
                        .into_iter()
                        .find(|p| p.0.name == *name)
                        .map(|p| p.1)
                        .unwrap_or_default();
                    for (i, chunk) in part.chunks(MAX_DATA as usize).enumerate() {
                        let b = frame::encode(
                            Kind::Data,
                            seq,
                            (i * MAX_DATA as usize) as u64,
                            chunk,
                            MAX_DATA,
                        )
                        .unwrap();
                        seq += 1;
                        pending.extend(host.handle(now, HostInput::Frame(&b)));
                    }
                }
                bytes = frame::encode(
                    Kind::Message,
                    seq,
                    0,
                    f.to_json().unwrap().as_bytes(),
                    MAX_DATA,
                )
                .unwrap();
                seq += 1;
                HostInput::Frame(&bytes)
            } else if roll < 65 && !asked.is_empty() {
                // A shell that works
                match asked.remove(0) {
                    HostEffect::BackupOpen { ticket, .. } => {
                        HostInput::Done(ticket, HostOutcome::Have(0))
                    }
                    HostEffect::BackupDigest { ticket, .. } => {
                        HostInput::Done(ticket, HostOutcome::Digest([1; 32]))
                    }
                    HostEffect::FactoryRead {
                        ticket,
                        offset,
                        len,
                    } => {
                        let d = image()[offset as usize..offset as usize + len as usize].to_vec();
                        HostInput::Done(ticket, HostOutcome::Data(d))
                    }
                    _ => HostInput::Tick,
                }
            } else {
                match rng.next_u64() % 30 {
                    0 => HostInput::LinkUp,
                    1 if rng.next_u64().is_multiple_of(6) => HostInput::LinkDown,
                    2 => {
                        now += pick(&mut rng, &[1, 1000, 2000, 60_000]);
                        HostInput::Tick
                    }
                    3..=12 => HostInput::Command(pick(&mut rng, &commands)),
                    13 if !asked.is_empty() => {
                        let t = match asked.remove(0) {
                            HostEffect::BackupOpen { ticket, .. }
                            | HostEffect::BackupDigest { ticket, .. }
                            | HostEffect::FactoryRead { ticket, .. } => ticket,
                            _ => 0,
                        };
                        let n = pick(&mut rng, &[0usize, 5, 100, 1024]);
                        let o = match rng.next_u64() % 5 {
                            0 => HostOutcome::Failed,
                            1 => HostOutcome::Have(rng.next_u64() % 5000),
                            2 => HostOutcome::Digest([1; 32]),
                            _ => HostOutcome::Data(rng.bytes(n)),
                        };
                        HostInput::Done(t, o)
                    }
                    14 if rng.next_u64().is_multiple_of(10) => HostInput::Fault,
                    15 if rng.next_u64().is_multiple_of(4) => {
                        let n = (rng.next_u64() % 100) as usize;
                        bytes = rng.bytes(n);
                        HostInput::Frame(&bytes)
                    }
                    16 if rng.next_u64().is_multiple_of(4) => {
                        let n = pick(&mut rng, &[1usize, 10, 1024]);
                        bytes = frame::encode(
                            Kind::Data,
                            seq,
                            pick(&mut rng, &[0, 1024, 2048]),
                            &rng.bytes(n),
                            MAX_DATA,
                        )
                        .unwrap();
                        seq += 1;
                        HostInput::Frame(&bytes)
                    }
                    17..=19 => {
                        // A message of the phone's nobody asked for
                        let mut f = pick(&mut rng, &samples);
                        if let (Some(re), true) = (&mut f.re, !rng.next_u64().is_multiple_of(4)) {
                            *re = last_id;
                        }
                        bytes = frame::encode(
                            Kind::Message,
                            seq,
                            0,
                            f.to_json().unwrap().as_bytes(),
                            MAX_DATA,
                        )
                        .unwrap();
                        seq += 1;
                        HostInput::Frame(&bytes)
                    }
                    _ => HostInput::Tick,
                }
            };
            pending.extend(host.handle(now, input));
        }
    }
    let missing: Vec<&&str> = HOST_TYPES
        .iter()
        .filter(|t| **t != "ping" && !sent_types.contains(**t))
        .collect();
    assert!(missing.is_empty(), "never sent: {missing:?}");
    for e in [
        "NeedCode",
        "Paired",
        "LinkLost",
        "Stopped",
        "Refused",
        "Status",
        "NotNow",
        "AnswersAccepted",
        "BackupDone",
        "FactoryVerified",
        "ProvisionDone",
        "Restarting",
        "Restarted",
        "SmokeReport",
        "Finished",
        "Aborted",
        "Closed",
        "Failed",
        "FirstbootDone",
        "FirstbootFailed",
    ] {
        assert!(events.contains(e), "no {e} among {events:?}");
    }
    assert!(steps > 50_000, "{steps}");
    eprintln!("{steps} steps");
}
