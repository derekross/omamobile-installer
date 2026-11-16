// SPDX-License-Identifier: MIT
//! A release that installs on the slot the phone runs from (manifest
//! format 2, Omarchy Mobile's ACTIVE-SLOT-PLAN), against the fake phone.
//! No USB.
//!
//! Each test reads the pretend phone's record of what it was sent. What
//! holds for such a release: the boot images go to `<image>_<current-slot>`,
//! the slot read at the first look and again before the first write and
//! after the last; never a name of the other slot; no `set_active` of any
//! kind, no slot probe, no question of what slot B holds and no
//! bootloader-slot check; `SLOT=` in `boot-update.conf`; and a phone on a
//! slot the release does not accept stops with only questions sent.
mod support;
use omamobile_installer_core::fastboot::{PhoneOptions, Session, WriteReach};
use omamobile_installer_core::flow::{
    Flow, FlowOptions, PhoneFact, Retry, Step, Stop, StopKind, ANDROID_REPLACED, GO_BACK_TO_ANDROID,
};
use omamobile_installer_core::manifest::{Manifest, Slot, SlotPlan};
use omamobile_installer_core::Error;
use pollster::block_on;
use serde_json::json;
use support::bundle::{TestBundle, IMAGES};
use support::drive::{drive, drive_with, id, walk};
use support::fake::{FakePhone, SENTINEL, SERIAL};

fn writing() -> FlowOptions {
    FlowOptions {
        write: true,
        ..Default::default()
    }
}

fn stop(flow: &Flow) -> &Stop {
    match flow.step() {
        Step::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}"),
    }
}

fn run(bundle: &TestBundle, phone: &FakePhone, options: FlowOptions) -> Flow {
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, bundle, phone, "cheetah", "");
    flow
}

/// The partitions of `slot` an install writes, and `userdata`.
fn ours(slot: &str) -> Vec<String> {
    let mut v: Vec<String> = IMAGES
        .iter()
        .map(|(n, _, _)| format!("{n}_{slot}"))
        .collect();
    v.push("userdata".into());
    v
}

fn nothing_written(phone: &FakePhone) {
    assert_eq!(phone.changes(), Vec::<String>::new());
    assert!(phone.written().is_empty(), "{:?}", phone.written());
    assert!(!phone.0.borrow().rebooted);
}

/// No `set_active`, no `reboot-bootloader`, and no flash of a name that is
/// not the written slot's or `userdata`, whatever happened.
fn no_slot_command_and_only(phone: &FakePhone, slot: &str) {
    let allowed = ours(slot);
    for c in phone.log() {
        assert!(!c.starts_with("set_active"), "{c}");
        assert!(!c.starts_with("reboot-"), "{c}");
        if let Some(p) = c.strip_prefix("flash:") {
            assert!(allowed.iter().any(|a| a == p), "{c}");
        }
        assert!(
            c.starts_with("getvar:")
                || c.starts_with("download:")
                || c.starts_with("flash:")
                || c == "reboot"
                || c == "flashing get_unlock_ability",
            "{c}"
        );
    }
}

// ---- The manifest ----------------------------------------------------------

fn manifest_of(bundle: &TestBundle) -> serde_json::Value {
    serde_json::from_slice(&bundle.manifest_json).unwrap()
}

fn refused(edit: impl FnOnce(&mut serde_json::Value), needle: &str) {
    let mut m = manifest_of(&TestBundle::running(&["a"]));
    edit(&mut m);
    match Manifest::parse(&serde_json::to_vec(&m).unwrap()) {
        Err(Error::Manifest(why)) => {
            assert!(why.contains(needle), "refused for another reason: {why}")
        }
        other => panic!("not refused ({needle}): {other:?}"),
    }
}

