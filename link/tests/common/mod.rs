// SPDX-License-Identifier: MIT
//! What the link's tests share: answers that first boot takes, and one of
//! every message of protocol 1 (the golden frames are made of these).
#![allow(dead_code)]
// A host written out by hand, for the phone's machine alone
pub mod client;
// The two machines, their stand-ins and the loopback between them
pub mod rig;

use omamobile_link::answers::{Answers, Field, Problem, ValidAnswers};
use omamobile_link::json::Value;
use omamobile_link::msg::*;

pub fn answers() -> ValidAnswers {
    Answers {
        username: "owner".into(),
        // The form of a sha512-crypt hash; not the hash of anything
        password_hash: format!("$6$saltsalt${}", "a".repeat(86)),
        credential: Some("pin".into()),
        hostname: Some("phone".into()),
        timezone: Some("Europe/Berlin".into()),
        wifi_ssid: Some("<SSID>".into()),
        wifi_psk: Some("a passphrase".into()),
        ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA owner@computer".into()],
        full_name: Some("An Owner".into()),
        email: Some("owner@example.org".into()),
    }
    .validate()
    .expect("the test's answers are valid")
}

pub fn digest(n: u8) -> [u8; 32] {
    core::array::from_fn(|i| n.wrapping_add(i as u8))
}

pub fn status() -> Status {
    Status {
        stage: Stage::FirstbootDone,
        boot: 1,
        release: "2026.11.08".into(),
        kernel: "7.3.0-rc6-omamobile".into(),
        device: "cheetah".into(),
        firstboot: FirstbootStatus {
            state: JobState::Done,
            step: "Omarchy setup".into(),
            n: 9,
            of: 9,
            failed: vec![],
        },
        backup_state: BackupState::Listed,
        backup_parts: vec![
            PartStatus {
                name: "efs".into(),
                size: 3000,
                verified: true,
            },
            PartStatus {
                name: "modem_a".into(),
                size: 2048,
                verified: false,
            },
        ],
        factory_state: FactoryState::Partial,
        factory_have: 4096,
        factory_size: 9316,
        provision_state: JobState::None,
        provision_step: String::new(),
        provision_n: 0,
        provision_of: 0,
        provision_optional_failed: vec![],
        link_boots_left: 3,
        link_closes_s: 0,
        battery: 80,
        charging: true,
        free: 100_000_000_000,
    }
}

/// Every message the host can send, by a name for its golden file.
pub fn host_samples() -> Vec<(&'static str, HostFrame)> {
    let a = answers().get().clone();
    let min = Answers {
        username: a.username.clone(),
        password_hash: a.password_hash.clone(),
        ..Answers::default()
    };
    let msgs = vec![
        (
            "hello",
            HostMsg::Hello(HostHello {
                proto: vec![1],
                installer_name: "omamobile-install".into(),
                installer_version: "0.0.1".into(),
                max_data: 1048576,
                nonce: digest(0x60),
            }),
        ),
        ("ping", HostMsg::Ping),
        (
            "pair",
            HostMsg::Pair {
                code: Code::parse("123456").unwrap(),
            },
        ),
        (
            "resume",
            HostMsg::Resume {
                token: Token(digest(0x10)),
            },
        ),
        ("bye", HostMsg::Bye),
        ("status", HostMsg::Status),
        ("answers", HostMsg::Answers(a)),
        ("answers-least", HostMsg::Answers(min)),
        ("firstboot-retry", HostMsg::FirstbootRetry),
        ("backup-list", HostMsg::BackupList),
        (
            "backup-read",
            HostMsg::BackupRead {
                name: "efs".into(),
                offset: 2097152,
            },
        ),
        (
            "backup-verify",
            HostMsg::BackupVerify {
                name: "efs".into(),
                sha256: digest(0x20),
            },
        ),
        ("backup-commit", HostMsg::BackupCommit),
        ("abort", HostMsg::Abort),
        (
            "factory-begin",
            HostMsg::FactoryBegin(FactoryBegin {
                size: 4_000_000_000,
                sha256: digest(0x30),
                build: "test.260101.001".into(),
                name: "cheetah-test.260101.001-factory.zip".into(),
            }),
        ),
        ("factory-end", HostMsg::FactoryEnd),
        ("provision", HostMsg::Provision),
        ("restart", HostMsg::Restart),
        ("smoke", HostMsg::Smoke),
        ("finish", HostMsg::Finish),
    ];
    msgs.into_iter()
        .enumerate()
        .map(|(i, (n, msg))| {
            (
                n,
                HostFrame {
                    id: i as u64 + 1,
                    msg,
                },
            )
        })
        .collect()
}

/// Every error code, with what it carries.
pub fn error_codes() -> Vec<ErrorCode> {
    vec![
        ErrorCode::Protocol,
        ErrorCode::Version {
            proto: vec![1],
            host_nonce: digest(0x60),
        },
        ErrorCode::Unknown,
        ErrorCode::NotAllowed,
        ErrorCode::NotPaired,
        ErrorCode::BadCode { left: 4 },
        ErrorCode::Locked { wait_s: 30 },
        ErrorCode::BadToken,
        ErrorCode::Problems {
            problems: vec![
                Problem::new(Field::Username, "reserved"),
                Problem::new(Field::Timezone, "no-such-zone"),
            ],
        },
        ErrorCode::Busy,
        ErrorCode::BackupPrecondition {
            parts: vec!["efs".into(), "persist".into()],
        },
        ErrorCode::BackupMismatch,
        ErrorCode::NoSpace {
            need: 30_000_000_000,
            free: 1_000_000_000,
        },
        ErrorCode::TooLarge,
        ErrorCode::FactoryMismatch,
        ErrorCode::ProvisionFailed {
            step: "modem".into(),
        },
        ErrorCode::FirstbootFailed {
            steps: vec!["SSH keys".into()],
        },
        ErrorCode::Battery,
        ErrorCode::Internal,
    ]
}

