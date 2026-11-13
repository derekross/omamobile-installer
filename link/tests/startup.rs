// SPDX-License-Identifier: MIT
//! What the daemon finds at its start, and when the link ends: the proofs
//! INSTALL-LINK-PLAN §11.3 names `rows` and `kill` (F7) and `budget` (F3),
//! as far as they run without a daemon.
//!
//! `rows`: every row of §6.5's three tables, as the files a start would
//! find (the stand-in phone's, in memory) and what the machine has the
//! daemon do to them. `kill`: through a whole install the "daemon" is
//! killed after each single output of each list the machine returns, and
//! started again from what is then on the "phone". `budget`: the ten
//! unpaired minutes by the clock, the three setup boots, `expired`,
//! `finished` and `reopen`.
//!
//! Stage L2 runs the same against a real daemon in a scratch root; what
//! these cannot show is that the daemon fills in [`Found`] rightly.
mod common;
use common::client::*;
use common::rig::*;
use common::*;
use omamobile_link::host::{Command, Event};
use omamobile_link::msg::*;
use omamobile_link::phone::*;
use omamobile_link::sim::{Loopback, SimHost, SimPhone, TestRng};

fn optional() -> Vec<String> {
    vec!["camera".into()]
}

/// A phone with no daemon started on it yet.
fn files() -> SimPhone {
    let mut p = SimPhone::blank(phone_config(), 3);
    p.parts = parts();
    p
}

/// ... whose state file says `stage`, and nothing else is there.
fn with_state(stage: Stage) -> SimPhone {
    let mut p = files();
    p.state_exists = true;
    p.state_file.stage = stage;
    p
}

fn factory(size: u64, synced: u64, verified: bool) -> Option<FactoryFile> {
    Some(FactoryFile {
        size,
        sha256: [7; 32],
        synced,
        verified,
    })
}

// --- rows: the first table ---------------------------------------------------

#[test]
fn rows_1_done_is_the_end() {
    // The unit's condition normally keeps the daemon from starting at all
    let mut p = files();
    p.done = Some("finished".into());
    p.state_exists = true;
    p.start(true, 0);
    assert!(p.phone.stopped() && !p.offered);
    assert!(!p.marker, "the marker that was still there is deleted");
    assert_eq!(p.log, vec![Effect::DeleteMarker]);
    // ... and with no marker there is nothing to do at all
    let mut p = files();
    p.done = Some("expired backup firmware cameras".into());
    p.marker = false;
    p.start(true, 0);
    assert!(p.phone.stopped() && p.log.is_empty());
}

#[test]
fn rows_2_a_first_start_writes_the_state_file_and_then_offers_the_link() {
    let mut p = files();
    p.start(true, 0);
    assert!(p.offered && !p.phone.stopped());
    let first = Persistent {
        boot_id: p.boot_id(),
        ..Persistent::default()
    };
    assert!(p.state_exists && p.state_file == first);
    assert!(p.phone.code().is_some());
    // The state file is on disk before the gadget is touched
    assert!(
        matches!(
            &p.log[..],
            [
                Effect::Persist(_),
                Effect::Screen(Screen::Code(_)),
                Effect::Offer
            ]
        ),
        "{:?}",
        p.log
    );
    assert_eq!(p.done, None);
}

#[test]
fn rows_3_a_phone_the_link_never_set_up_is_never_offered_it() {
    // A marker, and a firstboot.conf, a first-boot done or an account, but
    // no state file: installed before the link existed, or with
    // tools/install-phone
    type Arrange = fn(&mut SimPhone);
    let cases: [(&str, Arrange); 4] = [
        ("firstboot.conf", |p| p.conf = Some("USERNAME=x\n".into())),
        ("first boot's done", |p| p.fb_done = true),
        ("an account", |p| p.account = Account::Firstboot),
        ("another account", |p| p.account = Account::Other),
    ];
    for (what, arrange) in cases {
        let mut p = files();
        arrange(&mut p);
        p.start(true, 0);
        assert!(p.phone.stopped() && !p.offered, "{what}");
        assert_eq!(p.done.as_deref(), Some("not-first-start"), "{what}");
        assert!(!p.marker && !p.state_exists, "{what}");
        assert_eq!(
            p.log,
            vec![Effect::Refuse {
                reason: Refusal::NotFirstStart
            }],
            "{what}: no gadget change, nothing else"
        );
        // It is gone for good: the next boot's start finds `done`
        p.start(true, 0);
        assert!(p.phone.stopped() && !p.offered, "{what}");
    }
}

#[test]
fn rows_4_and_5_a_state_file_that_cannot_be_read() {
    // No account: as a first start. Nothing is lost, and a code can be shown
    let mut p = files();
    p.state_exists = true;
    p.state_unreadable = true;
    p.start(true, 0);
    assert!(p.offered && p.phone.code().is_some());
    let first = Persistent {
        boot_id: p.boot_id(),
        ..Persistent::default()
    };
    assert!(!p.state_unreadable && p.state_file == first);
    // An account: it never guesses about a phone that has one
    for account in [Account::Firstboot, Account::Other] {
        let mut p = files();
        p.state_exists = true;
        p.state_unreadable = true;
        p.account = account;
        p.fb_done = true;
        p.factory_exists = true;
        p.work_dir = true;
        p.start(true, 0);
        assert!(p.phone.stopped() && !p.offered);
        assert_eq!(p.done.as_deref(), Some("state-unreadable"));
        assert!(!p.marker);
        assert!(
            !p.factory_exists && !p.work_dir,
            "the image goes when the link ends, however it ends"
        );
    }
    // What "cannot be read" is: damaged, or another version
    for bytes in [
        &b""[..],
        b"{",
        b"\0\0\0\0",
        br#"{"v":2,"stage":"waiting"}"#,
        br#"{"v":1,"stage":"waiting","boot":1,"late_boots":0,"backup_done":false,"provision":"none","provision_failed":"","firstboot_failed":[]}"#,
    ] {
        assert_eq!(StateFile::read(Some(bytes)), StateFile::Unreadable);
    }
    assert_eq!(StateFile::read(None), StateFile::Missing);
    let p = Persistent::default();
    assert_eq!(
        StateFile::read(Some(p.to_json().as_bytes())),
        StateFile::Read(p)
    );
}

