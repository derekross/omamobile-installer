// SPDX-License-Identifier: MIT
//! A whole install's worth of link traffic through the loopback: the
//! straight path, and what the two sides do when the link is cut, a frame
//! is lost, repeated, swapped or damaged, a code is wrong, or what arrived
//! is not what was announced.
mod common;
use common::rig::*;
use common::*;
use omamobile_link::digest::{sha256, to_hex};
use omamobile_link::frame;
use omamobile_link::host::{Command, Event, Stop};
use omamobile_link::msg::*;
use omamobile_link::phone::{Effect, Persistent, PhoneInput, Screen};
use omamobile_link::sim::{Dir, Fate, Loopback, SimHost};

fn refused(lb: &Loopback, step: &str, code: &str) -> bool {
    lb.saw(|e| matches!(e, Event::Refused { step: s, error } if *s == step && error.code.as_str() == code))
}

/// From first boot done to `finished`, checking what each side holds.
fn finish_from_set_up(lb: &mut Loopback) {
    lb.command(Command::Backup);
    assert!(lb.saw(|e| *e == Event::BackupDone), "{:?}", lb.host.events);
    for (found, bytes) in parts() {
        assert_eq!(lb.host.backup[&found.name], bytes, "{}", found.name);
    }
    // The phone keeps NV_PARTS' lines, in sha256sum's format, and not devinfo's
    let sums: String = parts()
        .iter()
        .filter(|p| p.0.checked)
        .map(|(f, b)| format!("{}  {}.img\n", to_hex(&sha256(b)), f.name))
        .collect();
    assert_eq!(lb.phone.sums.as_deref(), Some(sums.as_str()));
    assert_eq!(lb.host.backup_finished.as_ref().map(Vec::len), Some(4));

    lb.command(Command::Factory(factory_begin(&image())));
    assert!(
        lb.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        lb.host.events
    );
    assert_eq!(lb.phone.factory_file, image());

    lb.command(Command::Provision);
    assert_eq!(lb.phone.provision_starts, 1);
    // The camera step alone failed: a success with a note (§6.6)
    let steps = result(Some("camera"));
    lb.phone_input(PhoneInput::ProvisionEnded {
        result: Some(steps.clone()),
    });
    assert!(
        lb.saw(|e| *e
            == Event::ProvisionDone {
                steps: steps.clone(),
                optional_failed: vec!["camera".into()]
            }),
        "{:?}",
        lb.host.events
    );
    assert_eq!(lb.phone.phone.stage(), Stage::Provisioned);
    assert_eq!(lb.phone.state_file.optional_failed, ["camera"]);
    assert!(
        lb.phone.factory_file.is_empty(),
        "the image is gone once provisioning succeeded"
    );

    lb.command(Command::Restart);
    assert_eq!(lb.phone.restarts, 1);
    assert!(lb.saw(|e| *e == Event::Restarting));
    reboot(lb);
    lb.connect();
    assert!(lb.saw(|e| *e == Event::Restarted), "{:?}", lb.host.events);

    session_up(&mut lb.phone, lb.now);
    // The planned restart was taken: a second one is not the link's to give
    lb.host.take_events();
    lb.command(Command::Restart);
    assert!(lb.saw(|e| *e == Event::Restarted) && lb.phone.restarts == 1);
    lb.command(Command::Smoke);
    assert!(
        lb.saw(|e| matches!(e, Event::SmokeReport(_))),
        "{:?}",
        lb.host.events
    );
    lb.command(Command::Finish);
    assert!(lb.saw(|e| *e == Event::Finished), "{:?}", lb.host.events);
    assert!(lb.phone.finished && lb.phone.phone.stopped());
    assert_eq!(lb.phone.done.as_deref(), Some("finished"));
    assert!(!lb.phone.marker && !lb.phone.state_exists);
    assert_eq!(lb.phone.state_file.stage, Stage::Finished);
    assert_eq!(lb.phone.state_file.token_hash, None);
    assert_eq!(lb.host.token, None);
}

