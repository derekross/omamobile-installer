// SPDX-License-Identifier: MIT
//! Repair (docs/DESIGN.md §11, "Repair"; Omarchy Mobile's ACTIVE-SLOT-PLAN
//! §2): for a release that installs on the slot the phone runs from
//! (manifest format 2), the way back from a bad kernel. Against the fake
//! phone; no USB.
//!
//! Each test reads the pretend phone's record of what it was sent. What
//! holds: only the running slot's five boot images are written, each as
//! `<image>_<current-slot>`; never `userdata`, never a name of the other
//! slot, never `set_active` of any kind; the slot is read again before the
//! first flash and after the last; a phone whose slot changes gets nothing
//! more; no build index, no factory image, no answers; and a format-1
//! release, a flow that does not write, and a locked phone are refused with
//! nothing written.
mod support;
use omamobile_installer_core::bundle::ReleaseFiles;
use omamobile_installer_core::fastboot::{PhoneOptions, Session, WriteReach};
use omamobile_installer_core::flow::{
    Flow, FlowOptions, PhoneFact, Retry, Step, Stop, REPAIR_KEEPS, REPAIR_WAY_BACK,
};
use omamobile_installer_core::install::flash_userdata_from_reader;
use omamobile_installer_core::manifest::Slot;
use omamobile_installer_core::Error;
use pollster::block_on;
use support::bundle::{TestBundle, IMAGES};
use support::drive::{drive, drive_with};
use support::fake::{FakePhone, SENTINEL, SERIAL};

fn repair() -> FlowOptions {
    FlowOptions {
        write: true,
        repair: true,
        ..Default::default()
    }
}

fn stop(flow: &Flow) -> &Stop {
    match flow.step() {
        Step::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}"),
    }
}

/// The five boot partitions of `slot`, in the release's order.
fn boot_images(slot: &str) -> Vec<String> {
    IMAGES
        .iter()
        .map(|(n, _, _)| format!("{n}_{slot}"))
        .collect()
}

fn run(bundle: &TestBundle, phone: &FakePhone, options: FlowOptions) -> (Flow, Vec<String>) {
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    let mut states = Vec::new();
    drive_with(&mut flow, bundle, phone, "cheetah", "", |s| {
        states.push(s.to_string())
    });
    (flow, states)
}

/// What a Repair may ever send, whatever happened: questions, downloads,
/// a flash of one of `slot`'s five boot images, and `reboot`. Never
/// userdata, never the other slot, never a slot command, never the unlock
/// question. And userdata as the pretend phone was made.
fn only_the_slots_boot_images(phone: &FakePhone, slot: &str) {
    sent_only_the_slots_boot_images(phone, slot);
    userdata_untouched(phone);
}

/// [`only_the_slots_boot_images`], for a phone whose userdata held
/// something of its own before.
fn sent_only_the_slots_boot_images(phone: &FakePhone, slot: &str) {
    let ours = boot_images(slot);
    for c in phone.log() {
        assert!(!c.starts_with("set_active"), "{c}");
        assert!(!c.starts_with("reboot-"), "{c}");
        assert_ne!(c, "flash:userdata");
        assert!(!c.contains("userdata") || c.starts_with("getvar:"), "{c}");
        if let Some(p) = c.strip_prefix("flash:") {
            assert!(ours.iter().any(|a| a == p), "{c}");
        }
        assert!(
            c.starts_with("getvar:")
                || c.starts_with("download:")
                || c.starts_with("flash:")
                || c == "reboot",
            "{c}"
        );
    }
    // The other slot holds what it held
    let other = if slot == "a" { "b" } else { "a" };
    for p in boot_images(other) {
        assert!(phone.partition(&p).iter().all(|b| *b == SENTINEL), "{p}");
    }
}

/// userdata holds what the pretend phone was made with.
fn userdata_untouched(phone: &FakePhone) {
    assert!(phone.partition("userdata").iter().all(|b| *b == SENTINEL));
}

