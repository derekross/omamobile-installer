// SPDX-License-Identifier: MIT
//! The proofs INSTALL-LINK-PLAN §11.3 names for the flaws building the first
//! version of this crate found, as far as they run without a daemon:
//! `rerun` (F1), `stale` (F2), `pair-again` and `rights` (F3), `count` (F4),
//! the camera-only failure (F5) and the image's window (F6). `budget`,
//! `rows` and `kill` (F3, F7) are in `tests/startup.rs`.
//!
//! All of it in memory. What only real FunctionFS can show is said at each
//! test, and in docs/LINK.md.
mod common;
use common::client::*;
use common::rig::*;
use common::*;
use omamobile_link::frame::{self, Kind};
use omamobile_link::host::{Command, Event, Stop, REHELLO_MS, SILENCE_MS};
use omamobile_link::msg::*;
use omamobile_link::phone::{AgentLine, Effect, Note, PhoneInput, Screen};
use omamobile_link::sim::{Loopback, SimHost, SimPhone};

fn no_protocol_error(p: &SimPhone) -> bool {
    !p.notes
        .iter()
        .any(|n| matches!(n, Note::Frame(_) | Note::Message(_)))
}

/// The install's steps, as a flow asks for them.
fn steps() -> Vec<Command> {
    vec![
        Command::Answers(answers()),
        Command::AwaitFirstboot,
        Command::Backup,
        Command::Factory(factory_begin(&image())),
        Command::Provision,
    ]
}

/// Drive an install from nothing until provisioning has started, or until
/// the loopback pauses. Commands not yet asked for stay in `todo`.
fn drive(lb: &mut Loopback, todo: &mut Vec<Command>) {
    let paused = |lb: &Loopback| {
        lb.pause_at
            .is_some_and(|n| lb.delivered[0] + lb.delivered[1] >= n)
    };
    for _ in 0..200 {
        if paused(lb) || lb.phone.provision_starts > 0 {
            return;
        }
        if !lb.up {
            lb.connect();
            continue;
        }
        if lb.under_way() > 0 {
            lb.run();
            continue;
        }
        if lb.phone.phone.stage() == Stage::Firstboot {
            lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] });
            continue;
        }
        if !lb.host.host.ready() {
            // The code on the phone, typed by the owner
            if lb.host.events.last() == Some(&Event::NeedCode) {
                let code = lb.phone.phone.code().expect("a code on the screen");
                lb.command(Command::Pair(code));
                continue;
            }
            panic!("neither paired nor asking: {:?}", lb.host.events);
        }
        if lb.host.host.busy() {
            lb.advance(2_000);
            continue;
        }
        match todo.pop() {
            Some(cmd) => lb.command(cmd),
            None => return,
        }
    }
    panic!("stuck: {:?}", lb.host.events);
}

// --- F1: rerun ---------------------------------------------------------------

#[test]
fn rerun_an_installer_killed_at_any_frame_pairs_again_with_the_cable_untouched() {
    // An undisturbed install, to count its frames
    let mut lb = rig();
    let mut todo: Vec<Command> = steps().into_iter().rev().collect();
    drive(&mut lb, &mut todo);
    let total = lb.delivered[0] + lb.delivered[1];
    assert!(total > 60, "{total}");
    assert_eq!(lb.phone.provision_starts, 1);

    let (mut by_token, mut by_code) = (0, 0);
    for k in 1..total {
        // The same, until frame k has arrived. Then the installer dies:
        // its machine is gone, and with it every number it had counted.
        // The link stays up; whatever was under way stays under way.
        let mut lb = rig();
        lb.pause_at = Some(k);
        let mut todo: Vec<Command> = steps().into_iter().rev().collect();
        drive(&mut lb, &mut todo);
        assert_eq!(lb.delivered[0] + lb.delivered[1], k);
        lb.pause_at = None;
        let before = lb.phone.notes.len();
        // A second run, with what the first left in the work directory
        let mut cfg = host_config();
        cfg.token = lb.host.token;
        let mut again = SimHost::with_seed(cfg, 1000 + k);
        again.backup = core::mem::take(&mut lb.host.backup);
        again.factory = image();
        lb.rerun_host(again);
        lb.run();
        if !lb.host.host.ready() {
            // No token yet (it died before `paired` reached it): the code
            assert_eq!(
                lb.host.events.last(),
                Some(&Event::NeedCode),
                "{k}: {:?}",
                lb.host.events
            );
            let code = lb.phone.phone.code().unwrap();
            lb.command(Command::Pair(code));
            by_code += 1;
        } else {
            by_token += 1;
        }
        assert!(lb.host.host.ready(), "{k}: {:?}", lb.host.events);
        // Within its first three frames, with no error on either side and
        // nothing counted against it: no replug was needed
        assert!(
            lb.host.paired_after.is_some_and(|n| n <= 3),
            "{k}: paired after {:?} frames",
            lb.host.paired_after
        );
        assert!(
            !lb.saw(|e| matches!(e, Event::LinkLost | Event::Stopped(_))),
            "{k}: {:?}",
            lb.host.events
        );
        assert!(
            lb.phone.notes[before..]
                .iter()
                .all(|n| !matches!(n, Note::Frame(_) | Note::Message(_))),
            "{k}: {:?}",
            &lb.phone.notes[before..]
        );
        assert_eq!(lb.phone.phone.wrong_tries(), 0, "{k}");
        // ... and the install goes on to its end from where the phone is
        let mut todo: Vec<Command> = steps().into_iter().rev().collect();
        drive(&mut lb, &mut todo);
        assert_eq!(lb.phone.provision_starts, 1, "{k}");
        assert_eq!(lb.phone.firstboot_starts, 1, "{k}: the answers once");
        assert_eq!(lb.phone.factory_file, image(), "{k}");
        for (found, bytes) in parts() {
            assert_eq!(lb.host.backup[&found.name], bytes, "{k}: {}", found.name);
        }
        assert!(no_protocol_error(&lb.phone), "{k}: {:?}", lb.phone.notes);
        assert_eq!(lb.phone.phone.wrong_tries(), 0, "{k}");
    }
    assert!(by_token > 50 && by_code >= 2, "{by_token} {by_code}");
}