#[test]
fn the_straight_path() {
    let mut lb = set_up();
    // The answers reached first boot as a firstboot.conf, value for value
    let conf = lb
        .phone
        .started_with
        .clone()
        .expect("first boot was started with a file");
    assert_eq!(lb.phone.conf, None, "which first boot deleted when done");
    assert!(lb.phone.boot_update_conf, "the plan's Q3");
    assert!(
        conf.contains("USERNAME=owner\n") && conf.contains("TIMEZONE=Europe/Berlin\n"),
        "{conf}"
    );
    assert!(
        conf.contains("WIFI_PSK=a passphrase\n") && conf.contains("SSH_KEY=ssh-ed25519 "),
        "{conf}"
    );
    assert_eq!(lb.phone.firstboot_starts, 1);
    finish_from_set_up(&mut lb);
    assert_eq!(lb.phone.phone.wrong_tries(), 0);
}

#[test]
fn a_stage_is_on_disk_before_the_reply_that_announces_it() {
    let mut lb = paired();
    lb.phone.log.clear();
    lb.command(Command::Answers(answers()));
    let persist = lb
        .phone
        .log
        .iter()
        .position(|e| matches!(e, Effect::Persist(p) if p.stage == Stage::Firstboot));
    let sent = lb.phone.log.iter().position(|e| {
        matches!(
            e,
            Effect::Note(omamobile_link::phone::Note::Sent("answers-ok"))
        )
    });
    assert!(persist.is_some() && persist < sent, "{:?}", lb.phone.log);
}

#[test]
fn the_serial_is_in_neither_sides_debug_output() {
    let shown = format!("{:?} {:?}", phone_config(), host_config());
    assert!(!shown.contains("SERIAL"), "{shown}");
}

#[test]
fn the_serial_proof_stops_another_phone() {
    let mut lb = rig();
    let mut cfg = host_config();
    cfg.serial = b"<OTHER-SERIAL>".to_vec();
    lb.host = SimHost::new(cfg);
    lb.connect();
    let code = lb.phone.phone.code().unwrap();
    lb.command(Command::Pair(code));
    assert!(
        lb.saw(|e| *e == Event::Stopped(Stop::WrongPhone)),
        "{:?}",
        lb.host.events
    );
    assert_eq!(
        lb.host.token, None,
        "a token from another phone is not kept"
    );
    assert!(lb.host.host.stopped());
    // ... and nothing more is asked of it
    lb.command(Command::Backup);
    assert!(!lb.phone.phone.paired(), "the session was closed with bye");
}

#[test]
fn wrong_codes_up_to_the_limit() {
    let mut lb = rig();
    lb.connect();
    let wrong = |lb: &Loopback| {
        let right = lb.phone.phone.code().unwrap();
        Code::parse(if right.digits() == "000000" {
            "000001"
        } else {
            "000000"
        })
        .unwrap()
    };
    let mut waits = Vec::new();
    for n in 1..=20u32 {
        let before = lb.phone.phone.code();
        lb.host.take_events();
        lb.command(Command::Pair(wrong(&lb)));
        assert_eq!(lb.phone.phone.wrong_tries(), n);
        if n % 5 != 0 {
            assert!(
                lb.saw(|e| *e == Event::WrongCode { left: 5 - n % 5 }),
                "{n}: {:?}",
                lb.host.events
            );
            assert_eq!(lb.phone.phone.code(), before);
            continue;
        }
        // The fifth: a new code, and a lock
        let wait = lb.host.events.iter().find_map(|e| match e {
            Event::Locked { wait_s } => Some(*wait_s),
            _ => None,
        });
        waits.push(wait.expect("locked"));
        if n < 20 {
            assert_ne!(lb.phone.phone.code(), before, "the code is replaced");
            // While it is locked the right code is refused too, and not counted
            let code = lb.phone.phone.code().unwrap();
            lb.host.take_events();
            lb.advance(1000);
            lb.command(Command::Pair(code));
            assert!(
                lb.saw(|e| matches!(e, Event::NotNow { .. })),
                "the host does not even ask: {:?}",
                lb.host.events
            );
            assert_eq!(lb.phone.phone.wrong_tries(), n);
            // The host asks again every five seconds, and may try when the
            // wait is over
            lb.advance(wait.unwrap() as u64 * 1000 + 5000);
            assert!(
                lb.saw(|e| *e == Event::NeedCode),
                "{n}: {:?}",
                lb.host.events
            );
        }
    }
    assert_eq!(
        waits,
        vec![30, 60, 120, 0],
        "30, 60 and 120 s; the twentieth locks until someone acts at the phone"
    );
    assert_eq!(lb.phone.phone.code(), None);
    assert!(lb.phone.screens.contains(&Screen::Locked { wait_s: 0 }));
    // Nothing pairs by a code now, however long the host waits
    lb.advance(3_600_000);
    assert!(!lb.phone.phone.paired());
    lb.cut();
    lb.connect();
    assert!(lb.saw(|e| *e == Event::Locked { wait_s: 0 }));
    // One press of a volume key: a new code, and the count from nothing
    lb.host.take_events();
    lb.phone_input(PhoneInput::Key);
    assert_eq!(lb.phone.phone.wrong_tries(), 0);
    lb.advance(5000);
    assert!(lb.saw(|e| *e == Event::NeedCode), "{:?}", lb.host.events);
    let code = lb.phone.phone.code().unwrap();
    lb.command(Command::Pair(code));
    assert!(lb.host.host.ready());
}