#[test]
fn rows_6_a_finish_that_was_begun_is_completed() {
    // Power was lost after the state file said `finished` and before
    // `done` was written
    let mut p = with_state(Stage::Finished);
    p.fb_done = true;
    p.account = Account::Firstboot;
    p.start(true, 0);
    assert!(p.phone.stopped() && !p.offered && p.finished);
    assert_eq!(p.done.as_deref(), Some("finished"));
    assert!(!p.marker && !p.state_exists);
}

// --- rows: the second table, what first boot itself left -----------------------

#[test]
fn rows_what_first_boot_left() {
    use FirstbootUnit::{Failed, NotRun, Running};
    let failed = || Failed(vec!["SSH keys".into(), "Wi-Fi".into()]);
    // (first boot's done, firstboot.conf, account, the unit) -> the stage,
    // from each of the three stages the table is for
    type Row = (
        &'static str,
        bool,
        bool,
        Account,
        FirstbootUnit,
        Option<Stage>,
    );
    let rows: Vec<Row> = vec![
        // Row 1: it ran to its end, perhaps while the daemon was down
        (
            "done",
            true,
            false,
            Account::Firstboot,
            NotRun,
            Some(Stage::FirstbootDone),
        ),
        (
            "done, conf",
            true,
            true,
            Account::Firstboot,
            NotRun,
            Some(Stage::FirstbootDone),
        ),
        (
            "done, no account",
            true,
            false,
            Account::None,
            NotRun,
            Some(Stage::FirstbootDone),
        ),
        // Row 2: the answers were taken
        (
            "conf, not run yet",
            false,
            true,
            Account::None,
            NotRun,
            Some(Stage::Firstboot),
        ),
        (
            "conf, running",
            false,
            true,
            Account::Firstboot,
            Running,
            Some(Stage::Firstboot),
        ),
        (
            "conf, failed",
            false,
            true,
            Account::Firstboot,
            failed(),
            Some(Stage::FirstbootFailed),
        ),
        (
            "conf, failed, no account",
            false,
            true,
            Account::None,
            failed(),
            Some(Stage::FirstbootFailed),
        ),
        // Row 3: the script deleted firstboot.conf, and power was lost
        // before it wrote its done
        (
            "neither, its account",
            false,
            false,
            Account::Firstboot,
            NotRun,
            Some(Stage::FirstbootDone),
        ),
        // Row 4
        (
            "neither, another account",
            false,
            false,
            Account::Other,
            NotRun,
            None,
        ),
        // Row 5: the answers are gone, or were never there
        (
            "nothing",
            false,
            false,
            Account::None,
            NotRun,
            Some(Stage::Waiting),
        ),
    ];
    for from in [Stage::Waiting, Stage::Firstboot, Stage::FirstbootFailed] {
        for (what, done, conf, account, unit, want) in rows.clone() {
            let mut p = with_state(from);
            if from == Stage::FirstbootFailed {
                p.state_file.firstboot_failed = vec!["an old one".into()];
            }
            p.fb_done = done;
            p.conf = conf.then(|| "USERNAME=owner\n".to_string());
            p.account = account;
            // (the daemon alone started again, so the unit is as it was)
            p.fb_unit = unit.clone();
            p.start(false, 0);
            let Some(want) = want else {
                assert!(p.phone.stopped() && !p.offered, "{what} from {from:?}");
                assert_eq!(p.done.as_deref(), Some("inconsistent"), "{what}");
                assert!(!p.marker, "{what}");
                continue;
            };
            assert!(p.offered, "{what} from {from:?}");
            assert_eq!(p.phone.stage(), want, "{what} from {from:?}");
            assert_eq!(p.state_file.stage, want, "{what} from {from:?}: recorded");
            assert_eq!(p.done, None, "{what}");
            // The steps its log marks FAILED, and no older ones
            let steps: &[&str] = if want == Stage::FirstbootFailed {
                &["SSH keys", "Wi-Fi"]
            } else {
                &[]
            };
            assert_eq!(p.state_file.firstboot_failed, steps, "{what} from {from:?}");
            // boot-update.conf is written if it is missing (Q3)
            assert_eq!(p.boot_update_conf, conf && !done, "{what} from {from:?}");
            // The first-start screen and its code, while it owns the display
            assert_eq!(
                p.phone.code().is_some(),
                want != Stage::FirstbootDone,
                "{what} from {from:?}"
            );
        }
    }
    // The case F7 is about: the answers were placed, and power was lost
    // before the stage was recorded. The link is not ended: it is at
    // `firstboot`, and the host learns so from `status`.
    let mut p = with_state(Stage::Waiting);
    p.conf = Some("USERNAME=owner\n".into());
    p.start(true, 0);
    assert!(p.offered && p.done.is_none() && p.marker);
    assert_eq!(p.state_file.stage, Stage::Firstboot);
    assert!(p.log.contains(&Effect::BootUpdateConf));
    // A failed unit whose log names no step still fails by name
    let mut p = with_state(Stage::Firstboot);
    p.conf = Some(String::new());
    p.fb_unit = Failed(vec![]);
    p.start(false, 0);
    assert_eq!(p.state_file.firstboot_failed, ["unknown"]);
    // In a new boot first boot's own retry has not run yet: `firstboot`
    // again, whatever was recorded
    let mut p = with_state(Stage::FirstbootFailed);
    p.state_file.firstboot_failed = vec!["SSH keys".into()];
    p.conf = Some(String::new());
    p.account = Account::Firstboot;
    p.start(true, 0);
    assert_eq!(p.state_file.stage, Stage::Firstboot);
    assert!(p.state_file.firstboot_failed.is_empty());
    // The table is for those three stages only
    for stage in [Stage::FirstbootDone, Stage::Provisioned] {
        let mut p = with_state(stage);
        p.account = Account::Other;
        p.start(false, 0);
        assert_eq!(p.phone.stage(), stage);
    }
}