#[test]
fn a_format_2_manifest_is_read_with_its_images_and_accepted_slots() {
    let bundle = TestBundle::running(&["a"]);
    let release = bundle.verify().unwrap();
    let m = release.manifest();
    assert_eq!(m.format, 2);
    assert!(m.on_running_slot());
    assert_eq!(
        m.slot_plan,
        SlotPlan::Running {
            accept: vec![Slot::A]
        }
    );
    assert_eq!(m.target_slot(Slot::A), Some(Slot::A));
    assert_eq!(m.target_slot(Slot::B), None);
    let names: Vec<String> = m
        .boot_writes
        .iter()
        .map(|w| w.partition(Slot::A).name().to_string())
        .collect();
    assert_eq!(names, ours("a")[..5]);
    // The allowlist in the descriptor's slot form, compared with UFS_RW
    assert_eq!(m.allowlist, ours("b"));
    // Format 1 keeps its rules: slot B, whatever slot the phone runs from
    let one = TestBundle::new().verify().unwrap();
    assert_eq!(one.manifest().format, 1);
    assert_eq!(one.manifest().target_slot(Slot::A), Some(Slot::B));
    let both = TestBundle::running(&["a", "b"]).verify().unwrap();
    assert_eq!(both.manifest().target_slot(Slot::B), Some(Slot::B));
}

#[test]
fn every_format_2_rule_refuses_a_manifest_that_breaks_it() {
    refused(|m| m["format"] = json!(3), "format 3");
    refused(
        |m| m["install"]["require_matching_bootloader_slots"] = json!(true),
        "no require_matching_bootloader_slots",
    );
    refused(
        |m| m["install"]["require_matching_bootloader_slots"] = json!(false),
        "no require_matching_bootloader_slots",
    );
    refused(
        |m| m["install"]["after"] = json!(["set-active a", "reboot"]),
        "must end with reboot",
    );
    refused(
        |m| m["install"]["after"] = json!(["reboot", "reboot"]),
        "must end with reboot",
    );
    refused(
        |m| m["install"]["after"] = json!([]),
        "must end with reboot",
    );
    refused(|m| m["install"]["slot"] = json!("b"), "install.slot");
    refused(
        |m| {
            m["install"].as_object_mut().unwrap().remove("slot");
        },
        "install.slot",
    );
    refused(
        |m| {
            m["install"]
                .as_object_mut()
                .unwrap()
                .remove("accept_current_slots");
        },
        "accept_current_slots",
    );
    refused(
        |m| m["install"]["accept_current_slots"] = json!([]),
        "empty",
    );
    refused(
        |m| m["install"]["accept_current_slots"] = json!(["c"]),
        "not a slot",
    );
    refused(
        |m| m["install"]["accept_current_slots"] = json!(["_a"]),
        "not a slot",
    );
    refused(
        |m| m["install"]["accept_current_slots"] = json!(["a", "a"]),
        "twice",
    );
    refused(
        |m| m["install"]["bootloader_slot_probe"] = json!(true),
        "bootloader_slot_probe",
    );
    refused(
        |m| m["install"]["storage_guard"]["slot"] = json!("b"),
        "storage_guard.slot",
    );
    refused(
        |m| m["install"]["storage_guard"]["marker"] = json!("/etc/other"),
        "storage_guard.marker",
    );
    refused(
        |m| m["install"]["storage_guard"]["also"] = json!(["userdata", "persist"]),
        "storage_guard.also",
    );
    refused(
        |m| m["install"]["storage_guard"]["images"] = json!(["boot", "vbmeta"]),
        "not a boot image",
    );
    refused(
        |m| m["install"]["storage_guard"]["extra"] = json!(1),
        "not a release manifest",
    );
    refused(
        |m| {
            m["install"]
                .as_object_mut()
                .unwrap()
                .remove("storage_guard");
        },
        "no storage_guard",
    );
    refused(
        |m| m["install"]["storage_guard_allowlist"] = json!(["boot_b", "userdata"]),
        "storage_guard_allowlist",
    );
    // A boot image write names its image, never a partition: the slot is
    // the phone's, so neither slot's name may be fixed in the manifest
    for p in ["boot_a", "boot_b", "boot"] {
        refused(
            |m| m["install"]["writes"][0]["partition"] = json!(p),
            "names its image",
        );
    }
    refused(
        |m| m["install"]["writes"][0]["image"] = json!("vbmeta"),
        "not a boot image",
    );
    refused(
        |m| m["install"]["writes"][0]["image"] = json!("boot_a"),
        "not a boot image",
    );
    refused(
        |m| m["install"]["writes"][5]["image"] = json!("userdata"),
        "last write",
    );
    // The AVB footer rule holds for both formats
    refused(
        |m| m["install"]["writes"][0]["partition_size"] = json!(1),
        "exactly its partition's size",
    );
    // A format-1 manifest may not carry format 2's keys, nor name images
    let mut one = manifest_of(&TestBundle::new());
    one["install"]["accept_current_slots"] = json!(["a"]);
    assert!(Manifest::parse(&serde_json::to_vec(&one).unwrap()).is_err());
    let mut one = manifest_of(&TestBundle::new());
    one["install"]["writes"][0] = json!({"image": "boot", "partition_size": 16384, "size": 16384, "source": "images/boot.img"});
    assert!(Manifest::parse(&serde_json::to_vec(&one).unwrap()).is_err());
}