#[test]
fn a_right_code_after_wrong_ones_pairs() {
    let mut lb = rig();
    lb.connect();
    let right = lb.phone.phone.code().unwrap();
    let wrong = Code::parse(if right.digits() == "999999" {
        "999998"
    } else {
        "999999"
    })
    .unwrap();
    for _ in 0..4 {
        lb.command(Command::Pair(wrong));
    }
    lb.command(Command::Pair(right));
    assert!(lb.host.host.ready());
    // A code is used once: the screen has another for the next time
    assert_ne!(lb.phone.phone.code(), Some(right));
    assert_eq!(
        lb.phone.phone.wrong_tries(),
        4,
        "a right code does not reset the count"
    );
}

#[test]
fn a_wrong_token_is_not_counted_and_the_code_still_works() {
    let mut lb = rig();
    let mut cfg = host_config();
    cfg.token = Some(Token([7; 32]));
    lb.host = SimHost::new(cfg);
    lb.connect();
    // No token exists on the phone yet: `resume` is not allowed
    assert_eq!(lb.phone.phone.wrong_tries(), 0);
    assert!(lb.saw(|e| *e == Event::NeedCode), "{:?}", lb.host.events);
    let code = lb.phone.phone.code().unwrap();
    lb.command(Command::Pair(code));
    assert!(lb.host.host.ready());
    // Now one exists. Another installer with a token of its own:
    lb.cut();
    let mut cfg = host_config();
    cfg.token = Some(Token([8; 32]));
    lb.host = SimHost::new(cfg);
    lb.connect();
    assert_eq!(
        lb.phone.phone.wrong_tries(),
        0,
        "a token is 256 random bits: counting guesses protects nothing"
    );
    assert_eq!(lb.host.token, None, "the refused token is forgotten");
    assert!(lb.saw(|e| *e == Event::NeedCode));
}