// --- rows: the third table -----------------------------------------------------

#[test]
fn rows_the_image_and_its_record() {
    let done = || {
        let mut p = with_state(Stage::FirstbootDone);
        p.fb_done = true;
        p.account = Account::Firstboot;
        p
    };
    // A record of the image, no file: the record is dropped
    let mut p = done();
    p.state_file.factory = factory(9000, 4096, false);
    p.start(false, 0);
    assert_eq!(p.state_file.factory, None);
    // An image file, no record: the file is deleted
    let mut p = done();
    p.factory_exists = true;
    p.factory_file = vec![1; 5000];
    p.start(false, 0);
    assert!(!p.factory_exists && p.factory_file.is_empty());
    assert!(p.log.contains(&Effect::FactoryDelete));
    // The file longer than `synced`: cut to it; bytes after the last sync
    // are not trusted
    let mut p = done();
    p.state_file.factory = factory(9000, 4096, false);
    p.factory_exists = true;
    p.factory_file = vec![1; 5000];
    p.start(false, 0);
    assert_eq!(p.factory_file.len(), 4096);
    assert_eq!(p.state_file.factory, factory(9000, 4096, false));
    // The file shorter than `synced`: a sync that did not hold. Emptied.
    let mut p = done();
    p.state_file.factory = factory(9000, 8192, false);
    p.factory_exists = true;
    p.factory_file = vec![1; 8191];
    p.start(false, 0);
    assert!(p.factory_file.is_empty() && p.factory_exists);
    assert_eq!(p.state_file.factory, factory(9000, 0, false));
    // "Verified", and the file's length is not `size`: the mark goes
    for len in [8999usize, 9001] {
        let mut p = done();
        p.state_file.factory = factory(9000, 9000, true);
        p.factory_exists = true;
        p.factory_file = vec![1; len];
        p.start(false, 0);
        let f = p.state_file.factory.clone().unwrap();
        assert!(!f.verified, "{len}");
        assert!(p.factory_file.len() as u64 == f.synced, "{len}");
    }
    // As recorded: nothing is touched, nothing is written
    let mut p = done();
    p.state_file.factory = factory(9000, 9000, true);
    p.factory_exists = true;
    p.factory_file = vec![1; 9000];
    p.start(false, 0);
    assert_eq!(p.state_file.factory, factory(9000, 9000, true));
    assert_eq!(p.log, vec![Effect::Offer]);
    // Stage `provisioned` and an image file: deleted
    let mut p = with_state(Stage::Provisioned);
    p.fb_done = true;
    p.account = Account::Firstboot;
    p.state_file.provision = JobState::Done;
    p.state_file.factory = factory(9000, 9000, true);
    p.factory_exists = true;
    p.factory_file = vec![1; 9000];
    p.start(false, 0);
    assert!(!p.factory_exists && p.state_file.factory.is_none());
}

#[test]
fn rows_the_backups_mark_and_the_pending_checksums() {
    for stage in [Stage::Waiting, Stage::FirstbootDone] {
        // `backup-done`, no pending checksum file: the backup is taken again
        let mut p = with_state(stage);
        p.fb_done = stage == Stage::FirstbootDone;
        p.state_file.backup_done = true;
        p.start(false, 0);
        assert!(!p.state_file.backup_done, "{stage:?}");
        // A pending checksum file, no `backup-done`: deleted, so the modem
        // can never start from a backup the computer did not confirm
        let mut p = with_state(stage);
        p.fb_done = stage == Stage::FirstbootDone;
        p.sums = Some("x  efs.img\n".into());
        p.start(false, 0);
        assert_eq!(p.sums, None, "{stage:?}");
        assert!(p.log.contains(&Effect::SumsDelete));
        // Both: as recorded
        let mut p = with_state(stage);
        p.fb_done = stage == Stage::FirstbootDone;
        p.state_file.backup_done = true;
        p.sums = Some("x  efs.img\n".into());
        p.start(false, 0);
        assert!(p.state_file.backup_done && p.sums.is_some(), "{stage:?}");
    }
    // At `provisioned` provisioning has taken the pending file: the mark
    // stays (docs/LINK.md §6 says why this row stops there)
    let mut p = with_state(Stage::Provisioned);
    p.fb_done = true;
    p.state_file.backup_done = true;
    p.state_file.provision = JobState::Done;
    p.start(false, 0);
    assert!(p.state_file.backup_done);
}