/// Every message the phone can send, by a name for its golden file.
pub fn phone_samples() -> Vec<(String, PhoneFrame)> {
    let hello = PhoneHello {
        proto: 1,
        link: "0.0.1".into(),
        device: "cheetah".into(),
        max_data: 1048576,
        nonce: digest(0x40),
        host_nonce: digest(0x60),
        stage: Stage::Waiting,
        pair: PairMode::Code,
        wait_s: 0,
    };
    let locked = PhoneHello {
        pair: PairMode::Locked,
        wait_s: 30,
        stage: Stage::Provisioned,
        ..hello.clone()
    };
    // After first boot: no code on show, the owner can ask for one
    let ask = PhoneHello {
        pair: PairMode::Ask,
        stage: Stage::FirstbootDone,
        ..hello.clone()
    };
    let mut after = status();
    after.stage = Stage::Provisioned;
    after.boot = 2;
    after.backup_state = BackupState::Done;
    after.backup_parts = vec![];
    after.factory_state = FactoryState::None;
    after.factory_have = 0;
    after.factory_size = 0;
    after.provision_state = JobState::Done;
    after.provision_optional_failed = vec!["camera".into()];
    after.link_boots_left = 2;
    after.link_closes_s = 600;
    let report = Value::Obj(vec![
        ("device".into(), Value::Str("cheetah".into())),
        (
            "required".into(),
            Value::Arr(vec![Value::Obj(vec![
                ("row".into(), Value::Str("root".into())),
                ("ok".into(), Value::Bool(true)),
            ])]),
        ),
        ("battery".into(), Value::Num("87.5".into())),
        ("note".into(), Value::Null),
    ]);
    let mut v: Vec<(String, PhoneFrame)> = vec![
        ("hello", Some(1), PhoneMsg::Hello(hello)),
        ("hello-locked", Some(1), PhoneMsg::Hello(locked)),
        ("hello-ask", Some(1), PhoneMsg::Hello(ask)),
        ("pong", Some(2), PhoneMsg::Pong),
        (
            "paired",
            Some(3),
            PhoneMsg::Paired {
                token: Some(Token(digest(0x10))),
                serial_proof: digest(0x50),
            },
        ),
        (
            "paired-resume",
            Some(4),
            PhoneMsg::Paired {
                token: None,
                serial_proof: digest(0x50),
            },
        ),
        ("bye", Some(5), PhoneMsg::Bye),
        ("status", Some(6), PhoneMsg::Status(Box::new(status()))),
        (
            "status-provisioned",
            Some(6),
            PhoneMsg::Status(Box::new(after)),
        ),
        ("answers-ok", Some(7), PhoneMsg::AnswersOk),
        (
            "backup-parts",
            Some(10),
            PhoneMsg::BackupParts {
                parts: vec![
                    BackupPart {
                        name: "efs".into(),
                        size: 3000,
                        checked: true,
                    },
                    BackupPart {
                        name: "devinfo".into(),
                        size: 10,
                        checked: false,
                    },
                ],
            },
        ),
        (
            "data-begin",
            Some(11),
            PhoneMsg::DataBegin {
                name: "efs".into(),
                size: 3000,
                offset: 1024,
            },
        ),
        ("data-end", None, PhoneMsg::DataEnd { name: "efs".into() }),
        ("backup-verified", Some(12), PhoneMsg::BackupVerified),
        ("backup-done", Some(13), PhoneMsg::BackupDone),
        ("aborted", Some(14), PhoneMsg::Aborted),
        (
            "factory-ready",
            Some(15),
            PhoneMsg::FactoryReady {
                offset: 67108864,
                ack_every: 67108864,
                window: 134217728,
            },
        ),
        ("ack", None, PhoneMsg::Ack { offset: 134217728 }),
        ("factory-verified", Some(16), PhoneMsg::FactoryVerified),
        ("provision-started", Some(17), PhoneMsg::ProvisionStarted),
        (
            "progress",
            None,
            PhoneMsg::Progress(Progress {
                what: What::Provision,
                step: "firmware".into(),
                done: 1,
                total: 6,
            }),
        ),
        (
            "provision-done",
            None,
            PhoneMsg::ProvisionDone {
                steps: vec![
                    StepResult {
                        step: "firmware".into(),
                        state: StepState::Ok,
                    },
                    StepResult {
                        step: "camera".into(),
                        state: StepState::Failed,
                    },
                ],
            },
        ),
        ("restarting", Some(18), PhoneMsg::Restarting),
        ("smoke-report", Some(19), PhoneMsg::SmokeReport { report }),
        ("finished", Some(20), PhoneMsg::Finished),
    ]
    .into_iter()
    .map(|(n, re, msg)| (n.to_string(), PhoneFrame { re, msg }))
    .collect();
    for code in error_codes() {
        // Errors that answer a message carry `re`; the ones that end a
        // session or a long step come by themselves
        let re = match code {
            ErrorCode::Protocol | ErrorCode::ProvisionFailed { .. } => None,
            _ => Some(21),
        };
        let name = format!("error-{}", code.as_str());
        v.push((
            name,
            PhoneFrame {
                re,
                msg: PhoneMsg::Error(ErrorMsg::new(code, Stage::FirstbootDone)),
            },
        ));
    }
    v
}