#[test]
fn a_cut_at_every_frame_of_the_factory_image() {
    // Count the frames of an undisturbed transfer, then cut before each one
    // in turn, both ways, and see the image arrive whole every time.
    let mut lb = set_up();
    let before = lb.delivered;
    lb.command(Command::Factory(factory_begin(&image())));
    assert!(lb.saw(|e| *e == Event::FactoryVerified));
    let to_phone = lb.delivered[0] - before[0];
    let to_host = lb.delivered[1] - before[1];
    assert_eq!(
        to_phone,
        1 + 10 + 1,
        "factory-begin, ten data frames, factory-end"
    );
    assert_eq!(
        to_host,
        1 + 3 + 1,
        "factory-ready, three acknowledgements, factory-verified"
    );

    for (dir, count) in [(Dir::ToPhone, to_phone), (Dir::ToHost, to_host)] {
        for k in 0..count {
            let mut lb = set_up();
            let start = lb.delivered[dir as usize];
            let mut seen = 0u64;
            let mut done = false;
            lb.fault = Some(Box::new(move |d, _, _| {
                if d != dir || done {
                    return Fate::Deliver;
                }
                seen += 1;
                if seen == k + 1 {
                    done = true;
                    return Fate::Cut;
                }
                Fate::Deliver
            }));
            let _ = start;
            lb.command(Command::Factory(factory_begin(&image())));
            assert!(!lb.up, "{dir:?} {k}: the link was cut");
            assert!(lb.saw(|e| *e == Event::LinkLost), "{dir:?} {k}");
            let kept = lb.phone.state_file.factory.as_ref().map_or(0, |f| f.synced);
            assert!(
                kept.is_multiple_of(TEST_ACK_EVERY) || kept == IMAGE_LEN as u64,
                "{dir:?} {k}: kept {kept}"
            );
            lb.host.take_events();
            lb.connect();
            // hello, resume with the token, and the step goes on by itself
            assert!(
                lb.saw(|e| *e == Event::FactoryVerified),
                "{dir:?} {k}: {:?}",
                lb.host.events
            );
            assert_eq!(lb.phone.factory_file, image(), "{dir:?} {k}");
            assert!(lb
                .phone
                .state_file
                .factory
                .as_ref()
                .is_some_and(|f| f.verified));
            assert_eq!(
                lb.phone.phone.wrong_tries(),
                0,
                "{dir:?} {k}: a cut is not a wrong try"
            );
            // The resumed transfer began at the last acknowledged offset
            let resumed = lb.host.events.iter().find_map(|e| match e {
                Event::FactorySent { sent, .. } => Some(*sent),
                _ => None,
            });
            if let Some(first) = resumed {
                assert!(
                    first <= kept + MAX_DATA as u64,
                    "{dir:?} {k}: resumed at {first}, kept {kept}"
                );
            }
        }
    }
}

#[test]
fn a_cut_at_every_frame_of_the_whole_install() {
    // The same for everything from pairing to the end of provisioning.
    let script = |lb: &mut Loopback| {
        lb.command(Command::Answers(answers()));
        if lb.phone.phone.stage() == Stage::Firstboot {
            lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] });
        }
        lb.command(Command::AwaitFirstboot);
        lb.command(Command::Backup);
        lb.command(Command::Factory(factory_begin(&image())));
        lb.command(Command::Provision);
    };
    let mut lb = paired();
    let before = lb.delivered;
    script(&mut lb);
    assert_eq!(lb.phone.provision_starts, 1);
    let total = (lb.delivered[0] - before[0]) + (lb.delivered[1] - before[1]);
    assert!(total > 40, "{total}");

    for k in 0..total {
        let mut lb = paired();
        let mut seen = 0u64;
        lb.fault = Some(Box::new(move |_, _, _| {
            seen += 1;
            if seen == k + 1 {
                Fate::Cut
            } else {
                Fate::Deliver
            }
        }));
        // Drive the flow as an installer would: ask for the next step
        // whenever the machine is free, reconnect whenever the link is down.
        let mut steps: Vec<Command> = vec![
            Command::Answers(answers()),
            Command::AwaitFirstboot,
            Command::Backup,
            Command::Factory(factory_begin(&image())),
            Command::Provision,
        ];
        steps.reverse();
        let mut rounds = 0;
        while lb.phone.provision_starts == 0 {
            rounds += 1;
            assert!(rounds < 50, "cut {k}: stuck: {:?}", lb.host.events);
            if !lb.up {
                lb.connect();
            }
            if lb.phone.phone.stage() == Stage::Firstboot {
                lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] });
            }
            if lb.up && lb.host.host.ready() && !lb.host.host.busy() {
                if let Some(cmd) = steps.pop() {
                    lb.command(cmd);
                }
            }
        }
        assert_eq!(lb.phone.provision_starts, 1, "cut {k}");
        assert_eq!(
            lb.phone.firstboot_starts, 1,
            "cut {k}: the answers were taken once"
        );
        assert_eq!(lb.phone.factory_file, image(), "cut {k}");
        for (found, bytes) in parts() {
            assert_eq!(
                lb.host.backup[&found.name], bytes,
                "cut {k}: {}",
                found.name
            );
        }
        assert_eq!(lb.phone.phone.wrong_tries(), 0, "cut {k}");
    }
}

