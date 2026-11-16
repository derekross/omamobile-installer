// SPDX-License-Identifier: MIT
//! `rows` (INSTALL-LINK-PLAN §11.3, F7): every row of §6.5's three start-up
//! tables as a scratch root, and what a real daemon process does to it.
//! The rules are the crate's `reconcile`; what is under test here is that
//! the daemon reads the phone into `Found` rightly and carries the result
//! out, and that it never changes the gadget when the link is not offered.
#![cfg(target_os = "linux")]
mod common;

use common::*;
use omamobile_link::msg::{JobState, Stage};
use omamobile_link::phone::{FactoryFile, Persistent};
use std::fs;

const STATE_DIR: &str = "/var/lib/omamobile/install-link";

/// Start the daemon and let its start-up rules run. `None`: the link is
/// offered (the daemon is then killed). `Some(status)`: it ended.
fn start(p: &Phone) -> Option<i32> {
    let mut d = p.daemon();
    if d.settled() {
        assert_eq!(p.gadget().last().map(String::as_str), Some("add"));
        None
    } else {
        Some(d.wait_exit())
    }
}

/// The link ended at this start, with this word in `done`, and the gadget
/// was never touched.
fn ended(p: &Phone, done: &str) {
    assert_eq!(start(p), Some(0));
    assert_eq!(p.done().as_deref(), Some(done));
    assert!(!p.marker(), "the marker is deleted");
    assert!(p.gadget().is_empty(), "no gadget change: {:?}", p.gadget());
}

fn offered(p: &Phone) -> Persistent {
    assert_eq!(start(p), None, "{}", p.journal());
    assert!(p.marker() && p.done().is_none());
    p.state().expect("a state file")
}

fn image(p: &Phone, len: usize) {
    p.write(&format!("{STATE_DIR}/factory.zip"), &vec![7u8; len]);
}

fn record(size: u64, synced: u64, verified: bool) -> Option<FactoryFile> {
    Some(FactoryFile {
        size,
        sha256: [3; 32],
        synced,
        verified,
    })
}

// --- The first table ---------------------------------------------------------

#[test]
fn row_1_done_deletes_the_marker_and_exits() {
    for word in [
        "finished",
        "expired backup firmware cameras",
        "not-first-start",
    ] {
        let p = Phone::new();
        p.write(&format!("{STATE_DIR}/done"), format!("{word}\n").as_bytes());
        p.firstboot_was_done();
        let kept = p.state_now(|s| s.stage = Stage::FirstbootDone);
        ended(&p, word);
        // An end that a kill cut short is completed: `finished` leaves no
        // state; the other ends keep it for `reopen`
        if word == "finished" {
            assert!(p.state().is_none());
        } else {
            assert_eq!(p.state(), Some(kept));
        }
    }
    // With no marker the daemon does nothing at all
    let p = Phone::new();
    p.remove("/etc/omamobile/install-link");
    assert_eq!(start(&p), Some(0));
    assert!(p.state().is_none() && p.done().is_none() && p.gadget().is_empty());
}

#[test]
fn row_2_a_first_start_writes_the_state_file_then_offers() {
    let p = Phone::new();
    let s = offered(&p);
    assert_eq!(
        s,
        Persistent {
            boot_id: p.boot_id(),
            ..Persistent::default()
        }
    );
    assert_eq!(p.gadget(), ["add"]);
}

#[test]
fn row_3_no_state_file_and_something_of_first_boots_is_not_a_first_start() {
    for what in ["firstboot.conf", "first boot's done", "an account"] {
        let p = Phone::new();
        match what {
            "firstboot.conf" => p.write("/etc/omamobile/firstboot.conf", b"USERNAME=owner\n"),
            "first boot's done" => p.write("/var/lib/omamobile/firstboot/done", b"x\n"),
            _ => p.add_account("someone", 1000, false),
        }
        ended(&p, "not-first-start");
        assert!(p.state().is_none(), "{what}");
        assert!(
            what != "firstboot.conf" || p.exists("/etc/omamobile/firstboot.conf"),
            "install-phone's file is left alone"
        );
    }
}