// --- F2: stale ---------------------------------------------------------------

/// The frames another run of the installer left: a `hello` with `id` 1 and
/// three more messages, all answered by the phone into an endpoint nobody
/// reads.
fn leave_stale_answers(lb: &mut Loopback) {
    lb.host_reads = false;
    let hello = HostFrame {
        id: 1,
        msg: HostMsg::Hello(HostHello {
            proto: vec![1],
            installer_name: "omamobile-install".into(),
            installer_version: "0.0.1".into(),
            max_data: MAX_DATA,
            nonce: [0xee; 32],
        }),
    };
    for (seq, f) in [
        hello,
        HostFrame {
            id: 2,
            msg: HostMsg::Status,
        },
        HostFrame {
            id: 3,
            msg: HostMsg::Ping,
        },
        HostFrame {
            id: 4,
            msg: HostMsg::Ping,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let json = f.to_json().unwrap();
        let bytes = frame::encode(Kind::Message, seq as u32, 0, json.as_bytes(), MAX_DATA).unwrap();
        lb.to_phone(&bytes);
    }
    assert_eq!(lb.unread.len(), 4, "hello, status, pong, pong");
}

#[test]
fn stale_answers_of_another_run_are_passed_over_and_the_proof_holds() {
    for with_token in [true, false] {
        let mut lb = paired();
        let token = lb.host.token;
        leave_stale_answers(&mut lb);
        // The second run: its first message has `id` 1 too
        let mut cfg = host_config();
        cfg.token = token.filter(|_| with_token);
        lb.rerun_host(SimHost::with_seed(cfg, 4242));
        lb.run();
        assert_eq!(lb.unread.len(), 5, "and the answer to its own hello");
        assert!(lb.host.events.is_empty());
        // Now it reads: the first run's hello answer comes first
        lb.host_reads_again();
        assert_eq!(lb.host.host.discarded(), 4, "four frames passed over");
        if !with_token {
            assert_eq!(lb.host.events.last(), Some(&Event::NeedCode));
            let code = lb.phone.phone.code().unwrap();
            lb.command(Command::Pair(code));
        }
        // The serial proof was made with this run's nonce and this
        // session's: the right phone is the right phone
        assert!(
            lb.host.host.ready(),
            "token {with_token}: {:?}",
            lb.host.events
        );
        assert!(!lb.saw(|e| matches!(e, Event::Stopped(_) | Event::LinkLost)));
        assert!(no_protocol_error(&lb.phone));
        assert_eq!(lb.phone.phone.wrong_tries(), 0);
        // And the session works
        lb.command(Command::Status);
        assert!(lb.saw(|e| matches!(e, Event::Status(_))));
    }
    // The other way round: a proof made for another run's nonce is not this
    // one's. Another phone is still stopped.
    let mut lb = rig();
    let mut cfg = host_config();
    cfg.serial = b"<OTHER-SERIAL>".to_vec();
    lb.host = SimHost::new(cfg);
    lb.connect();
    let code = lb.phone.phone.code().unwrap();
    lb.command(Command::Pair(code));
    assert!(lb.saw(|e| *e == Event::Stopped(Stop::WrongPhone)));
}

#[test]
fn stale_the_serial_proof_is_bound_to_both_nonces() {
    use omamobile_link::digest::{serial_proof, sha256, to_hex};
    // SHA-256("omil-serial-v1" || the host's 32 nonce bytes || the phone's
    // 32 nonce bytes || the serial's bytes), by the plan's §5.3 and not by
    // this crate's own function
    let (host, phone) = ([1u8; 32], [2u8; 32]);
    let mut all = b"omil-serial-v1".to_vec();
    all.extend_from_slice(&host);
    all.extend_from_slice(&phone);
    all.extend_from_slice(SERIAL);
    let proof = serial_proof(&host, &phone, SERIAL);
    assert_eq!(proof, sha256(&all));
    assert_eq!(
        to_hex(&proof),
        "d7360bdb3db98de8c4e1f9d967f6bcccb5bc3d641a3e5ed6a9a040b19de42e36"
    );
    // Another run's nonce, another session's nonce, another phone, or the
    // two nonces the other way round: another proof
    assert_ne!(proof, serial_proof(&[3; 32], &phone, SERIAL));
    assert_ne!(proof, serial_proof(&host, &[3; 32], SERIAL));
    assert_ne!(proof, serial_proof(&host, &phone, b"<OTHER-SERIAL>"));
    assert_ne!(proof, serial_proof(&phone, &host, SERIAL));
    // On the wire: the phone's `paired` carries exactly that, for the nonce
    // the host sent in this session's hello and the one the phone answered
    let mut p = new_phone(1);
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    let h = c.hello(&mut p);
    let code = p.phone.code().unwrap();
    let got = c.send(&mut p, HostMsg::Pair { code });
    let [Got::Msg(PhoneFrame {
        msg: PhoneMsg::Paired {
            serial_proof: sent, ..
        },
        ..
    })] = &got[..]
    else {
        panic!("{got:?}")
    };
    let mut all = b"omil-serial-v1".to_vec();
    all.extend_from_slice(&c.host_nonce);
    all.extend_from_slice(&h.nonce);
    all.extend_from_slice(SERIAL);
    assert_eq!(*sent, sha256(&all));
    assert_ne!(c.host_nonce, h.nonce);
}

// --- F3: pair-again ----------------------------------------------------------

/// A paired install at a stage, by name.
fn at(stage: &str) -> Loopback {
    let mut lb = paired();
    lb.command(Command::Answers(answers()));
    match stage {
        "firstboot" => return lb,
        "firstboot-failed" => {
            lb.phone_input(PhoneInput::FirstbootEnded {
                failed: vec!["SSH keys".into()],
            });
            return lb;
        }
        _ => lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] }),
    }
    if stage == "firstboot-done" {
        return lb;
    }
    lb.command(Command::Backup);
    if stage == "backup done" {
        return lb;
    }
    lb.command(Command::Factory(factory_begin(&image())));
    lb.command(Command::Provision);
    lb.phone_input(PhoneInput::ProvisionEnded {
        result: Some(result(None)),
    });
    assert_eq!(lb.phone.phone.stage(), Stage::Provisioned);
    if stage == "provisioned" {
        return lb;
    }
    assert_eq!(stage, "after the restart");
    lb.command(Command::Restart);
    reboot(&mut lb);
    lb.connect();
    assert!(lb.host.host.ready());
    lb
}