#[test]
fn a_wrong_image_digest() {
    // What arrives is not what was announced: the phone deletes it and says
    // so; nothing is counted (§2.5), and the stage does not move.
    let mut lb = set_up();
    lb.phone.damage_factory = Some(5000);
    lb.command(Command::Factory(factory_begin(&image())));
    assert!(
        refused(&lb, "factory", "factory-mismatch"),
        "{:?}",
        lb.host.events
    );
    assert!(lb.phone.factory_file.is_empty());
    assert_eq!(lb.phone.state_file.factory, None);
    assert_eq!(lb.phone.phone.wrong_tries(), 0);
    assert!(lb.phone.screens.contains(&Screen::Damaged));
    assert_eq!(lb.phone.phone.stage(), Stage::FirstbootDone);
    // `provision` is refused without a verified image
    lb.command(Command::Backup);
    lb.host.take_events();
    lb.command(Command::Provision);
    assert!(
        refused(&lb, "provision", "not-allowed"),
        "{:?}",
        lb.host.events
    );
    // Sent again from zero, it is taken
    lb.command(Command::Factory(factory_begin(&image())));
    assert!(lb.saw(|e| *e == Event::FactoryVerified));
    assert!(
        lb.saw(|e| *e
            == Event::FactorySent {
                sent: 1024,
                size: IMAGE_LEN as u64
            }),
        "from zero"
    );

    // A host that announces another digest than the file it sends
    let mut lb = set_up();
    let mut begin = factory_begin(&image());
    begin.sha256[0] ^= 1;
    lb.command(Command::Factory(begin));
    assert!(refused(&lb, "factory", "factory-mismatch"));
    assert_eq!(lb.phone.phone.wrong_tries(), 0);
}

#[test]
fn a_wrong_partition_checksum() {
    // One read on the phone comes back damaged: the computer's copy differs
    // from the phone's second read, is discarded and read again.
    let mut lb = set_up();
    lb.phone.bad_reads = 1;
    lb.command(Command::Backup);
    assert!(lb.saw(|e| *e == Event::BackupDone), "{:?}", lb.host.events);
    for (found, bytes) in parts() {
        assert_eq!(lb.host.backup[&found.name], bytes);
    }
    // A partition that never reads the same twice: five tries, then stop,
    // with nothing of it kept and nothing committed.
    let mut lb = set_up();
    lb.phone.bad_reads = u32::MAX;
    lb.command(Command::Backup);
    assert!(
        refused(&lb, "backup", "backup-mismatch"),
        "{:?}",
        lb.host.events
    );
    assert!(!lb.host.backup.contains_key("efs"));
    assert_eq!(lb.host.backup_finished, None);
    assert_eq!(lb.phone.sums, None);
    assert!(!lb.phone.state_file.backup_done);
    let reads = lb
        .phone
        .log
        .iter()
        .filter(|e| matches!(e, Effect::BackupDigest { .. }))
        .count();
    assert_eq!(reads, 5);
}

#[test]
fn a_partition_that_is_not_read_only_stops_the_backup() {
    let mut lb = set_up();
    lb.phone.precondition = vec!["efs".into()];
    lb.command(Command::Backup);
    assert!(lb.saw(|e| matches!(e, Event::Refused { error, .. }
        if error.code == ErrorCode::BackupPrecondition { parts: vec!["efs".into()] })));
    assert!(
        !lb.phone
            .log
            .iter()
            .any(|e| matches!(e, Effect::BackupRead { .. })),
        "nothing was read"
    );
}

#[test]
fn a_partition_that_moved_between_list_and_commit_stops_the_commit() {
    // The device name changes under the same partition name before the commit
    let mut lb = set_up();
    lb.phone.hold = true;
    lb.command(Command::Backup);
    // Walk the held answers by hand until the commit's second listing
    for _ in 0..200 {
        if lb
            .phone
            .held
            .iter()
            .any(|(_, o)| matches!(o, omamobile_link::phone::Outcome::Parts(_)))
            && lb
                .phone
                .log
                .iter()
                .filter(|e| matches!(e, Effect::BackupList { .. }))
                .count()
                == 2
        {
            break;
        }
        lb.release();
    }
    for (_, o) in lb.phone.held.iter_mut() {
        if let omamobile_link::phone::Outcome::Parts(p) = o {
            p[0].dev = "sdz9".into();
        }
    }
    lb.phone.hold = false;
    lb.release();
    assert!(
        lb.saw(|e| matches!(e, Event::Refused { error, .. }
        if error.code == ErrorCode::BackupPrecondition { parts: vec!["efs".into()] })),
        "{:?}",
        lb.host.events
    );
    assert_eq!(lb.phone.sums, None);
}