#[test]
fn the_descriptor_and_the_manifest_agree_on_the_slot() {
    // INSTALL_SLOT=active with a format-1 plan, format 2 without it, and
    // the accepted slots, each refused at verification
    let format1_active = TestBundle::build(
        |_| {},
        |c| c + "INSTALL_SLOT=active\nINSTALL_ACTIVE_SLOTS=a\n",
    );
    assert!(
        matches!(format1_active.verify(), Err(Error::DeviceConf(why)) if why.contains("format 1"))
    );
    let unset = TestBundle::running_with(
        &["a"],
        |_| {},
        |c| {
            c.replace("INSTALL_SLOT=active\n", "")
                .replace("INSTALL_ACTIVE_SLOTS=\"a\"\n", "")
        },
    );
    assert!(matches!(unset.verify(), Err(Error::DeviceConf(why)) if why.contains("format 2")));
    let other = TestBundle::running_with(
        &["a"],
        |_| {},
        |c| c.replace("INSTALL_ACTIVE_SLOTS=\"a\"", "INSTALL_ACTIVE_SLOTS=\"a b\""),
    );
    assert!(
        matches!(other.verify(), Err(Error::DeviceConf(why)) if why.contains("INSTALL_ACTIVE_SLOTS"))
    );
    for bad in ["INSTALL_SLOT=a", "INSTALL_SLOT=running", "INSTALL_SLOT="] {
        let b = TestBundle::running_with(&["a"], |_| {}, |c| c.replace("INSTALL_SLOT=active", bad));
        assert!(matches!(b.verify(), Err(Error::DeviceConf(_))), "{bad}");
    }
    for bad in ["\"c\"", "\"a a\"", "\"\"", "\"_a\""] {
        let b = TestBundle::running_with(&["a"], |_| {}, |c| c.replace("\"a\"", bad));
        assert!(matches!(b.verify(), Err(Error::DeviceConf(_))), "{bad}");
    }
    // A list nothing reads is refused too
    let stray = TestBundle::build(|_| {}, |c| c + "INSTALL_ACTIVE_SLOTS=a\n");
    assert!(matches!(stray.verify(), Err(Error::DeviceConf(_))));
    // INSTALL_SLOT=b is format 1, as absent
    TestBundle::build(|_| {}, |c| c + "INSTALL_SLOT=b\n")
        .verify()
        .unwrap();
}

// ---- A first install from stock Android on slot A -----------------------