#[test]
fn pair_again_after_first_boot_with_the_token_lost() {
    for stage in [
        "firstboot",
        "firstboot-failed",
        "firstboot-done",
        "backup done",
        "provisioned",
        "after the restart",
    ] {
        let mut lb = at(stage);
        let old = lb.host.token.expect("paired");
        let screen = matches!(stage, "firstboot" | "firstboot-failed");
        // The page is reloaded: the token was in its memory only (Q12)
        lb.host.take_events();
        lb.rerun_host(SimHost::with_seed(host_config(), 7));
        lb.run();
        if screen {
            // The first-start screen still owns the display: the code is there
            assert_eq!(lb.host.events.last(), Some(&Event::NeedCode), "{stage}");
        } else {
            // The session is not up: no code can be shown now
            assert_eq!(
                lb.host.events.last(),
                Some(&Event::NeedSession),
                "{stage}: {:?}",
                lb.host.events
            );
            assert_eq!(lb.phone.phone.code(), None, "{stage}");
            // The owner logs in. The notice is there, and holds no code
            session_up(&mut lb.phone, lb.now);
            assert!(
                matches!(lb.phone.agent.last(), Some(AgentLine::Open { .. })),
                "{stage}: {:?}",
                lb.phone.agent
            );
            lb.advance(REHELLO_MS);
            assert_eq!(lb.host.events.last(), Some(&Event::NeedTap), "{stage}");
            assert_eq!(
                lb.phone.phone.code(),
                None,
                "{stage}: nothing until the tap"
            );
            // Unlocked, the owner taps it: the window shows a new code
            let code = tap(&mut lb.phone, lb.now);
            assert_eq!(
                lb.phone.agent.last(),
                Some(&AgentLine::Code { code, valid_s: 600 }),
                "{stage}"
            );
            lb.advance(REHELLO_MS);
            assert_eq!(lb.host.events.last(), Some(&Event::NeedCode), "{stage}");
            // The code is on the agent's line and nowhere else: not on the
            // first-start screen, not in a note
            assert!(!lb.phone.screens.contains(&Screen::Code(code)), "{stage}");
        }
        let code = lb.phone.phone.code().expect("a code");
        lb.command(Command::Pair(code));
        assert!(lb.host.host.ready(), "{stage}: {:?}", lb.host.events);
        // A new token; the notice and the window are gone
        let new = lb.host.token.expect("a token");
        assert_ne!(new.0, old.0, "{stage}");
        assert!(lb.saw(|e| matches!(e, Event::Paired { .. })));
        if !screen {
            assert_eq!(lb.phone.agent.last(), Some(&AgentLine::Paired), "{stage}");
            assert_eq!(lb.phone.phone.code(), None, "{stage}: used once");
        }
        assert_eq!(lb.phone.phone.wrong_tries(), 0, "{stage}");
        // The old token stopped working with the new code
        let mut cfg = host_config();
        cfg.token = Some(old);
        lb.rerun_host(SimHost::with_seed(cfg, 8));
        lb.run();
        assert!(
            lb.host.host.token().is_none() && lb.host.token.is_none(),
            "{stage}: forgotten"
        );
        assert!(!lb.host.host.ready(), "{stage}");
        assert_eq!(lb.phone.phone.wrong_tries(), 0, "{stage}: not counted");
        // ... and the new one works, from a run that kept it
        let mut cfg = host_config();
        cfg.token = Some(new);
        lb.rerun_host(SimHost::with_seed(cfg, 9));
        lb.run();
        assert!(lb.host.host.ready(), "{stage}: {:?}", lb.host.events);
        assert!(no_protocol_error(&lb.phone), "{stage}");
    }
}