#[test]
fn answers_first_boot_refuses_come_back_by_field() {
    let mut lb = paired();
    lb.phone.porcelain = vec![omamobile_link::answers::Problem::new(
        omamobile_link::answers::Field::Timezone,
        omamobile_link::answers::reason::NO_SUCH_ZONE,
    )];
    lb.command(Command::Answers(answers()));
    assert!(lb.saw(|e| matches!(e, Event::Refused { error, .. } if matches!(&error.code,
        ErrorCode::Problems { problems } if problems.len() == 1 && problems[0].reason == "no-such-zone"))));
    assert_eq!(lb.phone.phone.stage(), Stage::Waiting);
    assert_eq!(lb.phone.conf, None, "nothing is kept");
    assert_eq!(lb.phone.firstboot_starts, 0);
}

#[test]
fn first_boot_fails_and_is_tried_again() {
    let mut lb = paired();
    lb.command(Command::Answers(answers()));
    lb.phone_input(PhoneInput::FirstbootEnded {
        failed: vec!["SSH keys".into()],
    });
    lb.command(Command::AwaitFirstboot);
    assert!(
        lb.saw(|e| *e
            == Event::FirstbootFailed {
                steps: vec!["SSH keys".into()]
            }),
        "{:?}",
        lb.host.events
    );
    lb.command(Command::Provision);
    assert!(refused(&lb, "provision", "firstboot-failed"));
    lb.host.take_events();
    lb.command(Command::FirstbootRetry);
    assert!(lb.saw(|e| *e == Event::AnswersAccepted));
    assert_eq!(lb.phone.firstboot_starts, 2);
    lb.command(Command::AwaitFirstboot);
    // Still running: the step asks again every two seconds until it ends
    lb.advance(5_000);
    assert!(lb.host.host.busy());
    lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] });
    lb.advance(2_000);
    assert!(
        lb.saw(|e| *e == Event::FirstbootDone),
        "{:?}",
        lb.host.events
    );
}

#[test]
fn provisioning_needs_power_and_reports_a_failed_step() {
    let mut lb = set_up();
    lb.command(Command::Backup);
    lb.command(Command::Factory(factory_begin(&image())));
    lb.phone.facts.charging = false;
    lb.phone.facts.battery = 29;
    lb.command(Command::Provision);
    assert!(refused(&lb, "provision", "battery"), "{:?}", lb.host.events);
    assert_eq!(lb.phone.provision_starts, 0);
    lb.phone.facts.battery = 30;
    lb.host.take_events();
    lb.command(Command::Provision);
    assert_eq!(lb.phone.provision_starts, 1);
    lb.phone_input(PhoneInput::Progress(Progress {
        what: What::Provision,
        step: "firmware".into(),
        done: 1,
        total: 6,
    }));
    assert!(lb.saw(|e| matches!(e, Event::Progress(p) if p.step == "firmware")));
    lb.phone_input(PhoneInput::ProvisionEnded {
        result: Some(result(Some("modem"))),
    });
    assert!(
        lb.saw(|e| matches!(e, Event::Refused { step: "provision", error }
        if error.code == ErrorCode::ProvisionFailed { step: "modem".into() })),
        "{:?}",
        lb.host.events
    );
    assert_eq!(lb.phone.phone.stage(), Stage::FirstbootDone);
    assert!(
        !lb.phone.factory_file.is_empty(),
        "the image is kept for another try"
    );
    // "Try again" sends `provision` again
    lb.host.take_events();
    lb.command(Command::Provision);
    assert_eq!(lb.phone.provision_starts, 2);
    lb.phone_input(PhoneInput::ProvisionEnded {
        result: Some(result(None)),
    });
    assert!(lb.saw(
        |e| matches!(e, Event::ProvisionDone { optional_failed, .. } if optional_failed.is_empty())
    ));
}