fn nothing_written(phone: &FakePhone) {
    assert_eq!(phone.changes(), Vec::<String>::new());
    assert!(phone.written().is_empty(), "{:?}", phone.written());
    assert!(!phone.0.borrow().rebooted);
}

// ---- What a Repair sends -----------------------------------------------------

#[test]
fn a_repair_writes_the_running_slots_five_boot_images_reads_back_and_restarts() {
    let bundle = TestBundle::running(&["a"]);
    // The phone as a format-2 install left it: on slot a, telling the
    // version of one bootloader copy only
    let phone = FakePhone::scenario("running-a").unwrap();
    // What userdata holds before: the owner's, which a Repair keeps
    phone.edit(|s| {
        let data = s.partitions.get_mut("userdata").unwrap();
        data[..12].copy_from_slice(b"owner's data");
    });
    let before = phone.partition("userdata");
    let (flow, states) = run(&bundle, &phone, repair());
    let Step::Repaired(plan) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    // The steps: no build, no factory image, no answers, no slot question
    assert_eq!(
        states,
        ["warning", "phone", "dry-run", "confirm", "write", "repaired"],
        "{states:?}"
    );
    assert_eq!(flow.screen().state, "repaired");
    assert_eq!(flow.screen().action, "none");
    assert!(plan.repair && plan.factory_zip.is_none() && plan.answers.is_none());
    assert_eq!(plan.running_slot, Some(Slot::A));
    let planned: Vec<&str> = plan.writes.iter().map(|w| w.partition.as_str()).collect();
    assert_eq!(planned, boot_images("a"));
    // The commands that change the phone: five flashes, then the restart
    let mut want: Vec<String> = boot_images("a")
        .iter()
        .map(|p| format!("flash:{p}"))
        .collect();
    want.push("reboot".into());
    assert_eq!(phone.changes(), want);
    sent_only_the_slots_boot_images(&phone, "a");
    // The slot is read again just before the first download, and after
    // the last flash, before the restart, with the flags logged
    let log = phone.log();
    let first = log.iter().position(|c| c.starts_with("download:")).unwrap();
    assert!(
        log[..first]
            .iter()
            .rev()
            .take(4)
            .any(|c| c == "getvar:current-slot"),
        "{:?}",
        &log[..first]
    );
    let reboot = log.iter().position(|c| c == "reboot").unwrap();
    let last_flash = log.iter().rposition(|c| c.starts_with("flash:")).unwrap();
    assert!(log[last_flash..reboot].contains(&"getvar:current-slot".to_string()));
    assert_eq!(log[reboot - 1], "getvar:all");
    // The partitions: slot a's boot images are the release's, userdata
    // and slot b are what they were
    for (name, size, _) in IMAGES {
        let part = phone.partition(&format!("{name}_a"));
        assert!(part[..size] == *bundle.image(name), "{name}_a");
    }
    assert_eq!(phone.partition("userdata"), before);
    let mut written = boot_images("a");
    written.push("userdata".into());
    written.sort();
    assert_eq!(
        phone.written(),
        written,
        "only the owner's own bytes in userdata"
    );
    assert_eq!(phone.active(), "a");
    assert!(phone.0.borrow().rebooted);
}

#[test]
fn a_repair_on_slot_b_writes_slot_b_only() {
    let bundle = TestBundle::running(&["a", "b"]);
    let phone = FakePhone::scenario("running-b").unwrap();
    let (flow, _) = run(&bundle, &phone, repair());
    assert!(
        matches!(flow.step(), Step::Repaired(_)),
        "{:?}",
        flow.step()
    );
    let mut want: Vec<String> = boot_images("b")
        .iter()
        .map(|p| format!("flash:{p}"))
        .collect();
    want.push("reboot".into());
    assert_eq!(phone.changes(), want);
    only_the_slots_boot_images(&phone, "b");
    let mut written = boot_images("b");
    written.sort();
    assert_eq!(phone.written(), written);
    assert_eq!(phone.active(), "b");
}