#[test]
fn rows_provisioning_that_was_running() {
    let running = || {
        let mut p = with_state(Stage::FirstbootDone);
        p.fb_done = true;
        p.account = Account::Firstboot;
        p.state_file.backup_done = true;
        p.sums = Some(String::new());
        p.state_file.provision = JobState::Running;
        p.state_file.factory = factory(9000, 9000, true);
        p.factory_exists = true;
        p.factory_file = vec![1; 9000];
        p.work_dir = true;
        p
    };
    // Its unit is active (the daemon alone restarted): it goes on watching
    let mut p = running();
    p.provision_unit = true;
    p.start(false, 0);
    assert_eq!(p.log, vec![Effect::ProvisionWatch, Effect::Offer]);
    assert!(p.work_dir && p.factory_exists);
    assert_eq!(p.state_file.provision, JobState::Running);
    // ... and hears its end as if it had started it
    p.feed(
        0,
        PhoneInput::ProvisionEnded {
            result: Some(result(None)),
        },
    );
    assert_eq!(p.state_file.stage, Stage::Provisioned);
    // No unit, a result in which every required step is `ok`: provisioned,
    // the image and the working directory deleted
    for failed in [None, Some("camera")] {
        let mut p = running();
        p.provision_result = Some(result(failed));
        p.start(false, 0);
        assert_eq!(p.state_file.stage, Stage::Provisioned, "{failed:?}");
        assert_eq!(p.state_file.provision, JobState::Done);
        assert!(!p.factory_exists && !p.work_dir, "{failed:?}");
        assert_eq!(p.state_file.factory, None);
        assert_eq!(
            p.state_file.optional_failed,
            failed.map(String::from).into_iter().collect::<Vec<_>>(),
            "{failed:?}: kept for `status`"
        );
        // The order of §6.5: the stage is recorded before anything is deleted
        let at = |e: &Effect| p.log.iter().position(|x| x == e).unwrap();
        assert!(matches!(&p.log[0], Effect::Persist(s) if s.stage == Stage::Provisioned));
        assert!(at(&Effect::FactoryDelete) > 0 && at(&Effect::WorkDirRemove) > 0);
        assert_eq!(p.state_file.boot, 1, "the daemon alone restarted");
    }
    // No unit, any other result or none: failed, at the result's step or
    // at `interrupted`. The working directory is removed, the image kept.
    let mut skipped = result(None);
    skipped[3].state = StepState::Skipped;
    for (res, step) in [
        (None, "interrupted"),
        (Some(vec![]), "interrupted"),
        (Some(result(Some("modem"))), "modem"),
        (Some(skipped), "modem"),
    ] {
        let mut p = running();
        p.provision_result = res;
        p.start(false, 0);
        assert_eq!(p.state_file.stage, Stage::FirstbootDone, "{step}");
        assert_eq!(p.state_file.provision, JobState::Failed, "{step}");
        assert_eq!(p.state_file.provision_failed, step);
        assert!(p.factory_exists && !p.work_dir, "{step}");
        assert!(p.state_file.factory.as_ref().is_some_and(|f| f.verified));
    }
    // A working directory and no unit, with nothing recorded as running
    let mut p = with_state(Stage::FirstbootDone);
    p.fb_done = true;
    p.work_dir = true;
    p.start(false, 0);
    assert!(!p.work_dir);
    // A candidate or a temporary file of its own
    let mut p = with_state(Stage::Waiting);
    p.candidate = Some("USERNAME=owner\n".into());
    p.start(false, 0);
    assert_eq!(p.candidate, None);
    assert!(p.log.contains(&Effect::Sweep));
    // Power lost after the worker's result and before the stage was
    // recorded: the next boot begins at `provisioned`, so it is boot 2,
    // and the planned restart is not the link's to give any more
    let mut p = running();
    p.provision_result = Some(result(None));
    p.start(true, 0);
    assert_eq!(
        (p.state_file.stage, p.state_file.boot),
        (Stage::Provisioned, 2)
    );
}