#[test]
fn a_first_install_from_slot_a_writes_slot_a_and_sends_no_slot_command() {
    let bundle = TestBundle::running(&["a"]);
    // A Pixel 7 Pro as stock leaves it: on slot a, telling the version of
    // one bootloader copy only, in a front end that could ask about slot B
    // and measure the other slot. None of it is asked or measured
    let phone = FakePhone::scenario("running-a").unwrap();
    let mut states = Vec::new();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions {
            write: true,
            asks_what_slot_b_holds: true,
            can_probe_slots: true,
            ..Default::default()
        },
    );
    drive_with(&mut flow, &bundle, &phone, "cheetah", "", |s| {
        states.push(s.to_string())
    });
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    for never in [
        "slot-b",
        "slot-probe",
        "slot-probe-put-back",
        "slot-probe-running",
    ] {
        assert!(!states.iter().any(|s| s == never), "{never}: {states:?}");
    }
    no_slot_command_and_only(&phone, "a");
    // The writes, in order, then the read-back and the restart
    let changes = phone.changes();
    let mut want: Vec<String> = ours("a")[..5]
        .iter()
        .map(|p| format!("flash:{p}"))
        .collect();
    let userdata = changes.iter().filter(|c| *c == "flash:userdata").count();
    assert!(userdata >= 1);
    want.extend(std::iter::repeat_n("flash:userdata".to_string(), userdata));
    want.push("reboot".into());
    assert_eq!(changes, want);
    // The slot is read again before the first write and after the last
    let log = phone.log();
    let first_flash = log.iter().position(|c| c.starts_with("download:")).unwrap();
    assert!(
        log[..first_flash]
            .iter()
            .rev()
            .take(4)
            .any(|c| c == "getvar:current-slot"),
        "{:?}",
        &log[..first_flash]
    );
    let reboot = log.iter().position(|c| c == "reboot").unwrap();
    assert!(log[reboot - 3..reboot].contains(&"getvar:current-slot".to_string()));
    assert_eq!(log[reboot - 1], "getvar:all");
    // The phone: slot a's images and userdata, slot b and the rest untouched
    let mut written = ours("a");
    written.sort();
    assert_eq!(phone.written(), written);
    for (name, size, _) in IMAGES {
        let part = phone.partition(&format!("{name}_a"));
        assert!(part[..size] == *bundle.image(name), "{name}_a");
        assert!(phone
            .partition(&format!("{name}_b"))
            .iter()
            .all(|b| *b == SENTINEL));
    }
    let userdata = phone.partition("userdata");
    assert!(
        userdata[..bundle.root_raw.len()] == bundle.root_as_installed_on(None, Some(Slot::A))[..]
    );
    assert_eq!(phone.active(), "a");
    assert!(phone.0.borrow().rebooted);
}

#[test]
fn boot_update_conf_names_the_slot_that_was_written() {
    let bundle = TestBundle::running(&["a", "b"]);
    for slot in [Slot::A, Slot::B] {
        let root = bundle.root_as_installed_on(None, Some(slot));
        let release = bundle.verify().unwrap();
        let p = release
            .manifest()
            .root_image
            .placeholder("/etc/omamobile/boot-update.conf")
            .unwrap()
            .offset as usize;
        let text = String::from_utf8_lossy(&root[p..p + 4096]).to_string();
        // SLOT= on its own line, before AUTO_WRITE=, which ends the text
        // ahead of the padding
        assert!(
            text.contains(&format!("\nSLOT={slot}\nAUTO_WRITE=yes\n")),
            "{text}"
        );
    }
    // Format 1 writes no SLOT= line: absent means b, and an older hook
    // refuses the line
    let one = TestBundle::new();
    let release = one.verify().unwrap();
    let root = one.root_as_installed();
    let p = release
        .manifest()
        .root_image
        .placeholder("/etc/omamobile/boot-update.conf")
        .unwrap()
        .offset as usize;
    assert!(!String::from_utf8_lossy(&root[p..p + 4096]).contains("SLOT="));
    // And the two can't be mixed up
    let a = omamobile_installer_core::firstboot::root_answers(
        &release,
        &support::drive::answers(),
        None,
        support::drive::SALT,
    );
    assert!(a.is_ok());
    let running = bundle.verify().unwrap();
    assert!(omamobile_installer_core::firstboot::root_answers(
        &running,
        &support::drive::answers(),
        None,
        support::drive::SALT
    )
    .is_err());
    assert!(omamobile_installer_core::firstboot::root_answers_on(
        &release,
        &support::drive::answers(),
        None,
        support::drive::SALT,
        Some(Slot::A)
    )
    .is_err());
}