#[test]
fn rows_4_and_5_a_state_file_that_cannot_be_read() {
    for bytes in [
        &b"{\"v\":1,\"stage\":\"wai"[..],
        b"{\"v\":2,\"stage\":\"waiting\"}",
        b"",
    ] {
        // No account: as a first start
        let p = Phone::new();
        p.write(&format!("{STATE_DIR}/state"), bytes);
        assert_eq!(offered(&p).stage, Stage::Waiting);
        // ... and a firstboot.conf beside it is the link's own (the answers
        // were taken): `firstboot`
        let p = Phone::new();
        p.write(&format!("{STATE_DIR}/state"), bytes);
        p.write("/etc/omamobile/firstboot.conf", b"USERNAME=owner\n");
        assert_eq!(offered(&p).stage, Stage::Firstboot);
        // An account: it never guesses
        let p = Phone::new();
        p.write(&format!("{STATE_DIR}/state"), bytes);
        p.firstboot_was_done();
        ended(&p, "state-unreadable");
    }
}

#[test]
fn row_6_stage_finished_completes_the_end() {
    let p = Phone::new();
    p.firstboot_was_done();
    p.state_now(|s| {
        s.stage = Stage::Finished;
        s.token_hash = Some([9; 32]);
    });
    ended(&p, "finished");
    assert!(p.state().is_none(), "the state and the token are deleted");
}

#[test]
fn row_7_the_budget_a_boot_is_counted_once_and_the_fourth_ends_the_link() {
    // A new boot at a stage with an account is a setup boot, counted once
    // however often the daemon starts in it
    let p = Phone::new();
    p.firstboot_was_done();
    p.state_now(|s| {
        s.stage = Stage::FirstbootDone;
        s.boot_id = "the boot before".into();
    });
    for _ in 0..3 {
        let s = offered(&p);
        assert_eq!((s.setup_boots, s.boot), (1, 1));
        assert_eq!(s.boot_id, p.boot_id());
    }
    // At `provisioned` the new boot is also `boot` 2
    let p = Phone::new();
    p.firstboot_was_done();
    p.state_now(|s| {
        s.stage = Stage::Provisioned;
        s.boot_id = "the boot before".into();
    });
    let s = offered(&p);
    assert_eq!((s.setup_boots, s.boot), (1, 2));
    // The fourth setup boot: the end, before any gadget change
    for stage in [
        Stage::Firstboot,
        Stage::FirstbootFailed,
        Stage::FirstbootDone,
        Stage::Provisioned,
    ] {
        let p = Phone::new();
        p.firstboot_was_done();
        if matches!(stage, Stage::Firstboot | Stage::FirstbootFailed) {
            fs::remove_file(p.path("/var/lib/omamobile/firstboot/done")).unwrap();
            p.write("/etc/omamobile/firstboot.conf", b"USERNAME=owner\n");
            if stage == Stage::FirstbootFailed {
                p.set_unit("omamobile-firstboot.service", "failed");
            }
        }
        p.state_now(|s| {
            s.stage = stage;
            s.boot_id = "the boot before".into();
            s.setup_boots = 3;
            s.token_hash = Some([9; 32]);
            s.factory = record(100, 50, false);
        });
        image(&p, 50);
        if stage == Stage::Provisioned {
            ended(&p, "finished");
            assert!(p.state().is_none());
        } else {
            ended(&p, "expired backup firmware cameras");
            // What `expired` leaves: the stage, no token, no image
            let s = p.state().unwrap();
            assert_eq!((s.stage, s.token_hash, s.factory), (stage, None, None));
        }
        assert_eq!(p.image_len(), None, "{stage:?}");
    }
}

// --- The second table: what first boot itself left ---------------------------