#[test]
fn rows_squared_once_is_squared() {
    // Whatever a start finds, a second start right after it finds nothing
    // more to do: the rules reach a fixed point in one pass. 20000 phones
    // with files at random.
    let mut rng = TestRng(99);
    let mut pick = |n: u64| rng.next_u64() % n;
    let (mut offered, mut ended) = (0, 0);
    for _ in 0..20_000 {
        let mut p = files();
        p.state_exists = pick(8) != 0;
        p.state_unreadable = pick(12) == 0;
        p.done = (pick(15) == 0).then(|| "finished".to_string());
        p.marker = pick(10) != 0;
        let s = &mut p.state_file;
        s.stage = Stage::ALL[pick(6) as usize];
        s.token_hash = (pick(2) == 0).then_some([1; 32]);
        s.boot = 1 + pick(2) as u8;
        s.setup_boots = pick(5) as u8;
        s.boots_allowed = 3 + pick(2) as u8;
        s.backup_done = pick(2) == 0;
        s.provision = JobState::ALL[pick(4) as usize];
        let size = 1 + pick(10_000);
        s.factory = (pick(2) == 0).then(|| FactoryFile {
            size,
            sha256: [2; 32],
            synced: pick(size + 1),
            verified: pick(3) == 0,
        });
        p.conf = (pick(3) == 0).then(String::new);
        p.fb_done = pick(3) == 0;
        p.account = [Account::None, Account::Firstboot, Account::Other][pick(3) as usize];
        p.fb_unit = match pick(3) {
            0 => FirstbootUnit::NotRun,
            1 => FirstbootUnit::Running,
            _ => FirstbootUnit::Failed(vec!["a step".into()]),
        };
        p.boot_update_conf = pick(2) == 0;
        p.factory_exists = pick(2) == 0;
        p.factory_file = vec![
            0;
            if p.factory_exists {
                pick(10_001) as usize
            } else {
                0
            }
        ];
        p.sums = (pick(2) == 0).then(String::new);
        p.provision_unit = pick(4) == 0;
        p.provision_result = match pick(4) {
            0 => None,
            1 => Some(result(None)),
            2 => Some(result(Some("camera"))),
            _ => Some(result(Some("gnss"))),
        };
        p.work_dir = pick(2) == 0;
        p.candidate = (pick(4) == 0).then(String::new);
        let new_boot = pick(2) == 0;
        if pick(2) == 0 {
            p.state_file.boot_id = "another boot".into();
        }
        let before = p.found();
        // The rules are a function of what was found, and of nothing else
        assert_eq!(
            reconcile(&before, &optional()),
            reconcile(&before, &optional())
        );
        p.start(new_boot, 0);
        if p.phone.stopped() {
            ended += 1;
            assert!(!p.offered, "ended: no gadget change\n{before:#?}");
            assert!(!p.marker, "ended: no marker\n{before:#?}");
            assert!(
                p.done.is_some() || before.done,
                "ended with a reason\n{before:#?}"
            );
            // An end is an end: the next start does nothing
            p.log.clear();
            p.start(true, 0);
            assert!(p.phone.stopped() && p.log.is_empty(), "{before:#?}");
            continue;
        }
        offered += 1;
        assert!(p.state_exists && !p.state_unreadable);
        let state = p.state_file.clone();
        assert_ne!(state.stage, Stage::Finished);
        // What is recorded is what is there
        if let Some(f) = &state.factory {
            assert_eq!(p.factory_file.len() as u64, f.synced, "{before:#?}");
            assert!(!f.verified || f.synced == f.size, "{before:#?}");
        } else {
            assert!(!p.factory_exists, "{before:#?}");
        }
        if state.stage.before_provisioned() {
            assert_eq!(state.backup_done, p.sums.is_some(), "{before:#?}");
        }
        assert!(
            p.work_dir <= p.provision_unit,
            "no working directory without its unit\n{before:#?}"
        );
        assert_eq!(p.candidate, None);
        // The daemon is started again at once: nothing to square
        p.log.clear();
        p.start(false, 0);
        assert_eq!(p.state_file, state, "{before:#?}");
        let again: Vec<&Effect> = p
            .log
            .iter()
            .filter(|e| {
                !matches!(
                    e,
                    Effect::Screen(_) | Effect::Offer | Effect::ProvisionWatch
                )
            })
            .collect();
        assert!(again.is_empty(), "{again:?}\n{before:#?}");
    }
    assert!(offered > 5000 && ended > 2000, "{offered} {ended}");
}

// --- budget --------------------------------------------------------------------

/// A phone at `firstboot-done` in the boot the answers were taken in, the
/// host gone, the owner's session up at `now`.
fn done_and_alone(now: u64) -> SimPhone {
    let (mut p, mut c) = paired_phone();
    assert_eq!(
        c.one(&mut p, HostMsg::Answers(answers().get().clone())),
        "answers-ok"
    );
    p.feed(now, PhoneInput::FirstbootEnded { failed: vec![] });
    p.feed(now, PhoneInput::LinkDown);
    session_up(&mut p, now);
    p
}

fn paired_phone() -> (SimPhone, Client) {
    let mut p = new_phone(1);
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    c.hello(&mut p);
    c.pair(&mut p);
    (p, c)
}

#[test]
fn budget_ten_unpaired_minutes_close_the_link_for_the_boot() {
    let mut p = done_and_alone(1000);
    assert_eq!(p.phone.deadline(), Some(1000 + UNPAIRED_MS));
    p.feed(1000 + UNPAIRED_MS - 1, PhoneInput::Tick);
    assert!(!p.closed && !p.phone.stopped());
    p.feed(1000 + UNPAIRED_MS, PhoneInput::Tick);
    assert!(p.closed && p.phone.stopped());
    // The gadget is put back, the notice says how many restarts are left,
    // and nothing else changed: the stage, the token, the marker
    assert!(!p.offered);
    assert_eq!(
        p.agent.last(),
        Some(&AgentLine::Closed { boots_left: 3 }),
        "the boot the answers were taken in is not a setup boot"
    );
    assert_eq!(p.state_file.stage, Stage::FirstbootDone);
    assert!(p.state_file.token_hash.is_some() && p.marker && p.done.is_none());
    // Closed is closed, for this boot
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    assert!(c.raw(&mut p, b"anything").is_empty());
    assert_eq!(p.phone.deadline(), None);
}