#[test]
fn a_repair_needs_no_build_index_no_factory_image_and_no_answers() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    // The index is not even handed in: a Repair reads none
    let mut flow = Flow::begin(&bundle.files(), None, &bundle.trusted(), repair());
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    assert_eq!(flow.screen().state, "confirm");
    // Nothing of an install's steps is taken
    assert!(flow
        .answers(&support::drive::answers(), "testsalt0123")
        .is_err());
    assert!(flow.factory_zip("x.zip", 1).is_err());
    assert!(flow.accept_google_terms(0).is_err());
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert!(
        matches!(flow.step(), Step::Repaired(_)),
        "{:?}",
        flow.step()
    );
    only_the_slots_boot_images(&phone, "a");
}

// ---- The session ---------------------------------------------------------------

#[test]
fn a_repair_session_holds_the_slots_boot_images_only_and_refuses_userdata_at_every_door() {
    let bundle = TestBundle::running(&["a", "b"]);
    let release = bundle.verify().unwrap();
    let phone = FakePhone::scenario("running-a").unwrap();
    // The dry run of a Repair: no list, and a plan without userdata
    let dry = block_on(Session::connect(
        phone.clone(),
        &release,
        SERIAL,
        PhoneOptions {
            repair: true,
            ..Default::default()
        },
    ))
    .unwrap();
    assert!(dry.repairs() && dry.is_dry_run() && dry.may_flash().is_empty());
    let plan = dry.plan();
    assert_eq!(plan.len(), 6, "{plan:?}");
    assert!(!plan
        .iter()
        .any(|l| l.contains("userdata") || l.contains("set_active")));
    assert_eq!(plan.last().unwrap(), "reboot");
    // The writing session, reached through the flow: the five, no userdata
    let (mut flow, mut session) = support::drive::writing_session(&bundle, &phone, repair());
    assert!(session.repairs() && !session.is_dry_run());
    assert_eq!(session.may_flash(), boot_images("a"));
    assert_eq!(session.running_slot(), Some(Slot::A));
    // userdata's size check is refused before the first write
    assert!(block_on(session.preflight_userdata(1)).is_err());
    for write in &release.manifest().boot_writes {
        let image = release
            .verify_boot_image(write, bundle.image(&write.image))
            .unwrap();
        block_on(session.flash_boot(&image)).unwrap();
    }
    // After the boot images, userdata is refused, and the session is over
    let mut check = release.root_image_check();
    check.update(&bundle.root_zst);
    let root = check.finish().unwrap();
    let sent = phone.log().len();
    let err = block_on(flash_userdata_from_reader(
        &mut session,
        &root,
        &bundle.root_zst[..],
        None,
    ))
    .unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("no userdata")),
        "{err}"
    );
    assert_eq!(phone.log().len(), sent, "{:?}", &phone.log()[sent..]);
    assert!(block_on(session.flash_userdata_segment(&[0; 28])).is_err());
    // Nothing more goes out of a session that refused: not the restart
    assert!(block_on(session.activate_and_reboot()).is_err());
    assert_eq!(phone.log().len(), sent);
    assert_eq!(
        session.reach(),
        WriteReach::BootImages {
            done: 5,
            of: 5,
            in_doubt: false
        }
    );
    flow.write_failed(session, &err).unwrap();
    assert_eq!(stop(&flow).id(), "write-interrupted");
    only_the_slots_boot_images(&phone, "a");
    assert!(!phone.0.borrow().rebooted);
}

#[test]
fn a_repair_session_is_only_for_a_release_on_the_running_slot_and_takes_no_key() {
    let phone = FakePhone::scenario("running-a").unwrap();
    // Format 1: refused before a question is asked
    let one = TestBundle::new().verify().unwrap();
    let err = block_on(Session::connect(
        phone.clone(),
        &one,
        SERIAL,
        PhoneOptions {
            repair: true,
            ..Default::default()
        },
    ))
    .unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("format 2")),
        "{err}"
    );
    assert!(phone.log().is_empty(), "{:?}", phone.log());
    // A Repair writes the images as signed: no SSH key goes in
    let two = TestBundle::running(&["a"]).verify().unwrap();
    let err = block_on(Session::connect(
        phone.clone(),
        &two,
        SERIAL,
        PhoneOptions {
            repair: true,
            owner_ssh_key: Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA test".into()),
            ..Default::default()
        },
    ))
    .unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("no SSH key")),
        "{err}"
    );
    assert!(phone.log().is_empty());
}