#[test]
fn the_sessions_code_lasts_ten_minutes_or_until_its_window_closes() {
    let mut lb = at("firstboot-done");
    lb.cut();
    lb.host = SimHost::with_seed(host_config(), 5);
    session_up(&mut lb.phone, lb.now);
    lb.connect();
    assert_eq!(lb.host.events.last(), Some(&Event::NeedTap));
    // Shown, then the window is closed: the code is gone
    let first = tap(&mut lb.phone, lb.now);
    lb.phone_input(PhoneInput::AgentHide);
    assert_eq!(lb.phone.phone.code(), None);
    // Shown again: another code. Each tap is an act at the phone.
    let second = tap(&mut lb.phone, lb.now);
    assert_ne!(first, second);
    lb.advance(REHELLO_MS);
    assert_eq!(lb.host.events.last(), Some(&Event::NeedCode));
    // Ten minutes in the window: gone, and the notice is back
    lb.advance(600_000);
    assert_eq!(lb.phone.phone.code(), None);
    assert!(matches!(
        lb.phone.agent.last(),
        Some(AgentLine::Open { .. } | AgentLine::Closed { .. })
    ));
    // The agent goes away with a code on show (the session ended): gone too
    let mut lb = at("firstboot-done");
    lb.cut();
    session_up(&mut lb.phone, lb.now);
    tap(&mut lb.phone, lb.now);
    lb.phone_input(PhoneInput::AgentDown);
    assert_eq!(lb.phone.phone.code(), None);
    // No agent, no window: `show` from nowhere shows nothing
    lb.phone_input(PhoneInput::AgentShow);
    assert_eq!(lb.phone.phone.code(), None);
    // While a host is paired there is nothing to show a code for
    let mut lb = at("firstboot-done");
    session_up(&mut lb.phone, lb.now);
    assert_eq!(lb.phone.agent.last(), Some(&AgentLine::Paired));
    lb.phone_input(PhoneInput::AgentShow);
    assert_eq!(lb.phone.phone.code(), None);
    // And at the first-start screen the agent's lines mean nothing
    let mut lb = paired();
    lb.phone_input(PhoneInput::AgentUp);
    lb.phone_input(PhoneInput::AgentShow);
    assert!(lb.phone.agent.is_empty());
}

// --- F3: rights ----------------------------------------------------------------

/// The phone at one of the stages with an account, a host paired, and
/// everything done that the stage lets a host do: so that what is refused
/// below is refused for the stage, not for a step left out.
fn with_account(stage: &str) -> (SimPhone, Client) {
    let mut p = new_phone(1);
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    c.hello(&mut p);
    c.pair(&mut p);
    assert_eq!(
        c.one(&mut p, HostMsg::Answers(answers().get().clone())),
        "answers-ok"
    );
    if stage == "firstboot-failed" {
        p.feed(
            0,
            PhoneInput::FirstbootEnded {
                failed: vec!["SSH keys".into()],
            },
        );
    } else if stage != "firstboot" {
        p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
    }
    backup_all(&mut p, &mut c);
    if matches!(stage, "firstboot" | "firstboot-failed") {
        return (p, c);
    }
    assert_eq!(c.one(&mut p, HostMsg::BackupCommit), "backup-done");
    send_image(&mut p, &mut c);
    assert_eq!(c.one(&mut p, HostMsg::FactoryEnd), "factory-verified");
    if stage == "firstboot-done" {
        return (p, c);
    }
    assert_eq!(c.one(&mut p, HostMsg::Provision), "provision-started");
    let f = p.feed(
        0,
        PhoneInput::ProvisionEnded {
            result: Some(result(None)),
        },
    );
    assert_eq!(c.read(f)[0].name(), "provision-done");
    if stage == "provisioned, boot 1" {
        return (p, c);
    }
    assert_eq!(stage, "provisioned, boot 2");
    p.start(true, 0);
    p.feed(0, PhoneInput::LinkUp);
    c.hello(&mut p);
    assert_eq!(c.resume(&mut p), "paired");
    (p, c)
}

/// INSTALL-LINK-PLAN §2.7, written out from its table and the list under
/// it, not from the code: what a paired host gets at each stage with an
/// account.
fn right(stage: &str, t: &str) -> &'static str {
    let provisioned = stage.starts_with("provisioned");
    let no = "error:not-allowed";
    match t {
        // The session's own messages
        "ping" => "pong",
        "bye" => "bye",
        // "Read `status`": until the link ends
        "status" => "status",
        // "and at `boot` 2 the smoke report"
        "smoke" if stage == "provisioned, boot 2" => "smoke-report",
        // "Read the descriptor's radio partitions": until stage `provisioned`
        "backup-list" if !provisioned => "backup-parts",
        "backup-read" if !provisioned => "data-begin",
        "backup-verify" if !provisioned => "backup-verified",
        "backup-commit" if !provisioned => "backup-done",
        // "Send one file ... and have it checked, extracted and installed":
        // until stage `provisioned`
        "factory-begin" if !provisioned => "factory-ready",
        "provision" if stage == "firstboot-done" => "provision-started",
        "provision" if stage == "firstboot-failed" => "error:firstboot-failed",
        // "Start first-boot setup again with the answers the phone kept":
        // only at `firstboot-failed`
        "firstboot-retry" if stage == "firstboot-failed" => "answers-ok",
        // "Restart the phone": once, at `provisioned`, `boot` 1
        "restart" if stage == "provisioned, boot 1" => "restarting",
        // "End the link": `finish`, at `provisioned`
        "finish" if provisioned => "finished",
        // "A paired host cannot, at any stage with an account: send
        // answers"; nor pair again in a paired session; and everything else
        // out of its stage
        _ => no,
    }
}