#[test]
fn budget_the_ten_minutes_run_from_the_latest_of_three_and_never_under_a_job() {
    // Not before the owner's session is up (docs/LINK.md §6): nobody could
    // be told, and the first-start screen has nowhere to say it
    let (mut p, mut c) = paired_phone();
    c.one(&mut p, HostMsg::Answers(answers().get().clone()));
    p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
    p.feed(0, PhoneInput::LinkDown);
    p.feed(86_400_000, PhoneInput::Tick);
    assert!(!p.closed, "no session, no clock");
    // From the session being up
    p.feed(86_400_000, PhoneInput::SessionUp);
    assert_eq!(p.phone.deadline(), Some(86_400_000 + UNPAIRED_MS));

    // From the end of the last paired session: while a host is paired the
    // link stays open, however long
    let (mut p, mut c) = paired_phone();
    c.one(&mut p, HostMsg::Answers(answers().get().clone()));
    p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
    session_up(&mut p, 0);
    for minute in 1..=30u64 {
        c.now = minute * 60_000;
        p.feed(c.now, PhoneInput::Tick);
        assert_eq!(c.one(&mut p, HostMsg::Ping), "pong", "{minute}");
    }
    assert!(!p.closed);
    assert_eq!(c.one(&mut p, HostMsg::Bye), "bye");
    let end = c.now;
    assert_eq!(p.phone.deadline(), Some(end + UNPAIRED_MS));
    // `status` says so, unpaired too
    c.hello(&mut p);
    c.now = end + 1_000;
    let got = c.send(&mut p, HostMsg::Status);
    let [Got::Msg(PhoneFrame {
        msg: PhoneMsg::Status(s),
        ..
    })] = &got[..]
    else {
        panic!("{got:?}")
    };
    assert_eq!((s.link_boots_left, s.link_closes_s), (3, 599));
    // A host that says hello and never pairs does not keep it open
    for i in 1..=200u64 {
        c.now = end + i * 5_000;
        p.feed(c.now, PhoneInput::Tick);
        if p.closed {
            break;
        }
        c.hello(&mut p);
    }
    assert!(p.closed);
    assert_eq!(
        c.now,
        end + UNPAIRED_MS,
        "at ten minutes, not a hello later"
    );

    // Never while first-boot setup or provisioning runs: those need no
    // host, and may take longer than ten minutes
    let (mut p, mut c) = paired_phone();
    c.one(&mut p, HostMsg::Answers(answers().get().clone()));
    p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
    session_up(&mut p, 0);
    backup_all(&mut p, &mut c);
    assert_eq!(c.one(&mut p, HostMsg::BackupCommit), "backup-done");
    send_image(&mut p, &mut c);
    assert_eq!(c.one(&mut p, HostMsg::FactoryEnd), "factory-verified");
    assert_eq!(c.one(&mut p, HostMsg::Provision), "provision-started");
    p.feed(60_000, PhoneInput::LinkDown);
    assert_eq!(p.phone.deadline(), None, "provisioning runs: no clock");
    p.feed(7_200_000, PhoneInput::Tick);
    assert!(!p.closed);
    // From the end of the job
    p.feed(
        7_200_000,
        PhoneInput::ProvisionEnded {
            result: Some(result(None)),
        },
    );
    assert_eq!(p.phone.deadline(), Some(7_200_000 + UNPAIRED_MS));
    p.feed(7_200_000 + UNPAIRED_MS, PhoneInput::Tick);
    assert!(p.closed, "the boot provisioning ended in has the rule too");

    // Never while a step owes an answer: the link is not closed under an
    // installer that just asked for something
    let (mut p, mut c) = paired_phone();
    c.one(&mut p, HostMsg::Answers(answers().get().clone()));
    p.feed(0, PhoneInput::FirstbootEnded { failed: vec![] });
    session_up(&mut p, 0);
    p.hold = true;
    assert_eq!(c.one(&mut p, HostMsg::BackupList), "nothing");
    p.feed(0, PhoneInput::LinkDown);
    p.feed(3_600_000, PhoneInput::Tick);
    assert!(!p.closed, "a step owes an answer");
    p.hold = false;
    p.release(3_600_000);
    assert_eq!(p.phone.deadline(), Some(3_600_000 + UNPAIRED_MS));

    // No limit of any kind while no account exists
    let mut p = new_phone(1);
    p.feed(0, PhoneInput::SessionUp);
    p.feed(1 << 40, PhoneInput::Tick);
    assert!(!p.closed && p.phone.deadline().is_none());
}