#[test]
fn the_confirmation_says_android_is_replaced_and_the_way_back() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    let screen = flow.screen();
    assert_eq!(screen.state, "confirm");
    let plan: Vec<&str> = screen
        .plan
        .iter()
        .map(|l| l.split(':').next().unwrap())
        .collect();
    assert_eq!(
        plan[..6],
        [
            "boot_a",
            "init_boot_a",
            "vendor_boot_a",
            "vendor_kernel_boot_a",
            "dtbo_a",
            "userdata"
        ]
    );
    assert_eq!(
        screen.plan.last().unwrap(),
        "then: restart the phone (reboot)"
    );
    assert!(!screen.plan.iter().any(|l| l.contains("set_active")));
    assert!(screen
        .body
        .iter()
        .any(|l| l.contains("written to slot a, the slot the phone runs from")));
    assert!(screen
        .footer
        .iter()
        .any(|l| l.starts_with(ANDROID_REPLACED)));
    assert!(screen.footer.iter().any(|l| l == GO_BACK_TO_ANDROID));
    assert!(screen
        .footer
        .iter()
        .any(|l| l.contains("No slot is made active")));
    assert!(GO_BACK_TO_ANDROID.contains("Android Flash Tool"));
    assert!(GO_BACK_TO_ANDROID.contains("Don't lock the bootloader unless both"));
    // The warning says it before anything is asked of a phone
    let warning = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    )
    .screen();
    assert!(warning
        .steps
        .iter()
        .any(|s| s.text.contains(ANDROID_REPLACED) && s.text.contains(GO_BACK_TO_ANDROID)));
    // No notice: nothing rests on anyone's word
    assert_eq!(screen.notice, None);
    // The last screen
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    let done = flow.screen();
    assert_eq!(done.state, "installed");
    assert!(done.body[1].contains("starts Omarchy Mobile from slot a"));
    assert!(done.footer.iter().any(|l| l == GO_BACK_TO_ANDROID));
    assert!(!done
        .footer
        .iter()
        .any(|l| l.contains("Android is still on slot A")));
}

#[test]
fn a_phone_whose_other_slot_has_started_is_told_that_slot_is_left_alone() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    phone.edit(|s| {
        s.vars.insert("slot-successful:b".into(), "yes".into());
        s.vars.insert("slot-unbootable:b".into(), "no".into());
    });
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    let screen = flow.screen();
    assert!(
        screen
            .footer
            .iter()
            .any(|l| l.contains("slot b, reads as a slot that has started")),
        "{:?}",
        screen.footer
    );
    nothing_written(&phone);
}

// ---- The slot rule --------------------------------------------------------

#[test]
fn a_phone_on_a_slot_the_release_does_not_accept_stops_with_only_questions_sent() {
    let bundle = TestBundle::running(&["a"]);
    for options in [
        writing(),
        // Neither the old reinstall nor the question about slot B gets past it
        FlowOptions {
            write: true,
            reinstall: true,
            asks_what_slot_b_holds: true,
            ..Default::default()
        },
        FlowOptions::default(),
    ] {
        let phone = FakePhone::scenario("running-b").unwrap();
        let flow = run(&bundle, &phone, options);
        let stop = stop(&flow);
        assert_eq!(stop.id(), "current-slot-not-accepted");
        assert_eq!(stop.retry, Retry::StopsAgain);
        assert!(
            matches!(&stop.kind, StopKind::CurrentSlotNotAccepted { slot, accepted } if slot == "b" && accepted == &["a"])
        );
        let text = stop.message.join(" ");
        assert!(text.contains("not supported yet"), "{text}");
        // Never advice to switch the slot by hand
        assert!(!text.contains("--set-active"), "{text}");
        assert!(!text.contains("fastboot "), "{text}");
        nothing_written(&phone);
        assert!(phone.not_reads().is_empty(), "{:?}", phone.not_reads());
    }
    // With b accepted, the same phone is written on slot b
    let both = TestBundle::running(&["a", "b"]);
    let phone = FakePhone::scenario("running-b").unwrap();
    let flow = run(&both, &phone, writing());
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    no_slot_command_and_only(&phone, "b");
    let mut written = ours("b");
    written.sort();
    assert_eq!(phone.written(), written);
    assert_eq!(phone.active(), "b");
}