#[test]
fn table_2_first_boots_done_makes_it_firstboot_done() {
    for stage in [Stage::Waiting, Stage::Firstboot, Stage::FirstbootFailed] {
        for conf in [false, true] {
            let p = Phone::new();
            p.firstboot_was_done();
            if conf {
                p.write("/etc/omamobile/firstboot.conf", b"USERNAME=owner\n");
            }
            p.state_now(|s| {
                s.stage = stage;
                s.firstboot_failed = if stage == Stage::FirstbootFailed {
                    vec!["login".into()]
                } else {
                    vec![]
                };
            });
            let s = offered(&p);
            assert_eq!(s.stage, Stage::FirstbootDone, "{stage:?} {conf}");
            assert!(s.firstboot_failed.is_empty());
        }
    }
}

#[test]
fn table_2_a_firstboot_conf_and_no_done_follows_the_unit() {
    for stage in [Stage::Waiting, Stage::Firstboot, Stage::FirstbootFailed] {
        for unit in ["inactive", "activating", "failed"] {
            let mut p = Phone::new();
            p.write("/etc/omamobile/firstboot.conf", b"USERNAME=owner\n");
            if unit == "failed" {
                // (as the script leaves it: the account, the log, the file kept)
                p.finish_firstboot(&["user", "login"]);
                p.remove("/etc/omamobile/boot-update.conf");
            } else if unit == "activating" {
                p.set_unit("omamobile-firstboot.service", unit);
            }
            p.state_now(|s| s.stage = stage);
            let s = offered(&p);
            if unit == "failed" {
                assert_eq!(s.stage, Stage::FirstbootFailed, "{stage:?}");
                // The steps its log marks FAILED; the account step names no user
                assert_eq!(s.firstboot_failed, ["user", "login"], "{stage:?}");
            } else {
                assert_eq!(s.stage, Stage::Firstboot, "{stage:?} {unit}");
                assert!(s.firstboot_failed.is_empty());
                // A unit that has not run yet in this boot is started (the
                // daemon may have been killed just before it started it)
                assert_eq!(p.unit("omamobile-firstboot.service"), "activating");
            }
            // The answers were taken: boot-update.conf is written if missing
            assert!(
                p.read("/etc/omamobile/boot-update.conf")
                    .is_some_and(|t| t.ends_with("AUTO_WRITE=yes\n")),
                "{stage:?} {unit}"
            );
            // This boot is a setup boot only when it was not counted yet
            assert_eq!(s.setup_boots, 0, "the same boot");
        }
    }
}

#[test]
fn table_2_neither_file_and_the_account() {
    for stage in [Stage::Waiting, Stage::Firstboot, Stage::FirstbootFailed] {
        // The account first boot's `user` file names: it deleted
        // firstboot.conf, and power was lost before its `done`
        let p = Phone::new();
        p.add_account("owner", 1000, true);
        p.state_now(|s| s.stage = stage);
        assert_eq!(offered(&p).stage, Stage::FirstbootDone, "{stage:?}");
        // Another account: the link ends
        let p = Phone::new();
        p.add_account("somebody", 1000, false);
        p.state_now(|s| s.stage = stage);
        ended(&p, "inconsistent");
        // ... also when first boot's file names someone, and there is one more
        let p = Phone::new();
        p.add_account("owner", 1000, true);
        p.add_account("somebody", 1001, false);
        p.state_now(|s| s.stage = stage);
        ended(&p, "inconsistent");
        // No account: `waiting` (from `firstboot`: the answers are gone)
        let p = Phone::new();
        p.state_now(|s| {
            s.stage = stage;
            s.firstboot_failed = vec!["login".into()];
        });
        let s = offered(&p);
        assert_eq!(s.stage, Stage::Waiting, "{stage:?}");
        assert!(s.firstboot_failed.is_empty());
    }
}

// --- The third table: at any stage before `finished` --------------------------