#[test]
fn budget_three_setup_boots_then_the_end() {
    // Stopped before `provisioned`: `expired`
    let mut p = done_and_alone(0);
    p.state_file.backup_done = true; // (say the backup was committed)
    p.sums = Some("x  efs.img\n".into());
    p.state_file.factory = factory(9000, 4096, false);
    p.factory_exists = true;
    p.factory_file = vec![1; 4096];
    for boot in 1..=3u8 {
        p.start(true, 0);
        assert!(p.offered, "setup boot {boot}");
        assert_eq!(p.state_file.setup_boots, boot);
        assert_eq!(p.state_file.boots_left(), 3 - boot);
        // The daemon alone started again: not another boot
        p.start(false, 0);
        assert_eq!(p.state_file.setup_boots, boot);
        // Nobody comes: closed after ten minutes, with the count
        session_up(&mut p, 0);
        p.feed(UNPAIRED_MS, PhoneInput::Tick);
        assert!(p.closed);
        assert_eq!(
            p.agent.last(),
            Some(&AgentLine::Closed {
                boots_left: 3 - boot
            })
        );
    }
    // The fourth: ended before the gadget is touched
    p.log.clear();
    p.start(true, 0);
    assert!(p.phone.stopped() && !p.offered);
    assert!(!p.log.contains(&Effect::Offer));
    assert_eq!(p.done.as_deref(), Some("expired firmware cameras"));
    // What `expired` leaves: no marker, no token, no image, no working
    // directory; the stage, and the pending checksums
    assert!(!p.marker && !p.factory_exists && !p.work_dir);
    assert!(p.state_exists);
    assert_eq!(p.state_file.stage, Stage::FirstbootDone);
    assert_eq!(p.state_file.token_hash, None);
    assert_eq!(p.state_file.factory, None);
    assert!(p.state_file.backup_done && p.sums.is_some());
    // The order: the state file without its token first
    assert!(
        matches!(&p.log[..], [Effect::Persist(s), Effect::Expire { missing }]
            if s.token_hash.is_none() && missing == &[Missing::Firmware, Missing::Cameras]),
        "{:?}",
        p.log
    );
    // ... and from then on, nothing
    p.log.clear();
    p.start(true, 0);
    assert!(p.phone.stopped() && p.log.is_empty());

    // With no backup, that is missing too
    let mut p = files();
    p.state_exists = true;
    p.state_file = Persistent {
        stage: Stage::FirstbootFailed,
        setup_boots: 3,
        ..Persistent::default()
    };
    p.conf = Some(String::new());
    p.account = Account::Firstboot;
    p.start(true, 0);
    assert_eq!(p.done.as_deref(), Some("expired backup firmware cameras"));

    // At `provisioned` it ends as `finished`: the smoke report is a
    // report, not a requirement
    let mut p = with_state(Stage::Provisioned);
    p.fb_done = true;
    p.account = Account::Firstboot;
    p.state_file.provision = JobState::Done;
    p.state_file.token_hash = Some([1; 32]);
    for boot in 1..=3u8 {
        p.start(true, 0);
        assert!(p.offered);
        assert_eq!((p.state_file.setup_boots, p.state_file.boot), (boot, 2));
    }
    p.start(true, 0);
    assert!(p.phone.stopped() && !p.offered && p.finished);
    assert_eq!(p.done.as_deref(), Some("finished"));
    assert!(!p.marker && !p.state_exists);

    // The boot in which the answers are taken is not a setup boot, and
    // boots without an account are not counted at all
    let mut p = new_phone(1);
    for _ in 0..20 {
        p.start(true, 0);
        assert!(p.offered);
        assert_eq!(p.state_file.setup_boots, 0);
    }
}

#[test]
fn budget_reopen_gives_one_more_setup_boot() {
    let mut p = done_and_alone(0);
    p.state_file.setup_boots = 3;
    p.start(true, 0);
    assert_eq!(p.done.as_deref(), Some("expired backup firmware cameras"));
    // `sudo omamobile-install-link reopen`: root on the phone itself. It
    // changes nothing until the next boot.
    let reopened = reopen(&p.state_file).expect("after `expired`");
    assert_eq!(
        (reopened.setup_boots, reopened.boots_allowed),
        (3, 4),
        "one more"
    );
    p.state_file = reopened;
    p.done = None;
    p.marker = true;
    // The next boot offers the link, under the same ten-minute rule
    p.start(true, 0);
    assert!(p.offered);
    assert_eq!(p.state_file.boots_left(), 0);
    // There is no token: only a code from the unlocked session pairs
    p.feed(0, PhoneInput::LinkUp);
    let mut c = Client::new();
    let h = c.hello(&mut p);
    assert_eq!(
        (h.pair, h.wait_s),
        (PairMode::Locked, 0),
        "no code, no token"
    );
    session_up(&mut p, 0);
    assert_eq!(c.hello(&mut p).pair, PairMode::Ask);
    tap(&mut p, 0);
    c.hello(&mut p);
    c.pair(&mut p);
    assert_eq!(c.one(&mut p, HostMsg::BackupList), "backup-parts");
    // Unpaired again for ten minutes: closed, no restarts left
    assert_eq!(c.one(&mut p, HostMsg::Bye), "bye");
    p.feed(UNPAIRED_MS, PhoneInput::Tick);
    assert_eq!(p.agent.last(), Some(&AgentLine::Closed { boots_left: 0 }));
    // One boot, not two
    p.start(true, 0);
    assert!(p.phone.stopped() && !p.offered);
    assert!(p.done.as_deref().is_some_and(|d| d.starts_with("expired")));
    // ... and it can be granted again
    assert_eq!(reopen(&p.state_file).map(|s| s.boots_allowed), Some(5));

    // It is taken only after `expired`: not while boots are left, not on a
    // phone with no account, not after the end as `finished`
    for (stage, setup_boots) in [
        (Stage::FirstbootDone, 2),
        (Stage::Waiting, 3),
        (Stage::Provisioned, 3),
        (Stage::Finished, 3),
    ] {
        let s = Persistent {
            stage,
            setup_boots,
            ..Persistent::default()
        };
        assert_eq!(reopen(&s), None, "{stage:?} {setup_boots}");
    }
}

// --- kill ----------------------------------------------------------------------