#[test]
fn a_phone_that_moves_to_its_other_slot_after_the_first_look_is_not_written() {
    // Between the first look and the dry run
    let bundle = TestBundle::running(&["a", "b"]);
    let phone = FakePhone::scenario("running-a-moves").unwrap();
    let flow = run(&bundle, &phone, writing());
    let s = stop(&flow);
    assert_eq!(s.id(), "current-slot-changed");
    assert!(
        matches!(&s.kind, StopKind::CurrentSlotChanged { was, now } if was == "a" && now == "b")
    );
    nothing_written(&phone);
    // At the write's own look
    for nth in 3..=7 {
        let phone = FakePhone::scenario("running-a").unwrap();
        phone.edit(|s| s.slot_moves_at = Some((nth, "b".into())));
        let flow = run(&bundle, &phone, writing());
        assert!(
            matches!(flow.step(), Step::Stopped(_)),
            "{nth}: {:?}",
            flow.step()
        );
        nothing_written(&phone);
        no_slot_command_and_only(&phone, "a");
    }
}

#[test]
fn the_last_gate_before_the_first_flash_reads_the_slot_again() {
    let bundle = TestBundle::running(&["a", "b"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let (mut flow, mut session) = support::drive::writing_session(&bundle, &phone, writing());
    assert_eq!(session.running_slot(), Some(Slot::A));
    assert_eq!(session.may_flash(), ours("a"));
    // The phone moves to slot b now, after the writing session's checks
    let asked = phone
        .log()
        .iter()
        .filter(|c| *c == "getvar:current-slot")
        .count();
    phone.edit(|s| s.slot_moves_at = Some((asked + 1, "b".into())));
    let release = flow.release().unwrap().clone();
    let err = block_on(walk(&mut session, &bundle, &release)).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("current slot changed from a to 'b'")),
        "{err}"
    );
    assert_eq!(session.reach(), WriteReach::Nothing);
    flow.write_failed(session, &err).unwrap();
    let s = stop(&flow);
    assert_eq!(s.id(), "write-interrupted");
    assert!(s
        .footer()
        .iter()
        .any(|l| l.contains("No write reached the phone")));
    nothing_written(&phone);
}

#[test]
fn a_session_on_the_running_slot_holds_only_that_slots_names_and_no_set_active() {
    let bundle = TestBundle::running(&["a", "b"]);
    let release = bundle.verify().unwrap();
    for slot in ["a", "b"] {
        let phone = FakePhone::scenario(if slot == "a" {
            "running-a"
        } else {
            "running-b"
        })
        .unwrap();
        // A dry run: no list at all
        let dry = block_on(Session::connect(
            phone.clone(),
            &release,
            SERIAL,
            PhoneOptions::default(),
        ))
        .unwrap();
        assert!(dry.may_flash().is_empty());
        assert_eq!(dry.facts().target_slot.name(), slot);
        // The writing session, reached through the flow
        let (_flow, session) = support::drive::writing_session(&bundle, &phone, writing());
        assert_eq!(session.may_flash(), ours(slot));
        let plan = session.plan();
        assert!(!plan.iter().any(|l| l.contains("set_active")));
        assert_eq!(plan.last().unwrap(), "reboot");
    }
    // A measurement of the other slot has no place in such a session
    let phone = FakePhone::scenario("running-a").unwrap();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "dry-run");
    assert!(flow
        .slot_probe(omamobile_installer_core::flow::ProbeAnswer::Measure)
        .is_err());
    assert!(flow
        .slot_probe(omamobile_installer_core::flow::ProbeAnswer::PutBack)
        .is_err());
}