// ---- What a Repair refuses ---------------------------------------------------

#[test]
fn a_format_1_release_is_refused_for_repair_before_any_phone_is_asked() {
    let bundle = TestBundle::new();
    let phone = FakePhone::scenario("ready").unwrap();
    let (flow, states) = run(&bundle, &phone, repair());
    let s = stop(&flow);
    assert_eq!(s.id(), "repair-not-for-this-release");
    assert_eq!(s.retry, Retry::StopsAgain);
    assert_eq!(s.phone, PhoneFact::NotAsked);
    assert!(states.iter().all(|s| s == "stopped"), "{states:?}");
    assert!(phone.log().is_empty(), "{:?}", phone.log());
}

#[test]
fn a_repair_is_only_a_run_that_writes() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let (flow, _) = run(
        &bundle,
        &phone,
        FlowOptions {
            repair: true,
            ..Default::default()
        },
    );
    let s = stop(&flow);
    assert_eq!(s.id(), "repair-needs-write");
    assert_eq!(s.phone, PhoneFact::NotAsked);
    // Stopped before the release was read
    assert!(flow.release().is_none());
    assert!(phone.log().is_empty());
    // And a release that did not verify is no release for a Repair either
    let mut files: ReleaseFiles<'_> = bundle.files();
    let mut bad = bundle.manifest_json.clone();
    bad.push(b' ');
    files.manifest_json = &bad;
    let flow = Flow::begin(&files, bundle.index(), &bundle.trusted(), repair());
    assert_eq!(stop(&flow).id(), "release-not-verified");
}

#[test]
fn a_locked_phone_is_not_repaired_and_not_shown_the_unlock_steps() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
        s.unlock_ability = Some("get_unlock_ability: 1".into());
    });
    let (flow, states) = run(&bundle, &phone, repair());
    let s = stop(&flow);
    assert_eq!(s.id(), "repair-locked");
    assert!(!states.iter().any(|s| s == "unlock"), "{states:?}");
    // Not even the unlock question: unlocking erases what a Repair keeps
    assert_eq!(phone.unlock_questions(), 0);
    assert!(phone.not_reads().is_empty(), "{:?}", phone.not_reads());
    assert!(s
        .message
        .join(" ")
        .contains("Unlocking it erases the phone"));
}

#[test]
fn a_phone_on_a_slot_the_release_does_not_accept_is_not_repaired() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-b").unwrap();
    let (flow, _) = run(&bundle, &phone, repair());
    assert_eq!(stop(&flow).id(), "current-slot-not-accepted");
    assert!(phone.not_reads().is_empty());
}

// ---- The slot changes ----------------------------------------------------------