/// A whole install, driven as an installer's flow and an owner would, with
/// a daemon that may die at any output. Returns the outputs the phone's
/// side carried out.
fn install(kill_at: Option<u64>) -> u64 {
    let who = || format!("killed at {kill_at:?}");
    let mut phone = SimPhone::blank(phone_config(), 1);
    phone.parts = parts();
    phone.die_after = kill_at;
    phone.start(true, 0);
    let mut host = SimHost::new(host_config());
    host.factory = image();
    let mut lb = Loopback::new(phone, host);
    let mut todo: Vec<Command> = vec![
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
    let mut restarts_seen = 0;
    let mut deaths = 0;
    for round in 0..2000 {
        // Never `done` early, and never anything but `finished`
        match lb.phone.done.as_deref() {
            None => assert!(lb.phone.marker || lb.phone.dead, "{}", who()),
            Some("finished") => {
                assert!(finish_asked, "{}: done before `finish`", who());
                assert!(!lb.phone.marker && !lb.phone.state_exists, "{}", who());
                assert_eq!(lb.phone.provision_starts.min(1), 1, "{}", who());
                assert_eq!(lb.phone.firstboot_starts, 1, "{}: answers once", who());
                assert_eq!(lb.phone.restarts, 1, "{}: one restart", who());
                assert!(deaths <= 1);
                return lb.phone.carried;
            }
            Some(other) => panic!("{}: done as {other}", who()),
        }
        assert!(round < 1999, "{}: stuck: {:?}", who(), lb.host.events);
        if lb.phone.dead {
            // The daemon died: its endpoint files are closed, the host
            // loses the link. systemd starts it again, in the same boot.
            deaths += 1;
            lb.cut();
            lb.phone.start(false, lb.now);
            continue;
        }
        if lb.phone.restarts > restarts_seen {
            // `systemctl reboot` was called: the planned restart
            restarts_seen = lb.phone.restarts;
            reboot(&mut lb);
            continue;
        }
        // The jobs that need no daemon go on without it, and end
        if lb.phone.fb_unit == FirstbootUnit::Running {
            lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] });
            continue;
        }
        if lb.phone.provision_unit {
            lb.phone_input(PhoneInput::ProvisionEnded {
                result: Some(result(None)),
            });
            continue;
        }
        if lb.phone.conf.is_some() && lb.phone.fb_unit == FirstbootUnit::NotRun {
            // (a daemon killed between placing the answers and starting
            // the unit: the unit starts by itself at the next boot; here
            // the daemon's own retry is what an owner would see first)
            lb.phone.fb_unit = FirstbootUnit::Running;
            continue;
        }
        if !lb.up {
            assert!(lb.phone.offered, "{}: no link offered", who());
            lb.connect();
            continue;
        }
        if !lb.host.host.ready() {
            match lb.host.events.last() {
                Some(Event::NeedCode) => {
                    let code = lb.phone.phone.code().expect("a code");
                    lb.command(Command::Pair(code));
                }
                Some(Event::NeedSession) => {
                    session_up(&mut lb.phone, lb.now);
                    lb.advance(5_000);
                }
                Some(Event::NeedTap) => {
                    tap(&mut lb.phone, lb.now);
                    lb.advance(5_000);
                }
                _ => lb.advance(5_000),
            }
            continue;
        }
        if lb.host.host.busy() {
            lb.advance(2_000);
            continue;
        }
        // The step before this one ended: with what?
        let events = lb.host.take_events();
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
            lb.advance(5_000);
            continue;
        };
        if cmd == Command::Smoke {
            session_up(&mut lb.phone, lb.now);
        }
        finish_asked |= cmd == Command::Finish;
        current = Some(cmd.clone());
        lb.command(cmd);
    }
    unreachable!()
}

#[test]
fn kill_the_daemon_after_every_single_output_of_a_whole_install() {
    let outputs = install(None);
    assert!(outputs > 150, "{outputs}");
    // After each: the daemon dies before the next thing on its list
    for k in 0..outputs {
        install(Some(k));
    }
}

#[test]
fn kill_between_placing_the_answers_and_recording_the_stage() {
    // The one F7 was found by, by hand. The answers are checked, the
    // candidate is copied to firstboot.conf and the unit started; the
    // daemon dies before the state file says `firstboot`.
    let (mut p, mut c) = paired_phone();
    p.hold = true;
    assert_eq!(
        c.one(&mut p, HostMsg::Answers(answers().get().clone())),
        "nothing"
    );
    p.hold = false;
    // (the check's answer: carried out up to and including the start of
    // the unit, and not one output further)
    let (ticket, outcome) = p.held.pop().unwrap();
    let out = p
        .phone
        .handle(0, PhoneInput::Done(ticket, outcome), &mut p.rng);
    assert!(
        matches!(
            &out[..],
            [PhoneOutput::Do(Effect::StartFirstboot { retry: false, .. })]
        ),
        "{out:?}"
    );
    p.boot_update_conf = true;
    p.conf = p.candidate.take();
    p.fb_unit = FirstbootUnit::Running;
    assert_eq!(p.state_file.stage, Stage::Waiting, "not yet recorded");
    // Power is lost. As the plan was first written, this phone's next boot
    // found a marker and a firstboot.conf and ended the link for good.
    p.start(true, 0);
    assert!(p.offered && p.done.is_none() && p.marker);
    assert_eq!(p.state_file.stage, Stage::Firstboot);
    // The installer comes back with its token, and learns it from `status`
    p.feed(0, PhoneInput::LinkUp);
    c.hello(&mut p);
    assert_eq!(c.resume(&mut p), "paired");
    let got = c.send(&mut p, HostMsg::Status);
    assert!(
        matches!(&got[..], [Got::Msg(PhoneFrame { msg: PhoneMsg::Status(s), .. })]
        if s.stage == Stage::Firstboot && s.firstboot.state == JobState::Running)
    );
    // It does not send the answers again; they would not be taken
    assert_eq!(
        c.one(&mut p, HostMsg::Answers(answers().get().clone())),
        "error:not-allowed"
    );
    // That boot is a setup boot: the account is being made in it
    assert_eq!(p.state_file.setup_boots, 1);
}