/// A phone at `firstboot-done` in this boot, with `change` made to its
/// state file.
fn set_up(change: impl FnOnce(&mut Persistent)) -> Phone {
    let p = Phone::new();
    p.firstboot_was_done();
    p.state_now(|s| {
        s.stage = Stage::FirstbootDone;
        change(s);
    });
    p
}

#[test]
fn table_3_the_image_and_its_record() {
    // A record, no file: the record is dropped
    let p = set_up(|s| s.factory = record(100, 40, false));
    assert_eq!(offered(&p).factory, None);
    // A file, no record: the file is deleted
    let p = set_up(|_| {});
    image(&p, 40);
    offered(&p);
    assert_eq!(p.image_len(), None);
    // Longer than `synced`: cut to it
    let p = set_up(|s| s.factory = record(100, 40, false));
    image(&p, 64);
    assert_eq!(offered(&p).factory, record(100, 40, false));
    assert_eq!(p.image_len(), Some(40));
    // Shorter than `synced`: a sync that did not hold. Emptied, from 0
    let p = set_up(|s| s.factory = record(100, 40, false));
    image(&p, 39);
    assert_eq!(offered(&p).factory, record(100, 0, false));
    assert_eq!(p.image_len(), Some(0));
    // As recorded: left alone
    let p = set_up(|s| s.factory = record(100, 100, true));
    image(&p, 100);
    assert_eq!(offered(&p).factory, record(100, 100, true));
    assert_eq!(p.image_len(), Some(100));
    // "Verified", and the length is not `size`: the mark is taken away
    let p = set_up(|s| s.factory = record(100, 100, true));
    image(&p, 101);
    assert_eq!(offered(&p).factory, record(100, 100, false));
    assert_eq!(p.image_len(), Some(100));
    let p = set_up(|s| s.factory = record(100, 100, true));
    image(&p, 99);
    assert_eq!(offered(&p).factory, record(100, 0, false));
    // Stage `provisioned` and an image file: deleted, with its record
    let p = set_up(|s| {
        s.stage = Stage::Provisioned;
        s.provision = JobState::Done;
        s.factory = record(100, 100, true);
    });
    image(&p, 100);
    assert_eq!(offered(&p).factory, None);
    assert_eq!(p.image_len(), None);
}

#[test]
fn table_3_the_backups_mark_and_the_pending_checksums() {
    let sums = format!("{STATE_DIR}/modem-nv.sha256");
    // `backup-done`, no pending file: the backup is taken again
    let p = set_up(|s| s.backup_done = true);
    assert!(!offered(&p).backup_done);
    // Both: kept
    let p = set_up(|s| s.backup_done = true);
    p.write(&sums, b"x  efs.img\n");
    assert!(offered(&p).backup_done);
    assert!(p.exists(&sums));
    // A pending file, no `backup-done`: deleted
    let p = set_up(|_| {});
    p.write(&sums, b"x  efs.img\n");
    assert!(!offered(&p).backup_done);
    assert!(!p.exists(&sums));
    // At `provisioned` the mark stays whatever is found (P2)
    let p = set_up(|s| {
        s.stage = Stage::Provisioned;
        s.provision = JobState::Done;
        s.backup_done = true;
    });
    assert!(offered(&p).backup_done);
}

