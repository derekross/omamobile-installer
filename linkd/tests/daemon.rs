// SPDX-License-Identifier: MIT
//! The daemon's own rules, each against a real process on a scratch root:
//! `reopen` and the boot it ran in, the link closed for a boot, the
//! session agent's socket and its lines, the backup's preconditions, a
//! pulled "cable", an installer run again with the link left up, and the
//! write rules of §5.7 as far as a socket pair shows them.
#![cfg(target_os = "linux")]
mod common;

use common::*;
use omamobile_link::frame::{self, Kind};
use omamobile_link::host::{Command, Event, HostInput};
use omamobile_link::msg::{
    Code, ErrorCode, HostFrame, HostHello, HostMsg, PhoneFrame, PhoneMsg, Stage,
};
use omamobile_link::phone::Persistent;
use omamobile_link::sim::SimHost;
use std::time::{Duration, Instant};

/// A phone at `firstboot-done` in this boot: an account, no token.
fn set_up(change: impl FnOnce(&mut Persistent)) -> Phone {
    let p = Phone::new();
    p.firstboot_was_done();
    p.state_now(|s| {
        s.stage = Stage::FirstbootDone;
        change(s);
    });
    p
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let before = Instant::now();
    while !f() {
        assert!(before.elapsed() < Duration::from_secs(10), "never: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// --- reopen (P6) and status ----------------------------------------------------

/// A phone whose link ended as `expired`: the fourth setup boot's start.
fn expired() -> Phone {
    let p = set_up(|s| {
        s.boot_id = "the boot before".into();
        s.setup_boots = 3;
        s.token_hash = Some([9; 32]);
    });
    let mut d = p.daemon();
    assert!(!d.settled());
    assert_eq!(d.wait_exit(), 0);
    assert_eq!(p.done().as_deref(), Some("expired backup firmware cameras"));
    p
}

#[test]
fn reopen_grants_one_boot_and_takes_effect_at_the_next() {
    let mut p = expired();
    let (status, out) = p.command("status");
    assert_eq!(status, 0);
    assert!(
        out.starts_with("link: ended\nreason: expired\nmissing: backup firmware cameras\nstage: firstboot-done\n"),
        "{out}"
    );
    assert!(out.contains("sudo omamobile-install-link reopen"), "{out}");
    let before = p.state().unwrap();

    let (status, out) = p.command("reopen");
    assert_eq!(status, 0, "{out}");
    assert!(p.marker() && p.done().is_none());
    let granted = p.state().unwrap();
    assert_eq!(
        granted,
        Persistent {
            boots_allowed: 4,
            ..before.clone()
        }
    );
    assert!(p
        .command("status")
        .1
        .starts_with("link: offered from the next restart\n"));
    // One reopen is one boot: a second one finds nothing to reopen
    assert_eq!(p.command("reopen").0, 1);
    assert_eq!(p.state().unwrap(), granted);

    // A daemon started in the boot `reopen` ran in exits before any gadget
    // change, however often, and counts nothing
    for _ in 0..3 {
        let mut d = p.daemon();
        assert!(!d.settled(), "offered in the boot of the reopen");
        assert_eq!(d.wait_exit(), 0);
        assert!(p.gadget().is_empty(), "{:?}", p.gadget());
        assert_eq!(p.state().unwrap(), granted);
        assert!(p.marker() && p.done().is_none());
    }

    // The next boot: the link, for this one boot
    p.new_boot();
    let mut d = p.daemon();
    assert!(d.settled());
    assert_eq!(p.gadget(), ["add"]);
    let s = p.state().unwrap();
    assert_eq!((s.setup_boots, s.boots_left()), (4, 0));
    assert!(!p.exists("/var/lib/omamobile/install-link/reopened"));
    d.kill();

    // And the boot after: ended again, with no gadget change
    p.new_boot();
    let mut d = p.daemon();
    assert!(!d.settled());
    assert_eq!(d.wait_exit(), 0);
    assert_eq!(p.done().as_deref(), Some("expired backup firmware cameras"));
    assert_eq!(p.gadget(), ["add", "boot"]);
}

#[test]
fn reopen_only_after_an_expired_end() {
    // The link has not ended
    let p = set_up(|_| {});
    assert_eq!(p.command("reopen").0, 1);
    assert!(p.done().is_none() && !p.exists("/var/lib/omamobile/install-link/reopened"));
    // It ended as something else
    for word in [
        "finished",
        "not-first-start",
        "state-unreadable",
        "inconsistent",
    ] {
        let p = set_up(|s| s.setup_boots = 3);
        p.remove("/etc/omamobile/install-link");
        p.write(
            "/var/lib/omamobile/install-link/done",
            format!("{word}\n").as_bytes(),
        );
        let (status, out) = p.command("reopen");
        assert_eq!(status, 1, "{word}: {out}");
        assert!(!p.marker() && p.done().as_deref() == Some(word), "{word}");
    }
    // `expired`, and a state file that cannot be read, or whose budget is
    // not spent: nothing is changed
    for state in [&b"{}"[..], b""] {
        let p = expired();
        p.write("/var/lib/omamobile/install-link/state", state);
        assert_eq!(p.command("reopen").0, 1);
        assert!(!p.marker() && p.done().is_some());
    }
    // This boot's id cannot be read: the boot could not be recorded
    let p = expired();
    p.remove("/proc/sys/kernel/random/boot_id");
    assert_eq!(p.command("reopen").0, 1);
    assert!(!p.marker() && p.done().is_some());
    // Anything else is the usage
    assert_eq!(p.command("restart").0, 2);
}

#[test]
fn status_prints_no_token_and_no_boot_id() {
    let p = set_up(|s| {
        s.token_hash = Some([0xab; 32]);
        s.backup_done = true;
    });
    let (status, out) = p.command("status");
    assert_eq!(status, 0);
    assert_eq!(
        out,
        "link: offered\nstage: firstboot-done\nboot: 1\nsetup boots left: 3\n\
         radio backup: done\nfactory image: none\nprovisioning: none\n"
    );
    assert!(!out.contains("abab") && !out.contains(&p.boot_id()));
    // A phone the link never set up
    let p = Phone::new();
    p.remove("/etc/omamobile/install-link");
    assert_eq!(p.command("status").1, "link: none\n");
}

// --- Closed for this boot (§2.2) -------------------------------------------------

#[test]
fn ten_unpaired_minutes_close_the_link_for_this_boot_only() {
    let mut p = set_up(|_| {});
    // (the machine's clock 3000 times the wall's: ten minutes in 0.2 s)
    let fast = ["--clock-scale", "3000"];
    let mut d = p.daemon_with(&fast);
    assert!(d.settled());
    // The owner's session is up: the agent's connection says so
    let line = d.agent().wait(|l| l.starts_with("open ")).unwrap();
    assert!(line.starts_with("open 3 "), "{line}");
    assert_eq!(
        d.agent().wait(|l| l.starts_with("closed ")).as_deref(),
        Some("closed 3")
    );
    assert_eq!(d.wait_exit(), 0);
    assert_eq!(p.gadget(), ["add", "remove"], "the gadget is put back");
    assert!(p.marker() && p.done().is_none());
    assert_eq!(
        p.command("status").1.lines().next(),
        Some("link: closed for this boot")
    );
    // A daemon started again in this boot offers nothing
    for _ in 0..2 {
        let mut d = p.daemon_with(&fast);
        assert!(!d.settled(), "offered again in the boot it was closed in");
        assert_eq!(d.wait_exit(), 0);
        assert_eq!(p.gadget(), ["add", "remove"]);
    }
    // The next boot opens it again: a setup boot is left
    p.new_boot();
    let mut d = p.daemon_with(&fast);
    assert!(d.settled());
    assert_eq!(p.state().unwrap().setup_boots, 1);
    // With no agent, the session counts as up three minutes after the
    // display manager started: the ten minutes run from there
    p.write("/fake-systemd/display-manager.service.since", b"1\n");
    assert_eq!(d.wait_exit(), 0);
    assert_eq!(p.gadget(), ["add", "remove", "boot", "add", "remove"]);
}

#[test]
fn the_ten_minutes_do_not_run_before_the_session_is_up() {
    let p = set_up(|_| {});
    let mut d = p.daemon_with(&["--clock-scale", "3000"]);
    assert!(d.settled());
    // An hour of the machine's clock, no session: the link stays
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(d.exited(), None);
    assert_eq!(p.gadget(), ["add"]);
}

// --- The session agent's socket (§2.8) ---------------------------------------------

#[test]
fn the_agent_socket_takes_the_account_first_boot_made_and_nobody_else() {
    // The account's uid is not the peer's: no line, the connection closed
    let p = Phone::new();
    p.add_account("owner", Phone::my_uid() + 1, true);
    p.write("/var/lib/omamobile/firstboot/done", b"x\n");
    p.state_now(|s| s.stage = Stage::FirstbootDone);
    let mut d = p.daemon();
    assert!(d.settled());
    assert_eq!(d.agent().wait(|_| true), None);
    assert!(
        d.agent.as_ref().unwrap().closed,
        "a peer of another uid was let in"
    );
    assert!(!p.journal().contains("agent: connected"));
    drop(d);

    // No account at all (the first-start screen's time): nobody
    let p = Phone::new();
    let mut d = p.daemon();
    assert!(d.settled());
    assert_eq!(d.agent().wait(|_| true), None);
    assert!(d.agent.as_ref().unwrap().closed);
}

#[test]
fn the_agents_lines_and_a_code_only_on_the_owners_tap() {
    if Phone::my_uid() < 1000 {
        eprintln!("skipped: the test's uid cannot be an account's");
        return;
    }
    let p = set_up(|_| {});
    let mut d = p.daemon();
    assert!(d.settled());
    // The notice: open, no code
    let line = d.agent().wait(|l| l.starts_with("open ")).unwrap();
    assert_eq!(line, "open 3 600");
    // One peer at a time: a second one gets nothing
    let mut second = Peer::connect(&p.path("/run/omamobile-install-link/agent")).unwrap();
    assert_eq!(second.wait(|_| true), None);
    assert!(second.closed, "a second agent was let in");

    // The installer has no token: the owner can make the phone show a code
    let mut i = Installer::new();
    i.connect(&mut d);
    i.until(&mut d, "the tap is asked for", |i| {
        i.saw(|e| *e == Event::NeedTap)
    });
    assert_eq!(d.agent().code(), None, "nothing until the tap");
    // The tap: a code in the window, valid ten minutes
    d.agent().say("show");
    let line = d.agent().wait(|l| l.starts_with("code ")).unwrap();
    let code = Code::parse(&line[5..11]).expect("six digits");
    assert_eq!(&line[11..], " 600");
    // ... and nowhere else: not in the journal, not in a file
    assert!(!p.journal().contains(code.digits()));
    assert!(!p.command("status").1.contains(code.digits()));
    // `hide`: the window is closed and its code is gone (the agent closed
    // it itself and is told nothing)
    d.agent().say("hide");
    // Another tap, another code; the installer pairs with it
    d.agent().say("show");
    wait_for("a new code", || {
        d.agent.as_mut().unwrap().code().is_some_and(|c| c != code)
    });
    let code = d.agent().code().unwrap();
    i.until(&mut d, "the code is asked for", |i| {
        i.host.events.last() == Some(&Event::NeedCode)
    });
    i.command(&mut d, Command::Pair(code));
    i.until(&mut d, "paired", |i| i.host.host.ready());
    assert_eq!(d.agent().wait(|l| l == "paired").as_deref(), Some("paired"));
    assert!(!p.journal().contains(code.digits()));

    // Anything but the two fixed lines ends the connection
    d.agent().say("status");
    wait_for("the agent is dropped", || {
        let a = d.agent.as_mut().unwrap();
        a.poll();
        a.closed
    });
    wait_for("the daemon says so", || p.journal().contains("agent: gone"));
}

#[test]
fn twenty_wrong_codes_in_the_session_the_agent_is_told_locked() {
    if Phone::my_uid() < 1000 {
        eprintln!("skipped: the test's uid cannot be an account's");
        return;
    }
    let p = set_up(|_| {});
    let mut d = p.daemon();
    assert!(d.settled());
    let mut i = Installer::new();
    i.connect(&mut d);
    d.agent().say("show");
    let mut locks = Vec::new();
    for n in 1..=20 {
        wait_for("a code in the window", || {
            d.agent.as_mut().unwrap().code().is_some()
        });
        i.until(&mut d, "a code is asked for", |i| {
            i.host.events.last() == Some(&Event::NeedCode)
        });
        let code = d.agent().code().unwrap();
        let wrong = Code::from_number((code.digits().parse::<u32>().unwrap() + 1) % 1_000_000);
        i.host.take_events();
        i.command(&mut d, Command::Pair(wrong));
        i.until(&mut d, "the wrong code is answered", |i| {
            i.saw(|e| matches!(e, Event::WrongCode { .. } | Event::Locked { .. }))
        });
        if let Some(Event::Locked { wait_s }) = i
            .host
            .events
            .iter()
            .find(|e| matches!(e, Event::Locked { .. }))
        {
            locks.push(*wait_s);
            assert_eq!(n % 5, 0);
        }
        if n == 20 {
            break;
        }
    }
    assert_eq!(locks, [30, 60, 120, 0]);
    // The window closed, and the agent is told why
    assert_eq!(d.agent().wait(|l| l == "locked").as_deref(), Some("locked"));
    assert_eq!(
        d.agent.as_ref().unwrap().lines.last().map(String::as_str),
        Some("locked")
    );
    // The owner's tap is the act that clears it: a new code, and it pairs
    d.agent().say("show");
    wait_for("a new code", || d.agent.as_mut().unwrap().code().is_some());
    let code = d.agent().code().unwrap();
    i.until(&mut d, "the code is asked for", |i| {
        i.host.events.last() == Some(&Event::NeedCode)
    });
    i.command(&mut d, Command::Pair(code));
    i.until(&mut d, "paired", |i| i.host.host.ready());
    assert!(!p.journal().contains(code.digits()));
}

#[test]
fn a_volume_key_at_the_first_start_screen_draws_a_new_code() {
    let p = Phone::new();
    let mut d = p.daemon();
    assert!(d.settled());
    // A screen program that connects late is told what is on show
    let first = d.screen().wait(|l| l.starts_with("code ")).unwrap();
    d.screen().say("key");
    wait_for("another code", || {
        let s = d.screen.as_mut().unwrap();
        s.poll();
        s.lines
            .last()
            .is_some_and(|l| l.starts_with("code ") && *l != first)
    });
    assert!(!p.journal().contains(&first[5..]));
    // The agent's lines are not the screen's
    d.screen().say("show");
    wait_for("the screen is dropped", || {
        let s = d.screen.as_mut().unwrap();
        s.poll();
        s.closed
    });
}

// --- The radio backup's preconditions (§5.5) ---------------------------------------

#[test]
fn the_backup_is_refused_unless_every_partition_is_found_read_only_and_unmounted() {
    type Break = (&'static str, fn(&Phone), &'static [&'static str]);
    let cases: [Break; 8] = [
        (
            "writable",
            |p| p.write("/sys/class/block/sda1/ro", b"0\n"),
            &["efs"],
        ),
        (
            "mounted",
            |p| {
                p.write(
                    "/proc/mounts",
                    b"/dev/sda99 / ext4 rw 0 0\n/dev/sda2 /mnt ext4 ro 0 0\n",
                )
            },
            &["persist"],
        ),
        (
            "missing",
            |p| p.remove("/sys/class/block/sda3/uevent"),
            &["modem_a"],
        ),
        (
            "doubled",
            |p| {
                p.write(
                    "/sys/class/block/sdb1/uevent",
                    b"DEVNAME=sdb1\nPARTNAME=devinfo\n",
                );
                p.write("/sys/class/block/sdb1/ro", b"1\n");
                p.write("/sys/class/block/sdb1/size", b"1\n");
                p.write("/dev/sdb1", &[0; 512]);
            },
            &["devinfo"],
        ),
        ("no device", |p| p.remove("/dev/sda1"), &["efs"]),
        (
            "no size",
            |p| p.write("/sys/class/block/sda2/size", b"0\n"),
            &["persist"],
        ),
        (
            "two of them",
            |p| {
                p.write("/sys/class/block/sda1/ro", b"0\n");
                p.remove("/sys/class/block/sda4/ro");
            },
            &["efs", "devinfo"],
        ),
        (
            "another board",
            |p| p.write("/proc/device-tree/compatible", b"google,gs201-panther\0"),
            &["efs", "persist", "modem_a", "devinfo"],
        ),
    ];
    for (what, breakage, names) in cases {
        let p = Phone::new();
        breakage(&p);
        let mut d = p.daemon();
        assert!(d.settled());
        let mut i = Installer::new();
        i.pair_at_screen(&mut d);
        i.step(&mut d, Command::Backup);
        let refused = i.host.events.iter().find_map(|e| match e {
            Event::Refused {
                step: "backup",
                error,
            } => Some(error.code.clone()),
            _ => None,
        });
        let want: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        assert_eq!(
            refused,
            Some(ErrorCode::BackupPrecondition { parts: want }),
            "{what}: {:?}",
            i.host.events
        );
        assert!(i.host.backup.is_empty(), "{what}: nothing is read");
        assert!(!p.exists("/var/lib/omamobile/install-link/modem-nv.sha256"));
    }
}

// --- The answers, and first boot's own end ------------------------------------------

#[test]
fn first_boots_objections_come_back_under_their_fields_and_nothing_is_kept() {
    let p = Phone::new();
    p.write(
        "/fake-porcelain",
        b"USERNAME\texists\nTIMEZONE\tno-such-zone\n",
    );
    let mut d = p.daemon();
    assert!(d.settled());
    let mut i = Installer::new();
    i.pair_at_screen(&mut d);
    i.step(&mut d, Command::Answers(answers()));
    let problems = i.host.events.iter().find_map(|e| match e {
        Event::Refused { error, .. } => match &error.code {
            ErrorCode::Problems { problems } => Some(
                problems
                    .iter()
                    .map(|p| format!("{} {}", p.field.as_str(), p.reason))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(
        problems,
        Some(vec![
            "username exists".to_string(),
            "timezone no-such-zone".to_string()
        ]),
        "{:?}",
        i.host.events
    );
    assert!(!p.exists("/etc/omamobile/firstboot.conf"));
    assert!(!p.exists("/etc/omamobile/boot-update.conf"));
    assert_eq!(p.state().unwrap().stage, Stage::Waiting);
    assert!(p.systemd_log().is_empty(), "nothing was started");
    // A program that does not answer as `--porcelain` does is the phone's
    // own fault: `internal`, and still nothing kept
    p.write("/fake-porcelain", b"USERNAME is bad\n");
    i.step(&mut d, Command::Answers(answers()));
    assert!(
        i.saw(|e| matches!(e, Event::Refused { error, .. } if error.code == ErrorCode::Internal)),
        "{:?}",
        i.host.events
    );
    assert!(!p.exists("/etc/omamobile/firstboot.conf"));
}

#[test]
fn first_boot_that_fails_is_told_by_its_steps_and_names_no_user() {
    let mut p = Phone::new();
    let mut d = p.daemon();
    assert!(d.settled());
    let mut i = Installer::new();
    i.pair_at_screen(&mut d);
    i.step(&mut d, Command::Answers(answers()));
    // The account step fails: the script logs it as "user owner"
    p.finish_firstboot(&["user", "login"]);
    i.step(&mut d, Command::AwaitFirstboot);
    assert!(
        i.saw(|e| matches!(e, Event::FirstbootFailed { steps } if steps == &["user", "login"])),
        "{:?}",
        i.host.events
    );
    assert_eq!(p.state().unwrap().firstboot_failed, ["user", "login"]);
    assert_eq!(
        d.screen().wait(|l| l.starts_with("stopped ")).as_deref(),
        Some("stopped user")
    );
    // `status` is answered unpaired: no step in it may name the account
    i.step(&mut d, Command::Status);
    let status = i.host.events.iter().find_map(|e| match e {
        Event::Status(s) => Some(format!("{s:?}")),
        _ => None,
    });
    assert!(!status.unwrap().contains("owner"));
    // "Try again": the unit is started again with the kept file
    i.host.take_events();
    i.command(&mut d, Command::FirstbootRetry);
    i.until(&mut d, "the unit is started again", |_| {
        p.systemd_log()
            .iter()
            .filter(|l| l.starts_with("start "))
            .count()
            == 2
    });
    p.finish_firstboot(&[]);
    i.until(&mut d, "the retry is answered", |i| !i.host.host.busy());
    i.step(&mut d, Command::AwaitFirstboot);
    assert!(i.saw(|e| *e == Event::FirstbootDone), "{:?}", i.host.events);
    assert_eq!(p.state().unwrap().stage, Stage::FirstbootDone);
}

// --- The factory image ---------------------------------------------------------------

#[test]
fn no_space_is_answered_with_the_numbers_and_nothing_is_written() {
    let p = set_up(|_| {});
    // (more working space than any disk has)
    let mut d = p.daemon_with(&["--working-space", "9000000000000000"]);
    assert!(d.settled());
    let mut i = Installer::new();
    i.connect(&mut d);
    d.agent().say("show");
    wait_for("a code", || d.agent.as_mut().unwrap().code().is_some());
    let code = d.agent().code().unwrap();
    i.until(&mut d, "the code is asked for", |i| {
        i.host.events.last() == Some(&Event::NeedCode)
    });
    i.command(&mut d, Command::Pair(code));
    i.until(&mut d, "paired", |i| i.host.host.ready());
    i.step(&mut d, Command::Factory(factory_begin(&image())));
    assert!(
        i.saw(|e| matches!(e, Event::Refused { error, .. }
            if matches!(error.code, ErrorCode::NoSpace { need, free } if need > free && free > 0))),
        "{:?}",
        i.host.events
    );
    assert_eq!(p.image_len(), None);
}

/// Paired at the first-start screen, the answers in, first boot done.
fn through_first_boot(p: &mut Phone, d: &mut Daemon) -> Installer {
    let mut i = Installer::new();
    i.pair_at_screen(d);
    i.step(d, Command::Answers(answers()));
    p.finish_firstboot(&[]);
    i.step(d, Command::AwaitFirstboot);
    assert!(i.saw(|e| *e == Event::FirstbootDone), "{:?}", i.host.events);
    i
}

#[test]
fn a_pulled_cable_at_any_moment_of_the_backup_and_the_image() {
    // The "cable" is pulled after this many frames from the phone, in the
    // backup and then in the image's transfer, and put back
    for pull_after in 0..24 {
        let mut p = Phone::new();
        let mut d = p.daemon();
        assert!(d.settled());
        let mut i = through_first_boot(&mut p, &mut d);
        for cmd in [Command::Backup, Command::Factory(factory_begin(&image()))] {
            i.host.take_events();
            i.command(&mut d, cmd.clone());
            for _ in 0..pull_after {
                match d.recv(Duration::from_millis(100)) {
                    Some(frame) => i.feed(&mut d, HostInput::Frame(&frame)),
                    // (the step is over, or waits for the host's clock)
                    None => break,
                }
            }
            d.disable();
            i.feed(&mut d, HostInput::LinkDown);
            i.connect(&mut d);
            i.until(&mut d, "the step ends after the replug", |i| {
                i.host.host.ready() && !i.host.host.busy()
            });
            // An installer's "try again", when the step was lost with the link
            let ended =
                |i: &Installer| i.saw(|e| matches!(e, Event::BackupDone | Event::FactoryVerified));
            if !ended(&i) {
                i.step(&mut d, cmd);
            }
            assert!(ended(&i), "pulled after {pull_after}: {:?}", i.host.events);
        }
        assert!(p.state().unwrap().backup_done);
        assert_eq!(
            std::fs::read(p.path("/var/lib/omamobile/install-link/factory.zip")).unwrap(),
            image(),
            "pulled after {pull_after}"
        );
        assert!(p.journal().contains("link: DISABLE"));
    }
}

#[test]
fn an_installer_run_again_with_the_link_left_up_pairs_within_two_frames() {
    // F1 and F2 against the daemon: the installer is replaced by a new one
    // after this many frames of the backup, the socket pair untouched, the
    // old run's frames still under way
    for replace_after in 0..16 {
        let mut p = Phone::new();
        let mut d = p.daemon();
        assert!(d.settled());
        let mut i = through_first_boot(&mut p, &mut d);
        i.command(&mut d, Command::Backup);
        for _ in 0..replace_after {
            match d.recv(Duration::from_millis(100)) {
                Some(frame) => i.feed(&mut d, HostInput::Frame(&frame)),
                None => break,
            }
        }
        let mut cfg = host_config();
        cfg.token = i.host.token;
        assert!(cfg.token.is_some());
        let mut again = Installer::with(SimHost::with_seed(cfg, 7 + replace_after));
        again.feed(&mut d, HostInput::LinkUp);
        again.until(&mut d, "the new run is paired", |i| i.host.host.ready());
        assert_eq!(again.host.paired_after, Some(2), "`hello` and `resume`");
        assert!(
            !again.saw(|e| matches!(
                e,
                Event::Stopped(_)
                    | Event::LinkLost
                    | Event::WrongCode { .. }
                    | Event::Locked { .. }
            )),
            "replaced after {replace_after}: {:?}",
            again.host.events
        );
        again.step(&mut d, Command::Backup);
        assert!(
            again.saw(|e| *e == Event::BackupDone),
            "{:?}",
            again.host.events
        );
        // Nothing was counted, nothing ended the session over the protocol
        let journal = p.journal();
        assert!(
            !journal.contains("Frame("),
            "replaced after {replace_after}: {journal}"
        );
    }
}

// --- §5.7's write rules, as far as a socket pair shows them ---------------------------

fn message(seq: u32, id: u64, msg: HostMsg) -> Vec<u8> {
    let json = HostFrame { id, msg }.to_json().unwrap();
    frame::encode(Kind::Message, seq, 0, json.as_bytes(), MAX_DATA).unwrap()
}

fn hello(nonce: u8) -> Vec<u8> {
    message(
        0,
        1,
        HostMsg::Hello(HostHello {
            proto: vec![1],
            installer_name: "test".into(),
            installer_version: "1".into(),
            max_data: MAX_DATA,
            nonce: [nonce; 32],
        }),
    )
}

fn read_message(bytes: &[u8]) -> (u32, PhoneFrame) {
    let f = frame::decode(bytes, MAX_DATA).expect("a valid frame");
    (
        f.seq,
        PhoneFrame::parse(f.payload).expect("a valid message"),
    )
}

#[test]
fn a_host_that_never_reads_cannot_hold_the_daemon() {
    let p = Phone::new();
    // (each write gets 300 ms where the phone gives it 30 s)
    let mut d = p.daemon_with(&["--write-timeout-ms", "300"]);
    assert!(d.settled());
    d.enable();
    assert!(d.send(&hello(1)));
    // `status` again and again, and never a read. The phone's answers fill
    // the bus; its queue fills to 8; it stops reading; and then the host's
    // own writes are no longer taken either (here: not within 50 ms).
    d.sock
        .set_write_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut sent = 0u32;
    let mut held_back = false;
    let before = Instant::now();
    while !p.journal().contains("a write was not taken in time") {
        assert!(
            before.elapsed() < Duration::from_secs(8),
            "the write is never given up"
        );
        for _ in 0..32 {
            if d.send(&message(sent + 1, u64::from(sent) + 2, HostMsg::Status)) {
                sent += 1;
            } else {
                held_back = true;
                break;
            }
        }
    }
    assert!(
        held_back,
        "the daemon went on reading ({sent} frames) while its own frames waited for the bus"
    );
    // A write the bus did not take in time ends the session, and what was
    // queued behind it is cancelled
    assert_eq!(d.exited(), None, "the daemon goes on");
    // The host reads at last: whole frames, numbered one by one from the
    // `hello` answer, none of them past what the bus held
    let mut seq = 0;
    while let Some(frame) = d.recv(Duration::from_millis(400)) {
        let (got, _) = read_message(&frame);
        assert_eq!(got, seq);
        seq += 1;
    }
    assert!(seq > 8 && seq < sent, "{seq} answers read of {sent} asked");
    // And the next `hello` is answered, as frame 0, with nothing replugged.
    // (What the host sent before it are frames of a session that ended:
    // dropped, not answered.)
    let before = Instant::now();
    let answered = loop {
        assert!(
            before.elapsed() < Duration::from_secs(8),
            "no answer to `hello`"
        );
        d.send(&hello(2));
        if let Some(frame) = d.recv(Duration::from_millis(100)) {
            let (seq, msg) = read_message(&frame);
            if let PhoneMsg::Hello(h) = msg.msg {
                if h.host_nonce == [2; 32] {
                    break seq;
                }
            }
        }
    };
    assert_eq!(answered, 0);
}

// --- What is for a scratch root only ---------------------------------------------------

#[test]
fn the_test_options_are_refused_without_a_scratch_root() {
    // (refused while the arguments are read: nothing is opened)
    for option in [
        "--clock-scale",
        "--poll-ms",
        "--write-timeout-ms",
        "--max-data",
        "--ack-every",
        "--window",
        "--working-space",
        "--die-file",
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_omamobile-linkd"))
            .args(["--transport", "stdio", option, "5"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{option}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("need a scratch --root"));
    }
}