#[test]
fn rights_of_a_paired_host_at_every_stage_with_an_account() {
    let stages = [
        "firstboot",
        "firstboot-failed",
        "firstboot-done",
        "provisioned, boot 1",
        "provisioned, boot 2",
    ];
    let (mut allowed, mut refused) = (0, 0);
    for stage in stages {
        for (name, sample) in host_samples() {
            let (mut p, mut c) = with_account(stage);
            let boot = p.state_file.boot;
            assert_eq!(boot == 2, stage == "provisioned, boot 2");
            let msg = match sample.msg {
                HostMsg::Hello(_) => continue, // a new session: tested as such
                HostMsg::Pair { .. } => HostMsg::Pair {
                    code: p.phone.code().unwrap_or(Code::parse("000000").unwrap()),
                },
                HostMsg::Resume { .. } => HostMsg::Resume {
                    token: c.token.unwrap(),
                },
                HostMsg::BackupRead { .. } => HostMsg::BackupRead {
                    name: "efs".into(),
                    offset: 0,
                },
                HostMsg::BackupVerify { .. } => HostMsg::BackupVerify {
                    name: "efs".into(),
                    sha256: omamobile_link::digest::sha256(&parts()[0].1),
                },
                HostMsg::FactoryBegin(_) => begin(&image()),
                other => other,
            };
            let t = msg.t();
            let before = (p.state_file.clone(), p.started_with.clone());
            p.log.clear();
            let got = c.one(&mut p, msg);
            let want = right(stage, t);
            assert_eq!(got, want, "{name} at {stage}");
            // No message can add or change an account, a password, a key,
            // a host name or a network on a phone that has an account
            assert_eq!(p.started_with, before.1, "{name} at {stage}");
            assert!(
                !p.log.iter().any(|e| matches!(
                    e,
                    Effect::CheckAnswers { .. } | Effect::StartFirstboot { retry: false, .. }
                )),
                "{name} at {stage}"
            );
            if want.starts_with("error:") {
                refused += 1;
                assert_eq!(p.state_file, before.0, "{name} at {stage}: nothing changed");
                let did: Vec<&Effect> = p
                    .log
                    .iter()
                    .filter(|e| !matches!(e, Effect::Note(_)))
                    .collect();
                assert!(did.is_empty(), "{name} at {stage}: {did:?}");
            } else {
                allowed += 1;
            }
        }
    }
    assert_eq!(allowed + refused, 5 * 19);
    assert!(allowed > 30 && refused > 40, "{allowed} {refused}");

    // `restart` is taken once: a paired host cannot restart a set-up phone
    // again and again
    let (mut p, mut c) = with_account("provisioned, boot 1");
    assert_eq!(c.one(&mut p, HostMsg::Restart), "restarting");
    assert_eq!(c.one(&mut p, HostMsg::Restart), "error:not-allowed");
    assert_eq!(p.restarts, 1);
    // ... also not when something else restarted the phone first
    let (mut p, mut c) = with_account("provisioned, boot 2");
    assert_eq!(c.one(&mut p, HostMsg::Restart), "error:not-allowed");
    assert_eq!(p.restarts, 0);

    // An unpaired host, at every such stage: `hello`, `ping`, `status` and
    // `bye` are answered, `resume` with the token, `pair` only while a code
    // is shown; nothing else
    for stage in stages {
        for (name, sample) in host_samples() {
            let (mut p, mut c) = with_account(stage);
            let token = c.token.unwrap();
            let screen = matches!(stage, "firstboot" | "firstboot-failed");
            let h = c.hello(&mut p);
            assert_eq!(
                h.pair,
                if screen {
                    PairMode::Code
                } else {
                    PairMode::Token
                },
                "{stage}"
            );
            let msg = match sample.msg {
                HostMsg::Hello(_) => continue,
                HostMsg::Resume { .. } => HostMsg::Resume { token },
                HostMsg::Pair { .. } => HostMsg::Pair {
                    code: p.phone.code().unwrap_or(Code::parse("000000").unwrap()),
                },
                other => other,
            };
            let want = match msg.t() {
                "ping" => "pong",
                "status" => "status",
                "bye" => "bye",
                "resume" => "paired",
                "pair" if screen => "paired",
                "pair" => "error:not-allowed",
                _ => "error:not-paired",
            };
            let before = p.state_file.clone();
            p.log.clear();
            assert_eq!(c.one(&mut p, msg), want, "{name} unpaired at {stage}");
            if want.starts_with("error:") {
                assert_eq!(p.state_file, before);
                assert!(p.log.iter().all(|e| matches!(e, Effect::Note(_))));
            }
        }
    }
}

#[test]
fn status_names_nothing_of_the_owners() {
    // "Neither names a user, a host name, a network, a serial or an
    // address": the answers' values are in no status the phone sends
    for stage in ["firstboot", "firstboot-done", "provisioned, boot 2"] {
        let (mut p, mut c) = with_account(stage);
        let got = c.send(&mut p, HostMsg::Status);
        let [Got::Msg(f)] = &got[..] else {
            panic!("{got:?}")
        };
        let json = f.to_json().unwrap();
        for secret in [
            "owner",
            "saltsalt",
            "phone\"",
            "Berlin",
            "SSID",
            "passphrase",
            "ssh-",
            "SERIAL",
            "example.org",
        ] {
            assert!(!json.contains(secret), "{secret} in {json}");
        }
    }
}

// --- F4: count -----------------------------------------------------------------

fn wrong_code(p: &SimPhone) -> Code {
    let right = p.phone.code().map(|c| c.digits().to_string());
    Code::parse(if right.as_deref() == Some("000000") {
        "000001"
    } else {
        "000000"
    })
    .unwrap()
}