#[test]
fn table_3_provisioning_that_was_running() {
    let running = |s: &mut Persistent| {
        s.provision = JobState::Running;
        s.backup_done = true;
        s.factory = record(100, 100, true);
    };
    let work = "/var/lib/omamobile/provision/work.standin/x";
    let result = format!("{STATE_DIR}/provision.result");
    let files = |p: &Phone| {
        image(p, 100);
        p.write(&format!("{STATE_DIR}/modem-nv.sha256"), b"x  efs.img\n");
        p.write(work, b"unpacked");
    };
    // Its unit is active (the daemon alone restarted): it goes on watching,
    // touches nothing, and takes the end when it comes
    let p = set_up(running);
    files(&p);
    p.set_unit("omamobile-provision-run.service", "active");
    let mut d = p.daemon();
    assert!(d.settled());
    assert_eq!(p.state().unwrap().provision, JobState::Running);
    assert!(p.exists(work) && p.image_len() == Some(100));
    p.finish_provision(None);
    let before = std::time::Instant::now();
    while p.state().unwrap().stage != Stage::Provisioned {
        assert!(before.elapsed().as_secs() < 10, "{}", p.journal());
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    drop(d);
    // No unit, a result in which every required step is `ok`: `provisioned`
    for failed in [None, Some("camera")] {
        let p = set_up(running);
        files(&p);
        p.finish_provision(failed);
        p.write(work, b"left by a kill");
        let s = offered(&p);
        assert_eq!((s.stage, s.provision), (Stage::Provisioned, JobState::Done));
        assert_eq!(
            s.optional_failed,
            failed.map(str::to_string).into_iter().collect::<Vec<_>>()
        );
        assert_eq!(p.image_len(), None, "the image is deleted");
        assert!(!p.exists(work), "the working directory is removed");
    }
    // No unit, a result with a required step failed: failed at that step
    let p = set_up(running);
    files(&p);
    p.finish_provision(Some("gnss"));
    p.write(work, b"left by a kill");
    let s = offered(&p);
    assert_eq!(
        (s.stage, s.provision),
        (Stage::FirstbootDone, JobState::Failed)
    );
    assert_eq!(s.provision_failed, "gnss");
    assert_eq!(p.image_len(), Some(100), "the image is kept");
    assert!(!p.exists(work));
    // No unit and no result, or one that is not a result: `interrupted`
    for junk in [
        None,
        Some(&b"{\"version\":1,\"ok\":tr"[..]),
        Some(b"{\"steps\":[]}"),
    ] {
        let p = set_up(running);
        files(&p);
        if let Some(junk) = junk {
            p.write(&result, junk);
        }
        let s = offered(&p);
        assert_eq!(s.provision, JobState::Failed);
        assert_eq!(s.provision_failed, "interrupted");
        assert_eq!(p.image_len(), Some(100));
        assert!(!p.exists(work));
    }
}

#[test]
fn table_3_a_working_directory_and_the_daemons_own_leftovers() {
    // A working directory and no unit: removed
    let p = set_up(|_| {});
    p.write("/var/lib/omamobile/provision/work.ab12/super.img", b"x");
    p.write("/var/lib/omamobile/provision/lock", b"");
    offered(&p);
    assert!(!p.exists("/var/lib/omamobile/provision/work.ab12"));
    assert!(
        p.exists("/var/lib/omamobile/provision/lock"),
        "not the daemon's"
    );
    // A candidate, and temporary files of its own: deleted
    let p = Phone::new();
    let left = [
        "/run/omamobile-install-link/firstboot.conf".to_string(),
        format!("{STATE_DIR}/.state.linkd.tmp"),
        "/etc/omamobile/.firstboot.conf.linkd.tmp".to_string(),
    ];
    for f in &left {
        p.write(f, b"USERNAME=owner\n");
    }
    p.write("/etc/omamobile/.other.tmp", b"someone else's");
    assert_eq!(offered(&p).stage, Stage::Waiting);
    for f in &left {
        assert!(!p.exists(f), "{f}");
    }
    assert!(p.exists("/etc/omamobile/.other.tmp"));
}

// --- What a start needs before it offers anything -----------------------------

#[test]
fn nothing_is_offered_and_nothing_written_without_a_serial_or_a_codename() {
    for what in ["/proc/bootconfig", "/usr/share/omamobile/device.conf"] {
        let p = Phone::new();
        p.write(what, b"");
        assert_eq!(start(&p), Some(1), "{what}");
        assert!(p.state().is_none() && p.done().is_none() && p.marker());
        assert!(p.gadget().is_empty());
    }
}