#[test]
fn a_release_whose_initramfs_lacks_the_guard_marker_is_written_nowhere() {
    let bundle = TestBundle::running_without_marker(&["a"]);
    let release = bundle.verify().unwrap();
    let vkb = release
        .manifest()
        .boot_writes
        .iter()
        .find(|w| w.image == "vendor_kernel_boot")
        .unwrap();
    let err = release
        .verify_boot_image(vkb, bundle.image("vendor_kernel_boot"))
        .unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("/etc/omamobile-ufs-rw-slot")),
        "{err}"
    );
    // The flow stops at the dry run, before the confirmation
    let phone = FakePhone::scenario("running-a").unwrap();
    let flow = run(&bundle, &phone, writing());
    assert_eq!(stop(&flow).id(), "dry-run-refused");
    nothing_written(&phone);
    // A format-1 release needs no marker
    let one = TestBundle::new().verify().unwrap();
    let w = one
        .manifest()
        .boot_writes
        .iter()
        .find(|w| w.image == "vendor_kernel_boot")
        .unwrap();
    one.verify_boot_image(w, TestBundle::new().image("vendor_kernel_boot"))
        .unwrap();
}

#[test]
fn the_old_slot_options_change_nothing_for_a_release_on_the_running_slot() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let flow = run(
        &bundle,
        &phone,
        FlowOptions {
            write: true,
            reinstall: true,
            bootloader_slots_match: Some("fake-0.9".into()),
            can_probe_slots: true,
            ..Default::default()
        },
    );
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    no_slot_command_and_only(&phone, "a");
    // A descriptor that allows the probe gets none: format 2 switches no slot
    let probe = TestBundle::running_with(&["a"], |_| {}, |c| c + "BOOTLOADER_SLOT_PROBE=yes\n");
    let phone = FakePhone::scenario("running-a").unwrap();
    let flow = run(
        &probe,
        &phone,
        FlowOptions {
            write: true,
            can_probe_slots: true,
            ..Default::default()
        },
    );
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    no_slot_command_and_only(&phone, "a");
}

#[test]
fn a_dry_run_on_the_running_slot_lists_the_slots_writes_and_no_set_active() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let flow = run(&bundle, &phone, FlowOptions::default());
    let Step::Done(plan) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    assert_eq!(plan.running_slot, Some(Slot::A));
    assert!(
        plan.would.iter().any(|l| l.ends_with("flash:boot_a")),
        "{:?}",
        plan.would
    );
    assert!(
        !plan
            .would
            .iter()
            .any(|l| l.contains("set_active") || l.ends_with("_b")),
        "{:?}",
        plan.would
    );
    assert_eq!(plan.would.last().unwrap(), "reboot");
    nothing_written(&phone);
    assert!(phone.not_reads().is_empty());
}