#[test]
fn a_repair_stops_whenever_the_current_slot_changes() {
    let bundle = TestBundle::running(&["a", "b"]);
    // How often a whole Repair reads current-slot, and where
    let clean = FakePhone::scenario("running-a").unwrap();
    let (flow, _) = run(&bundle, &clean, repair());
    assert!(matches!(flow.step(), Step::Repaired(_)));
    let log = clean.log();
    let reads: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, c)| *c == "getvar:current-slot")
        .map(|(i, _)| i)
        .collect();
    let first_download = log.iter().position(|c| c.starts_with("download:")).unwrap();
    let last_flash = log.iter().rposition(|c| c.starts_with("flash:")).unwrap();
    let before_writes = reads.iter().filter(|i| **i < first_download).count();
    let after_writes = reads.iter().filter(|i| **i > last_flash).count();
    assert_eq!(after_writes, 1, "one read-back after the last flash");
    assert_eq!(before_writes + after_writes, reads.len());
    // From the second read on (the first is the look that pins the slot),
    // every read before the first write that sees the other slot leaves
    // the phone as it was
    for nth in 2..=before_writes {
        let phone = FakePhone::scenario("running-a").unwrap();
        phone.edit(|s| s.slot_moves_at = Some((nth, "b".into())));
        let (flow, _) = run(&bundle, &phone, repair());
        let s = stop(&flow);
        assert!(
            [
                "current-slot-changed",
                "dry-run-refused",
                "write-refused",
                "write-interrupted"
            ]
            .contains(&s.id()),
            "{nth}: {}",
            s.id()
        );
        nothing_written(&phone);
        only_the_slots_boot_images(&phone, "a");
    }
    // At the read-back after the last boot image: the five are written,
    // and the phone is not restarted into a slot it was not checked on
    let phone = FakePhone::scenario("running-a").unwrap();
    phone.edit(|s| s.slot_moves_at = Some((reads.len(), "b".into())));
    let (flow, _) = run(&bundle, &phone, repair());
    let s = stop(&flow);
    assert_eq!(s.id(), "write-interrupted");
    let PhoneFact::RepairStopped { reach, slot } = s.phone else {
        panic!("{:?}", s.phone)
    };
    assert_eq!((reach, slot), (WriteReach::Written, Slot::A));
    let footer = s.footer().join(" ");
    assert!(
        footer.contains("Every boot image was written to slot a"),
        "{footer}"
    );
    assert!(footer.contains("userdata was not written"), "{footer}");
    let changes = phone.changes();
    assert!(!changes.contains(&"reboot".to_string()), "{changes:?}");
    assert_eq!(changes.len(), 5);
    only_the_slots_boot_images(&phone, "a");
    assert!(!phone.0.borrow().rebooted);
    // The first look's slot is the one the run keeps to: a phone that has
    // moved when the dry run looks is stopped there with the words for it
    let phone = FakePhone::scenario("running-a-moves").unwrap();
    let (flow, _) = run(&bundle, &phone, repair());
    assert_eq!(stop(&flow).id(), "current-slot-changed");
    nothing_written(&phone);
}

// ---- The words -------------------------------------------------------------------

#[test]
fn the_screens_of_a_repair_say_what_is_written_and_what_is_kept() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), repair());
    let warning = flow.screen();
    assert_eq!(warning.state, "warning");
    assert_eq!(warning.title, "Repair: read this once");
    let steps: Vec<&str> = warning.steps.iter().map(|s| s.text.as_str()).collect();
    assert!(steps.contains(&REPAIR_KEEPS), "{steps:?}");
    assert!(
        !steps.join(" ").contains("The phone is erased"),
        "{steps:?}"
    );
    assert!(steps.iter().any(|s| s.starts_with(REPAIR_WAY_BACK)));
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    let confirm = flow.screen();
    assert_eq!(confirm.title, "This is what the repair will write");
    let mut want: Vec<String> = IMAGES
        .iter()
        .map(|(n, size, _)| {
            format!(
                "{n}_a: images/{n}.img, {size} bytes (the partition holds {} bytes)",
                phone.partition(&format!("{n}_a")).len()
            )
        })
        .collect();
    want.push("then: restart the phone (reboot)".into());
    assert_eq!(confirm.plan, want);
    let all = [confirm.body.join(" "), confirm.footer.join(" ")].join(" ");
    for line in [
        "userdata is not written: your account, your files and everything else stored on the phone are kept.",
        "No slot is made active: the phone keeps starting from slot a.",
        "To go ahead, type the phone's codename: cheetah",
    ] {
        assert!(all.contains(line), "not said: {line}\n{all}");
    }
    for never in [
        "ERASED",
        "set_active",
        "factory image for build",
        "Your answers",
    ] {
        assert!(!all.contains(never), "said: {never}\n{all}");
    }
    // A wrong codename writes nothing
    let mut other = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), repair());
    let phone2 = FakePhone::scenario("running-a").unwrap();
    drive(&mut other, &bundle, &phone2, "panther", "");
    assert_eq!(stop(&other).id(), "not-confirmed");
    nothing_written(&phone2);
    // The last screen
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    let last = flow.screen();
    assert_eq!(last.state, "repaired");
    assert!(last.body.join(" ").contains("userdata was not written"));
    assert!(
        last.body[0].contains("boot_a, init_boot_a, vendor_boot_a, vendor_kernel_boot_a, dtbo_a")
    );
}