#[test]
fn provisioning_goes_on_without_the_host() {
    let mut lb = set_up();
    lb.command(Command::Backup);
    lb.command(Command::Factory(factory_begin(&image())));
    lb.command(Command::Provision);
    lb.cut();
    // The camera step alone fails while nobody listens (§6.6)
    lb.phone_input(PhoneInput::ProvisionEnded {
        result: Some(result(Some("camera"))),
    });
    assert_eq!(lb.phone.state_file.stage, Stage::Provisioned);
    lb.host.take_events();
    lb.connect();
    // The step is taken up, sees the stage and ends without sending
    // `provision` again; `status` still says what the lost event would
    // have: "Cameras: not set up"
    assert!(
        lb.saw(|e| *e
            == Event::ProvisionDone {
                steps: vec![],
                optional_failed: vec!["camera".into()]
            }),
        "{:?}",
        lb.host.events
    );
    // ... and so does a later run of the installer, in a later boot
    reboot(&mut lb);
    lb.host.take_events();
    lb.connect();
    lb.command(Command::Provision);
    assert!(lb.saw(
        |e| matches!(e, Event::ProvisionDone { optional_failed, .. } if optional_failed == &["camera"])
    ));
    // There is nothing to provision again from: the image is gone, and
    // `provision` at `provisioned` is not allowed
    assert!(lb.phone.factory_file.is_empty() && !lb.phone.factory_exists);
    assert_eq!(lb.phone.provision_starts, 1);
}

#[test]
fn no_space_and_too_large() {
    let mut lb = set_up();
    lb.phone.free = 1000;
    lb.command(Command::Factory(factory_begin(&image())));
    assert!(lb.saw(|e| matches!(e, Event::Refused { error, .. } if matches!(error.code, ErrorCode::NoSpace { free: 1000, .. }))));
    let mut begin = factory_begin(&image());
    begin.size = MAX_FACTORY_SIZE + 1;
    lb.host.take_events();
    lb.command(Command::Factory(begin));
    assert!(refused(&lb, "factory", "too-large"));
}

#[test]
fn another_file_s_partial_is_not_resumed() {
    let mut lb = set_up();
    let mut n = 0;
    lb.fault = Some(Box::new(move |d, _, _| {
        n += (d == Dir::ToPhone) as u32;
        if n == 8 {
            Fate::Cut
        } else {
            Fate::Deliver
        }
    }));
    lb.command(Command::Factory(factory_begin(&image())));
    assert_eq!(
        lb.phone.state_file.factory.as_ref().map(|f| f.synced),
        Some(4096)
    );
    // Another image is announced: it starts at zero
    lb.fault = None;
    let other: Vec<u8> = image().iter().map(|b| b ^ 0x55).collect();
    lb.host.factory = other.clone();
    lb.host.take_events();
    lb.connect();
    lb.command(Command::Abort);
    lb.command(Command::Factory(factory_begin(&other)));
    assert!(
        lb.saw(|e| *e
            == Event::FactorySent {
                sent: 1024,
                size: IMAGE_LEN as u64
            }),
        "{:?}",
        lb.host.events
    );
    assert!(lb.saw(|e| *e == Event::FactoryVerified));
    assert_eq!(lb.phone.factory_file, other);
}

