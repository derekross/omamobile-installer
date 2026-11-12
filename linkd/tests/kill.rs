// SPDX-License-Identifier: MIT
//! `kill` (INSTALL-LINK-PLAN §11.3, F7): through a whole install the real
//! daemon process is killed (SIGKILL: no clean-up) at each of its kill
//! points in turn (before each thing on each list the machine returns,
//! and between the single writes of each effect that makes more than one,
//! a durable write's temporary file and its rename among them) and
//! started again, as systemd would while the unit's condition holds. The
//! install reaches `finished` every time, `done` is never written early,
//! the answers are placed once, and the phone restarts once.
#![cfg(target_os = "linux")]
mod common;

use common::*;
use omamobile_link::host::{Command, Event};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A whole install, driven as an installer's flow and an owner would. The
/// daemon dies at kill point `kill_at`. Returns the kill points passed by a
/// daemon that was not killed, and whether it was.
fn install(kill_at: Option<u64>) -> (u64, bool) {
    const NEVER: u64 = 1_000_000;
    let who = || format!("killed at {kill_at:?}");
    let mut p = Phone::new();
    let die = p.path("/kill-points");
    std::fs::write(&die, kill_at.unwrap_or(NEVER).to_string()).unwrap();
    let die = die.to_str().unwrap().to_string();
    let spawn = |p: &Phone| p.daemon_with(&["--die-file", &die]);
    let mut d = spawn(&p);
    let mut i = Installer::new();
    let mut todo = vec![
        Command::Answers(answers()),
        Command::AwaitFirstboot,
        Command::Backup,
        Command::Factory(factory_begin(&image())),
        Command::Provision,
        Command::Restart,
        Command::Smoke,
        Command::Finish,
    ];
    todo.reverse();
    let mut current: Option<Command> = None;
    let mut finish_asked = false;
    let mut reboots_seen = 0;
    let mut deaths = 0;
    let began = Instant::now();
    loop {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "{}: stuck at {:?}, state {:?}, events {:?}\n{}",
            who(),
            current,
            p.state(),
            i.host.events,
            p.journal()
        );
        // Never `done` early, and never anything but `finished`
        match p.done().as_deref() {
            None => {}
            Some("finished") => {
                assert!(finish_asked, "{}: done before `finish`", who());
                // The end is completed, by this daemon or by the one
                // systemd starts after it while the marker is there
                let status = d.wait_exit();
                if p.marker() {
                    assert_eq!(status, -9, "{}", who());
                    deaths += 1;
                    d = spawn(&p);
                    assert_eq!(d.wait_exit(), 0, "{}", who());
                }
                break;
            }
            Some(other) => panic!("{}: done as {other}", who()),
        }
        if p.reboots() > reboots_seen {
            // `systemctl reboot` was called: the planned restart
            reboots_seen = p.reboots();
            d.kill();
            i.pump(&mut d, Duration::from_millis(2));
            p.new_boot();
            d = spawn(&p);
            continue;
        }
        if let Some(status) = d.exited() {
            // The daemon died: its end of the link is closed, the host
            // loses the link. systemd starts it again, in the same boot.
            assert_eq!(
                status,
                -9,
                "{}: the daemon ended by itself\n{}",
                who(),
                p.journal()
            );
            deaths += 1;
            i.pump(&mut d, Duration::from_millis(2));
            assert!(p.marker(), "{}: no marker and no done", who());
            d = spawn(&p);
            continue;
        }
        // The jobs that need no daemon go on without it, and end
        if p.unit("omamobile-firstboot.service") == "activating" {
            p.finish_firstboot(&[]);
            continue;
        }
        if p.unit("omamobile-provision-run.service") == "active" {
            p.finish_provision(None);
            continue;
        }
        if !d.up {
            if d.settled() {
                i.connect(&mut d);
            }
            continue;
        }
        if !i.host.host.ready() {
            match i.host.events.last() {
                Some(Event::NeedCode) => {
                    // On the first-start screen, or in the session's window
                    let code = match d.screen().code() {
                        Some(code) => Some(code),
                        None => d.agent().code(),
                    };
                    if let Some(code) = code {
                        i.command(&mut d, Command::Pair(code));
                    }
                }
                // The owner logs in: the agent's connection is the session
                Some(Event::NeedSession) => {
                    d.agent();
                }
                // The owner taps the notice
                Some(Event::NeedTap) => d.agent().say("show"),
                _ => {}
            }
            i.pump(&mut d, Duration::from_millis(3));
            continue;
        }
        if i.host.host.busy() {
            i.pump(&mut d, Duration::from_millis(2));
            continue;
        }
        // The step before this one ended: with what?
        let events = i.host.take_events();
        if let Some(cmd) = current.take() {
            let failed = events.iter().any(|e| {
                matches!(
                    e,
                    Event::Refused { .. } | Event::Failed { .. } | Event::NotNow { .. }
                )
            });
            if failed {
                // An installer's "try again": the step, once more
                todo.push(cmd);
            }
        }
        let Some(cmd) = todo.pop() else {
            i.pump(&mut d, Duration::from_millis(2));
            continue;
        };
        if cmd == Command::Smoke {
            d.agent();
        }
        finish_asked |= cmd == Command::Finish;
        current = Some(cmd.clone());
        i.command(&mut d, cmd);
        i.pump(&mut d, Duration::from_millis(2));
    }
    assert!(!p.marker(), "{}", who());
    assert!(p.state().is_none(), "{}: the state file is left", who());
    assert_eq!(p.image_len(), None, "{}", who());
    assert_eq!(p.firstboot_runs, 1, "{}: the answers placed once", who());
    assert_eq!(
        p.firstboot_ran_with,
        answers().firstboot_conf().as_bytes(),
        "{}: first boot ran with the answers that were sent",
        who()
    );
    assert_eq!(p.reboots(), 1, "{}: one restart", who());
    assert!(p.provision_starts() >= 1, "{}", who());
    assert!(
        p.read("/etc/omamobile/boot-update.conf").is_some(),
        "{}: boot-update.conf",
        who()
    );
    // The gadget: put back at the end, and after the daemon that died.
    // `add` never meets a gadget that already has the link's function.
    let gadget = p.gadget();
    assert_eq!(
        gadget.last().map(String::as_str),
        Some("remove"),
        "{}",
        who()
    );
    for pair in gadget.windows(2) {
        assert!(pair[0] != pair[1], "{}: {gadget:?}", who());
    }
    // The kill point was reached and the daemon died there, once; or this
    // run had fewer points than that (how many `status` and `progress`
    // frames an install has depends on its timing)
    let left: Option<u64> = p.read("/kill-points").map(|n| n.trim().parse().unwrap());
    match (kill_at, left) {
        (None, Some(left)) => {
            assert_eq!(deaths, 0, "{}", who());
            (NEVER - left, false)
        }
        (Some(_), None) => {
            assert_eq!(deaths, 1, "{}", who());
            (0, true)
        }
        (Some(k), Some(left)) => {
            assert_eq!(deaths, 0, "{}", who());
            (k - left, false)
        }
        (None, None) => panic!("the daemon died with no kill asked for"),
    }
}

#[test]
fn kill_the_daemon_at_every_kill_point_of_a_whole_install() {
    let (points, _) = install(None);
    assert!(points > 200, "{points}");
    // After each: the daemon dies before the next thing it would do
    let next = AtomicU64::new(0);
    let died = AtomicU64::new(0);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= points + 4 {
                    return;
                }
                let (passed, killed) = install(Some(k));
                if killed {
                    died.fetch_add(1, Ordering::SeqCst);
                } else {
                    // Only the last few can be past a run's end
                    assert!(
                        k + 12 >= points && passed <= k,
                        "{k} of {points}: not killed"
                    );
                }
            });
        }
    });
    let died = died.load(Ordering::SeqCst);
    assert!(died + 12 >= points, "{died} of {points}");
    println!("{points} kill points in an install; killed at {died} of them, each survived");
}