#[test]
fn an_interrupted_write_on_the_running_slot_says_where_it_stopped() {
    let bundle = TestBundle::running(&["a"]);
    // The cable pulled before each write command in turn, from the first
    let base = {
        let phone = FakePhone::scenario("running-a").unwrap();
        let (_f, _s) = support::drive::writing_session(&bundle, &phone, writing());
        phone.log().len()
    };
    let mut seen = std::collections::BTreeSet::new();
    for extra in 0..60 {
        let phone = FakePhone::scenario("running-a").unwrap();
        phone.edit(|s| s.unplug_at = Some(base + extra));
        let flow = run(&bundle, &phone, writing());
        let Step::Stopped(s) = flow.step() else {
            // Past the last command: the install ran through
            assert!(matches!(flow.step(), Step::Installed(_)));
            break;
        };
        assert_eq!(s.id(), "write-interrupted", "{extra}");
        let PhoneFact::WriteStoppedRunning { reach, slot } = s.phone else {
            panic!("{:?}", s.phone)
        };
        assert_eq!(slot, Slot::A);
        let footer = s.footer().join(" ");
        assert!(
            !footer.contains("slot B") && !footer.contains("Slot A, where Android is"),
            "{footer}"
        );
        match reach {
            WriteReach::Nothing => assert!(footer.contains("No write reached the phone")),
            WriteReach::BootImages { .. } => assert!(
                footer.contains("slot a's boot images")
                    && footer.contains("Google's factory image")
            ),
            WriteReach::Userdata { .. } => assert!(
                footer.contains("writing userdata") && footer.contains("Google's factory image")
            ),
            WriteReach::Written => assert!(footer.contains("Every image was written to slot a")),
            other => panic!("not a reach of format 2: {other:?}"),
        }
        seen.insert(
            format!("{reach:?}")
                .split([' ', '{'])
                .next()
                .unwrap()
                .to_string(),
        );
        no_slot_command_and_only(&phone, "a");
    }
    for want in ["Nothing", "BootImages", "Userdata", "Written"] {
        assert!(seen.contains(want), "{want}: {seen:?}");
    }
}

#[test]
fn a_locked_phone_on_the_running_slot_is_shown_the_unlock_steps_once_it_passes() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
        s.unlock_ability = Some("get_unlock_ability: 1".into());
    });
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "unlock");
    let screen = flow.screen();
    assert_eq!(screen.state, "unlock");
    assert!(
        screen.body[1].contains("runs from slot a, the slot this release is written to"),
        "{:?}",
        screen.body
    );
    nothing_written(&phone);
    // A locked phone on a slot it doesn't accept stops before the unlock steps
    let phone = FakePhone::scenario("running-b").unwrap();
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
    });
    let flow = run(&bundle, &phone, writing());
    assert_eq!(stop(&flow).id(), "current-slot-not-accepted");
    assert_eq!(phone.unlock_questions(), 0);
    let _ = id();
}

#[test]
fn a_writing_session_on_another_slot_than_the_one_confirmed_writes_nothing() {
    // Both slots accepted, so the session's own check would pass slot b:
    // the flow's check that the session is on the slot the owner confirmed
    // is what stops it. The phone moves at the writing session's connect,
    // the seventh current-slot question of a run (the look, the dry run's
    // look, its session's connect, its gate and read-back, the write's
    // look, the write's session)
    // A release whose answers go into the image: the answers name slot a,
    // and the session on slot b refuses them first. A release with the
    // install link has no answers in its image: the flow's own check
    // stops it
    let link = TestBundle::running_with(
        &["a", "b"],
        |m| m["install_link"] = json!({"protocol": 1}),
        |c| c,
    );
    for (bundle, why) in [
        (
            TestBundle::running(&["a", "b"]),
            "the answers name another slot",
        ),
        (link, "not on the slot the owner confirmed"),
    ] {
        let phone = FakePhone::scenario("running-a").unwrap();
        phone.edit(|s| s.slot_moves_at = Some((7, "b".into())));
        let flow = run(&bundle, &phone, writing());
        let s = stop(&flow);
        assert_eq!(s.id(), "write-refused", "{s:?}");
        assert!(s.message.join(" ").contains(why), "{s:?}");
        nothing_written(&phone);
        no_slot_command_and_only(&phone, "a");
    }
}

#[test]
fn both_slots_flags_are_logged_before_and_after_the_writes() {
    let bundle = TestBundle::running(&["a"]);
    let phone = FakePhone::scenario("running-a").unwrap();
    let (_flow, session) = support::drive::writing_session(&bundle, &phone, writing());
    let before = session
        .log()
        .iter()
        .find(|l| l.starts_with("before the writes: "))
        .expect("the flags at the first look")
        .clone();
    for flag in [
        "slot-successful:a",
        "slot-successful:b",
        "slot-unbootable:b",
    ] {
        assert!(before.contains(flag), "{before}");
    }
}