// ---- Interrupted ---------------------------------------------------------------

#[test]
fn an_interrupted_repair_says_where_it_stopped_and_a_rerun_writes_the_five_again() {
    let bundle = TestBundle::running(&["a"]);
    let base = {
        let phone = FakePhone::scenario("running-a").unwrap();
        let (_f, _s) = support::drive::writing_session(&bundle, &phone, repair());
        phone.log().len()
    };
    let mut seen = std::collections::BTreeSet::new();
    for extra in 0..60 {
        let phone = FakePhone::scenario("running-a").unwrap();
        phone.edit(|s| s.unplug_at = Some(base + extra));
        let (flow, _) = run(&bundle, &phone, repair());
        let Step::Stopped(s) = flow.step() else {
            assert!(matches!(flow.step(), Step::Repaired(_)));
            break;
        };
        assert_eq!(s.id(), "write-interrupted", "{extra}");
        assert!(s.title.starts_with("The repair stopped"), "{}", s.title);
        let PhoneFact::RepairStopped { reach, slot } = s.phone else {
            panic!("{:?}", s.phone)
        };
        assert_eq!(slot, Slot::A);
        let footer = s.footer().join(" ");
        assert!(
            !footer.contains("erased") && !footer.contains("ERASED"),
            "{footer}"
        );
        match reach {
            WriteReach::Nothing => assert!(footer.contains("No write reached the phone")),
            WriteReach::BootImages { .. } => assert!(
                footer.contains("slot a's boot images")
                    && footer.contains("userdata was not written")
            ),
            WriteReach::Written => {
                assert!(footer.contains("Every boot image was written to slot a"))
            }
            other => panic!("not a reach of a Repair: {other:?}"),
        }
        seen.insert(
            format!("{reach:?}")
                .split([' ', '{'])
                .next()
                .unwrap()
                .to_string(),
        );
        only_the_slots_boot_images(&phone, "a");
        // The cable back in, and the Repair again: the five, and the restart
        phone.replug();
        let (again, _) = run(&bundle, &phone, repair());
        assert!(
            matches!(again.step(), Step::Repaired(_)),
            "{extra}: {:?}",
            again.step()
        );
        for (name, size, _) in IMAGES {
            assert!(phone.partition(&format!("{name}_a"))[..size] == *bundle.image(name));
        }
        only_the_slots_boot_images(&phone, "a");
    }
    for want in ["Nothing", "BootImages", "Written"] {
        assert!(seen.contains(want), "{want}: {seen:?}");
    }
}

// ---- What stays as it is -------------------------------------------------------

#[test]
fn an_install_of_the_same_release_still_writes_userdata() {
    // The control: without repair, the same flow on the same phone writes
    // the five and userdata, so the tests above see a Repair's doing
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let (flow, _) = run(
        &bundle,
        &phone,
        FlowOptions {
            write: true,
            ..Default::default()
        },
    );
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    assert!(phone.changes().contains(&"flash:userdata".to_string()));
    assert!(!flow.repairs());
    // And the session `walk` drives is not a Repair's
    let (_f, session) = support::drive::writing_session(
        &bundle,
        &FakePhone::scenario("running-a").unwrap(),
        FlowOptions {
            write: true,
            ..Default::default()
        },
    );
    assert!(!session.repairs());
    assert!(session.may_flash().contains(&"userdata".to_string()));
}