#[test]
fn count_only_wrong_codes() {
    let mut p = new_phone(1);
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    // 100 frame errors, each in a session of its own
    for i in 0..100u32 {
        c.hello(&mut p);
        let bytes: &[u8] = match i % 4 {
            0 => b"garbage",
            1 => b"",
            2 => &[0x4f, 0x4d, 0x49, 0x4c, 9, 9, 9, 9],
            _ => b"OMIL but no frame at all, and long enough to hold a whole header .....",
        };
        let got = c.raw(&mut p, bytes);
        assert_eq!(got[0].name(), "error:protocol");
        // ... and a malformed message
        c.hello(&mut p);
        let got = c.payload(&mut p, r#"{"t":"pair","id":2,"code":"12345"}"#);
        assert_eq!(got[0].name(), "error:protocol");
    }
    assert_eq!(p.phone.wrong_tries(), 0, "200 protocol errors");
    c.hello(&mut p);
    c.pair(&mut p);
    let token = c.token.unwrap();
    // 100 wrong tokens
    for i in 0..100u8 {
        let mut other = Client::new();
        other.hello(&mut p);
        let mut t = token.0;
        t[(i % 32) as usize] ^= 1 << (i % 8);
        assert_eq!(
            other.one(&mut p, HostMsg::Resume { token: Token(t) }),
            "error:bad-token"
        );
        // The session stays: the right one is taken in it
        other.token = Some(token);
        assert_eq!(other.resume(&mut p), "paired");
    }
    assert_eq!(p.phone.wrong_tries(), 0, "100 wrong tokens");
    // On to a stage where the image goes
    c.hello(&mut p);
    assert_eq!(c.resume(&mut p), "paired");
    assert_eq!(
        c.one(&mut p, HostMsg::Answers(answers().get().clone())),
        "answers-ok"
    );
    p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
    // 3 images that are not the file announced: a damaged download
    for _ in 0..3 {
        let img = image();
        let mut b = factory_begin(&img);
        b.sha256[5] ^= 0x80;
        assert_eq!(c.one(&mut p, HostMsg::FactoryBegin(b)), "factory-ready");
        for (i, chunk) in img.chunks(MAX_DATA as usize).enumerate() {
            c.data(&mut p, i as u64 * MAX_DATA as u64, chunk);
        }
        assert_eq!(c.one(&mut p, HostMsg::FactoryEnd), "error:factory-mismatch");
        assert!(p.phone.paired(), "the session stays");
    }
    assert_eq!(p.phone.wrong_tries(), 0, "3 mismatched images");
    // A laptop that slept: the stream was suspended after 120 s, and the
    // data frame it then sends meets no open stream
    let img = image();
    assert_eq!(c.one(&mut p, begin(&img)), "factory-ready");
    c.data(&mut p, 0, &img[..1024]);
    c.now += 121_000;
    p.feed(c.now, PhoneInput::Tick);
    assert!(p.notes.contains(&Note::StreamIdle));
    let got = c.data(&mut p, 1024, &img[1024..2048]);
    assert_eq!(got[0].name(), "error:not-allowed");
    assert!(p.phone.paired());
    assert_eq!(p.phone.wrong_tries(), 0, "a data frame after 121 s");
    // After all that the right code pairs: here, the one the owner asks for
    session_up(&mut p, c.now);
    let mut fresh = Client::new();
    fresh.now = c.now;
    let h = fresh.hello(&mut p);
    assert_eq!((h.pair, h.wait_s), (PairMode::Ask, 0), "not locked");
    tap(&mut p, c.now);
    assert_eq!(fresh.hello(&mut p).pair, PairMode::Code);
    fresh.pair(&mut p);
    assert!(p.phone.paired());
}

#[test]
fn count_twenty_wrong_codes_lock_until_an_act_and_never_lock_resume() {
    // The same on the first-start screen (a volume key is the act) and in
    // the owner's session (the tap is)
    for session in [false, true] {
        let mut p = new_phone(1);
        p.feed(0, PhoneInput::LinkUp);
        let mut owner = Client::new();
        owner.hello(&mut p);
        owner.pair(&mut p);
        if session {
            assert_eq!(
                owner.one(&mut p, HostMsg::Answers(answers().get().clone())),
                "answers-ok"
            );
            p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
            session_up(&mut p, 0);
        }
        let token = owner.token;
        assert_eq!(owner.one(&mut p, HostMsg::Bye), "bye");
        // Another host on the cable, which cannot see the screen. (In the
        // session the owner's window is open: there is a code to guess at.)
        if session {
            tap(&mut p, 0);
        }
        let mut other = Client::new();
        let mut now = 0u64;
        let mut locks = Vec::new();
        for n in 1..=20u32 {
            other.now = now;
            assert_eq!(other.hello(&mut p).pair, PairMode::Code, "{n}");
            let before = p.phone.code();
            let code = wrong_code(&p);
            let got = other.send(&mut p, HostMsg::Pair { code });
            assert_eq!(p.phone.wrong_tries(), n);
            let [Got::Msg(PhoneFrame {
                msg: PhoneMsg::Error(e),
                ..
            })] = &got[..]
            else {
                panic!("{got:?}")
            };
            if n % 5 != 0 {
                assert_eq!(e.code, ErrorCode::BadCode { left: 5 - n % 5 });
                assert_eq!(p.phone.code(), before, "the same code until the fifth");
                continue;
            }
            let ErrorCode::Locked { wait_s } = e.code else {
                panic!("{:?}", e.code)
            };
            locks.push(wait_s);
            // `hello` says so, and `pair` is answered `locked`, uncounted,
            // the right code too
            let h = other.hello(&mut p);
            assert_eq!((h.pair, h.wait_s), (PairMode::Locked, wait_s), "{n}");
            if let Some(code) = p.phone.code() {
                assert_ne!(Some(code), before, "{n}: the code was replaced");
                assert_eq!(other.one(&mut p, HostMsg::Pair { code }), "error:locked");
            } else {
                assert_eq!(n, 20, "no code is shown after the twentieth");
                let any = Code::parse("123456").unwrap();
                assert_eq!(
                    other.one(&mut p, HostMsg::Pair { code: any }),
                    "error:locked"
                );
            }
            assert_eq!(p.phone.wrong_tries(), n, "{n}: not counted");
            // The owner's installer holds the token: it is never kept out.
            // (On a copy of the phone: a paired host closes the window, and
            // this test goes on guessing at it.)
            let mut q = p.clone();
            let mut owner = Client::new();
            owner.token = token;
            owner.now = now;
            assert_eq!(owner.hello(&mut q).pair, PairMode::Locked);
            assert_eq!(owner.resume(&mut q), "paired", "{n}: resume under the lock");
            assert_eq!(q.phone.wrong_tries(), n);
            if n < 20 {
                // A timed lock ends by the clock, not a second sooner
                let end = now + wait_s as u64 * 1000;
                p.feed(end - 1, PhoneInput::Tick);
                other.now = end - 1;
                assert_eq!(other.hello(&mut p).pair, PairMode::Locked, "{n}");
                p.feed(end, PhoneInput::Tick);
                other.now = end;
                assert_eq!(other.hello(&mut p).pair, PairMode::Code, "{n}");
                now = end;
            }
        }
        assert_eq!(locks, [30, 60, 120, 0], "session {session}");
        // The twentieth: until someone acts at the phone, however long
        // (in the session the link itself closes after ten unpaired
        // minutes, which is tests/startup.rs's `budget`: stay inside them)
        now += if session { 180_000 } else { 86_400_000 };
        p.feed(now, PhoneInput::Tick);
        other.now = now;
        let h = other.hello(&mut p);
        assert_eq!((h.pair, h.wait_s), (PairMode::Locked, 0));
        assert_eq!(p.phone.code(), None);
        if session {
            // The window closed, and the agent is told why (§2.8): `locked`,
            // and nothing after it until the owner acts
            assert_eq!(p.agent.last(), Some(&AgentLine::Locked));
            assert_eq!(AgentLine::Locked.line(), "locked");
        } else {
            assert!(!p.agent.contains(&AgentLine::Locked), "no agent there");
        }
        // The wrong act does nothing: a volume key in the session, the
        // agent at the first-start screen
        p.feed(
            now,
            if session {
                PhoneInput::Key
            } else {
                PhoneInput::AgentShow
            },
        );
        assert_eq!(p.phone.wrong_tries(), 20);
        // One act at the phone: the count from nothing, and a new code
        if session {
            tap(&mut p, now);
            assert!(matches!(p.agent.last(), Some(AgentLine::Code { .. })));
        } else {
            p.feed(now, PhoneInput::Key);
            assert!(matches!(p.screens.last(), Some(Screen::Code(_))));
        }
        assert_eq!(p.phone.wrong_tries(), 0);
        let h = other.hello(&mut p);
        assert_eq!((h.pair, h.wait_s), (PairMode::Code, 0));
        other.pair(&mut p);
        assert!(p.phone.paired(), "session {session}");
    }
}

#[test]
fn count_what_resets_it_and_what_does_not() {
    let mut p = new_phone(1);
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    c.hello(&mut p);
    for _ in 0..3 {
        let code = wrong_code(&p);
        c.send(&mut p, HostMsg::Pair { code });
    }
    // A right code does not
    c.pair(&mut p);
    assert_eq!(p.phone.wrong_tries(), 3);
    // `pair` while no code is shown is not allowed, and not counted
    assert_eq!(
        c.one(
            &mut p,
            HostMsg::Pair {
                code: Code::parse("000000").unwrap()
            }
        ),
        "error:not-allowed"
    );
    assert_eq!(p.phone.wrong_tries(), 3);
    // A restart of the daemon does (the count is in memory only)
    p.start(false, 0);
    assert_eq!(p.phone.wrong_tries(), 0);
}

// --- F5: a camera-only failure ---------------------------------------------------

#[test]
fn a_camera_only_failure_is_a_success_with_a_note() {
    let mut lb = at("backup done");
    lb.command(Command::Factory(factory_begin(&image())));
    lb.command(Command::Provision);
    lb.phone_input(PhoneInput::ProvisionEnded {
        result: Some(result(Some("camera"))),
    });
    // A success: the stage, the image gone, the event with `camera` failed
    assert_eq!(lb.phone.state_file.stage, Stage::Provisioned);
    assert!(!lb.phone.factory_exists && !lb.phone.work_dir);
    assert!(lb.saw(|e| matches!(e, Event::ProvisionDone { steps, optional_failed }
        if optional_failed == &["camera"] && steps.iter().any(|s| s.step == "camera" && s.state == StepState::Failed))));
    // `status` keeps saying so
    lb.host.take_events();
    lb.command(Command::Status);
    assert!(lb.saw(
        |e| matches!(e, Event::Status(s) if s.provision_optional_failed == ["camera"] && s.provision_state == JobState::Done)
    ));
    // `provision` again is not allowed: there is nothing to provision from
    let (mut p, mut c) = with_account("provisioned, boot 1");
    assert_eq!(c.one(&mut p, HostMsg::Provision), "error:not-allowed");
    assert_eq!(c.one(&mut p, begin(&image())), "error:not-allowed");
    // A required step that fails is a failure, whatever the camera did:
    // the image is kept and `provision` runs it again
    let mut lb = at("backup done");
    lb.command(Command::Factory(factory_begin(&image())));
    lb.command(Command::Provision);
    let mut r = result(Some("modem"));
    r[5].state = StepState::Failed;
    lb.phone_input(PhoneInput::ProvisionEnded { result: Some(r) });
    assert_eq!(lb.phone.state_file.stage, Stage::FirstbootDone);
    assert_eq!(lb.phone.state_file.provision_failed, "modem");
    assert!(lb.phone.factory_exists);
    // A worker that left no result: failed at `interrupted`
    lb.host.take_events();
    lb.command(Command::Provision);
    lb.phone_input(PhoneInput::ProvisionEnded { result: None });
    assert_eq!(lb.phone.state_file.provision_failed, "interrupted");
    assert!(lb.saw(|e| matches!(
        e,
        Event::Refused {
            step: "provision",
            ..
        }
    )));
}

// --- F6: the image's window ------------------------------------------------------

#[test]
fn window_a_host_that_does_not_read_stalls_itself_and_goes_on_when_it_reads() {
    // What an in-memory link can show of §5.7: the host machine never has
    // bytes sent beyond the last acknowledgement it has READ plus the
    // window, so a host whose reader is stuck stops at the window with the
    // phone's frames few and waiting, and nothing is lost when it reads
    // again. What it cannot show: that the phone's write of `ack` blocks
    // on real USB, the bound of 8 queued frames, the 30 s per write and the
    // cancelled request. Those are the two shells' and L3's `flow`.
    let mut lb = set_up();
    // Until `factory-ready` is read, then the reader stops
    lb.pause_at = Some(lb.delivered[0] + lb.delivered[1] + 2);
    lb.command(Command::Factory(factory_begin(&image())));
    lb.pause_at = None;
    lb.host_reads = false;
    lb.run();
    assert_eq!(
        lb.phone.factory_file.len() as u64,
        TEST_WINDOW,
        "stopped at the window's edge"
    );
    assert_eq!(lb.unread.len(), 2, "the acknowledgements at 4096 and 8192");
    assert!(lb.unread.len() <= 8, "the phone's queue stays short");
    assert!(lb.host.host.busy() && !lb.saw(|e| *e == Event::LinkLost));
    // It reads its acknowledgements five seconds late: the transfer goes on
    lb.advance(5_000);
    assert_eq!(lb.phone.factory_file.len() as u64, TEST_WINDOW);
    lb.host_reads_again();
    assert!(
        lb.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        lb.host.events
    );
    assert_eq!(lb.phone.factory_file, image());
    assert!(no_protocol_error(&lb.phone));

    // A minute at the window's edge is a lost link: `hello` again, and the
    // image goes on from the phone's last acknowledged offset
    let mut lb = set_up();
    lb.pause_at = Some(lb.delivered[0] + lb.delivered[1] + 2);
    lb.command(Command::Factory(factory_begin(&image())));
    lb.pause_at = None;
    lb.host_reads = false;
    lb.run();
    lb.advance(SILENCE_MS - 1);
    assert!(!lb.saw(|e| *e == Event::LinkLost));
    lb.advance(1);
    assert!(lb.saw(|e| *e == Event::LinkLost));
    assert_eq!(
        lb.phone.state_file.factory.as_ref().map(|f| f.synced),
        Some(TEST_WINDOW),
        "the file is at its last acknowledged offset"
    );
    lb.host.take_events();
    lb.host_reads_again();
    assert!(
        lb.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        lb.host.events
    );
    assert!(
        lb.saw(|e| *e
            == Event::FactorySent {
                sent: TEST_WINDOW + 1024,
                size: IMAGE_LEN as u64
            }),
        "from the acknowledged offset, not from zero"
    );
    assert_eq!(lb.phone.factory_file, image());
    assert_eq!(lb.phone.phone.wrong_tries(), 0);
}

#[test]
fn window_is_what_factory_ready_says_and_never_too_small() {
    // The phone says its numbers; the host holds to them
    let (mut p, mut c) = with_account("firstboot");
    let got = c.send(&mut p, begin(&image()));
    assert!(matches!(
        &got[..],
        [Got::Msg(PhoneFrame {
            msg: PhoneMsg::FactoryReady {
                offset: 0,
                ack_every: TEST_ACK_EVERY,
                window: TEST_WINDOW
            },
            ..
        })]
    ));
    // The real ones: 64 MiB, and twice that
    let cfg = omamobile_link::phone::PhoneConfig::new("0.0.1", "cheetah", SERIAL);
    assert_eq!((cfg.ack_every, cfg.window), (67108864, 134217728));
    // A daemon told a window that could never be left is not believed
    let mut cfg = phone_config();
    cfg.window = 1;
    let mut p = SimPhone::first_start(cfg, 0, 1);
    p.parts = parts();
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    c.hello(&mut p);
    c.pair(&mut p);
    let got = c.send(&mut p, begin(&image()));
    assert!(
        matches!(
            &got[..],
            [Got::Msg(PhoneFrame {
                msg: PhoneMsg::FactoryReady { window, .. },
                ..
            })] if *window == TEST_ACK_EVERY + MAX_DATA as u64
        ),
        "{got:?}"
    );
}