#[test]
fn version_mismatch_each_way() {
    // An installer that is too new for the phone
    let mut lb = rig();
    let mut cfg = host_config();
    cfg.protos = vec![2, 3];
    lb.host = SimHost::new(cfg);
    lb.connect();
    assert!(
        lb.saw(|e| *e == Event::Stopped(Stop::Version { phone: vec![1] })),
        "{:?}",
        lb.host.events
    );
    assert!(lb.phone.screens.contains(&Screen::Version));
    assert!(!lb.phone.phone.paired());
    // A phone that is too new for the installer
    let mut cfg = phone_config();
    cfg.protos = vec![2];
    let phone = omamobile_link::sim::SimPhone::new(cfg, Persistent::default(), true, 0, 1);
    let mut lb = Loopback::new(phone, SimHost::new(host_config()));
    lb.connect();
    assert!(lb.saw(|e| *e == Event::Stopped(Stop::Version { phone: vec![2] })));
    // Both speak several: the highest in common is taken
    let mut cfg = phone_config();
    cfg.protos = vec![1, 2, 4];
    let phone = omamobile_link::sim::SimPhone::new(cfg, Persistent::default(), true, 0, 1);
    let mut hc = host_config();
    hc.protos = vec![1, 2, 3];
    let mut lb = Loopback::new(phone, SimHost::new(hc));
    let seen = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let s = seen.clone();
    lb.fault = Some(Box::new(move |d, n, bytes| {
        if d == Dir::ToHost && n == 0 {
            let f = frame::decode(bytes, MAX_DATA).unwrap();
            if let PhoneMsg::Hello(h) = PhoneFrame::parse(f.payload).unwrap().msg {
                s.set(h.proto);
            }
        }
        Fate::Deliver
    }));
    lb.connect();
    assert_eq!(seen.get(), 2);
}

#[test]
fn a_lost_repeated_swapped_or_damaged_frame_ends_the_session_and_the_install_goes_on() {
    type Mangle = fn(&mut Vec<u8>) -> Fate;
    let faults: [(&str, Mangle); 6] = [
        ("dropped", |_| Fate::Drop),
        ("repeated", |_| Fate::Duplicate),
        ("swapped", |_| Fate::Swap),
        ("cut short", |b| {
            b.truncate(b.len() - 1);
            Fate::Deliver
        }),
        ("a flipped bit in the payload", |b| {
            let last = b.len() - 2;
            b[last] ^= 0x10;
            Fate::Deliver
        }),
        ("a flipped bit in the header", |b| {
            b[13] ^= 1;
            Fate::Deliver
        }),
    ];
    for (what, mangle) in faults {
        for dir in [Dir::ToPhone, Dir::ToHost] {
            // The fifth frame each way of a backup followed by the image
            let mut lb = set_up();
            let mut n = 0;
            lb.fault = Some(Box::new(move |d, _, bytes| {
                if d != dir {
                    return Fate::Deliver;
                }
                n += 1;
                if n == 5 {
                    mangle(bytes)
                } else {
                    Fate::Deliver
                }
            }));
            lb.command(Command::Backup);
            // A lost frame is silence: the host waits its minute, then says hello again
            lb.advance(61_000);
            if !lb.host.host.busy() {
                lb.command(Command::Factory(factory_begin(&image())));
                lb.advance(61_000);
            }
            lb.command(Command::Backup);
            lb.command(Command::Factory(factory_begin(&image())));
            lb.advance(61_000);
            assert!(
                lb.saw(|e| *e == Event::BackupDone),
                "{what} {dir:?}: {:?}",
                lb.host.events
            );
            assert!(
                lb.saw(|e| *e == Event::FactoryVerified),
                "{what} {dir:?}: {:?}",
                lb.host.events
            );
            for (found, bytes) in parts() {
                assert_eq!(lb.host.backup[&found.name], bytes, "{what} {dir:?}");
            }
            assert_eq!(lb.phone.factory_file, image(), "{what} {dir:?}");
            assert_eq!(lb.phone.phone.wrong_tries(), 0, "{what} {dir:?}");
        }
    }
}

#[test]
fn a_long_step_sends_progress_until_it_answers() {
    let mut lb = set_up();
    lb.phone.hold = true;
    lb.command(Command::Backup);
    // The phone is at its checks and owes an answer: within ten seconds it
    // has said twice that it is working, and the host does not give up
    lb.advance(10_000);
    let said = lb
        .host
        .events
        .iter()
        .filter(|e| matches!(e, Event::Progress(p) if p.what == What::Working))
        .count();
    assert_eq!(said, 2, "{:?}", lb.host.events);
    lb.advance(120_000);
    assert!(
        !lb.saw(|e| *e == Event::LinkLost),
        "progress keeps the step alive"
    );
    lb.phone.hold = false;
    lb.release();
    assert!(lb.saw(|e| *e == Event::BackupDone), "{:?}", lb.host.events);
}
