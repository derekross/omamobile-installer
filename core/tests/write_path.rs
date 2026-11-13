// SPDX-License-Identifier: MIT
//! The write path of the install flow, against the fake phone. No USB.
//!
//! The fake phone records every command it is sent, and each test reads
//! that record: what was written, in which order, and above all what was
//! not. A test that takes a condition of a write away must see no command
//! that changes a phone (`flash:`, `set_active:`, `reboot`, or anything
//! else that is not a question).
mod support;
use omamobile_installer_core::fastboot::{Session, WriteReach};
use omamobile_installer_core::flow::{
    Flow, FlowOptions, PhoneFact, Step, Stop, StopKind, NO_INSTALL_YET, NO_USABLE_PHONE_YET,
    SLOTS_ON_THE_OWNERS_WORD,
};
use omamobile_installer_core::install::flash_userdata_from_reader;
use omamobile_installer_core::Error;
use pollster::block_on;
use serde_json::json;
use support::bundle::{TestBundle, IMAGES};
use support::drive::{drive, drive_with, walk};
use support::fake::{FakePhone, SENTINEL, SERIAL};

/// What an install of the test release may write, in its order.
const OURS: [&str; 6] = [
    "boot_b",
    "init_boot_b",
    "vendor_boot_b",
    "vendor_kernel_boot_b",
    "dtbo_b",
    "userdata",
];

fn writing() -> FlowOptions {
    FlowOptions {
        write: true,
        ..Default::default()
    }
}
fn ready() -> FakePhone {
    FakePhone::scenario("ready").unwrap()
}
fn stop(flow: &Flow) -> &Stop {
    match flow.step() {
        Step::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}"),
    }
}

/// A whole run of the installer: the flow from its beginning to its end.
fn run(bundle: &TestBundle, phone: &FakePhone, options: FlowOptions) -> Flow {
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, bundle, phone, "cheetah", "");
    flow
}

/// A flow that writes, taken to `until` on this phone.
fn flow_at(bundle: &TestBundle, phone: &FakePhone, until: &str) -> Flow {
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    drive(&mut flow, bundle, phone, "cheetah", until);
    assert_eq!(flow.screen().state, until, "{:?}", flow.step());
    flow
}

/// No command that changes a phone was received, and nothing on it changed.
fn nothing_written(phone: &FakePhone) {
    assert_eq!(phone.changes(), Vec::<String>::new());
    assert!(phone.written().is_empty(), "{:?}", phone.written());
    assert!(!phone.0.borrow().rebooted);
}

/// Whatever happened, the record holds only commands an install may send,
/// and every flash names a partition of the release.
fn only_our_commands(phone: &FakePhone) {
    for c in phone.log() {
        match c.strip_prefix("flash:") {
            Some(partition) => assert!(OURS.contains(&partition), "{c}"),
            None => assert!(
                c.starts_with("getvar:")
                    || c.starts_with("download:")
                    || c == "set_active:b"
                    || c == "reboot",
                "{c}"
            ),
        }
    }
    for name in phone.written() {
        assert!(OURS.contains(&name.as_str()), "{name} was written");
    }
}

/// The phone holds the release: each image at the start of its slot B
/// partition, the root image on userdata, slot B active, restarted. And
/// nothing else on it changed.
fn holds_the_release(bundle: &TestBundle, phone: &FakePhone) {
    let mut ours = OURS.map(str::to_string).to_vec();
    ours.sort();
    assert_eq!(phone.written(), ours);
    for (name, size, _) in IMAGES {
        let part = phone.partition(&format!("{name}_b"));
        assert!(part[..size] == *bundle.image(name), "{name}_b");
        assert!(part[size..].iter().all(|b| *b == SENTINEL), "{name}_b");
    }
    let userdata = phone.partition("userdata");
    assert!(userdata[..bundle.root_raw.len()] == bundle.root_raw[..]);
    assert!(userdata[bundle.root_raw.len()..]
        .iter()
        .all(|b| *b == SENTINEL));
    let state = phone.0.borrow();
    assert_eq!(state.active_slot, "b");
    assert!(state.rebooted);
}

// ---- Rule 1, and the order (rules 4, 5 and 7) ---------------------------

#[test]
fn an_install_sends_these_commands_in_this_order_and_nothing_else() {
    let bundle = TestBundle::new();
    let phone = ready();
    let flow = run(&bundle, &phone, writing());
    let Step::Installed(plan) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    holds_the_release(&bundle, &phone);
    only_our_commands(&phone);

    // Every command but the questions, in the order the phone received
    // them: slot B's boot images in the descriptor's order, userdata in
    // pieces, slot B made active, the restart. As tools/install-phone.
    let pieces = plan.writes[5].pieces.unwrap() as usize;
    let sent = phone.not_reads();
    println!("{}", sent.join("\n"));
    let mut expected: Vec<String> = IMAGES
        .iter()
        .flat_map(|(name, size, _)| [format!("download:{size:08x}"), format!("flash:{name}_b")])
        .collect();
    assert_eq!(sent[..10], expected[..]);
    let userdata = &sent[10..sent.len() - 2];
    assert_eq!(userdata.len(), 2 * pieces);
    // 2 MiB through a 256 KiB max-download-size: several pieces
    assert!(pieces > 4);
    for pair in userdata.chunks(2) {
        let size = usize::from_str_radix(pair[0].strip_prefix("download:").unwrap(), 16).unwrap();
        assert!(size <= 256 << 10);
        assert_eq!(pair[1], "flash:userdata");
    }
    expected.extend(userdata.iter().cloned());
    expected.extend(["set_active:b".to_string(), "reboot".to_string()]);
    assert_eq!(sent, expected);

    // The slot: `set_active:b` once, after the last write, and no other
    // slot command of any kind
    let slot: Vec<&String> = sent.iter().filter(|c| c.contains("active")).collect();
    assert_eq!(slot, ["set_active:b"]);
    assert_eq!(sent.iter().filter(|c| *c == "reboot").count(), 1);

    // Slot A, the unsuffixed names and what an install never touches still
    // hold what they held
    for untouched in [
        "boot_a",
        "init_boot_a",
        "vendor_boot_a",
        "vendor_kernel_boot_a",
        "dtbo_a",
        "vbmeta_a",
        "vbmeta_b",
        "bootloader_a",
        "bootloader_b",
        "modem_a",
        "modem_b",
        "persist",
        "efs",
        "efs_backup",
        "devinfo",
        "misc",
    ] {
        assert!(
            phone.partition(untouched).iter().all(|b| *b == SENTINEL),
            "{untouched}"
        );
    }

    // After the last write: what the owner is told, and nothing more is sent
    let screen = flow.screen();
    assert_eq!(screen.state, "installed");
    assert_eq!(screen.action, "none");
    assert_eq!(
        screen.title,
        "Omarchy Mobile is written, and the phone is restarting"
    );
    assert_eq!(screen.body[0], "Every image was written, and the phone confirmed each one: boot_b, init_boot_b, vendor_boot_b, vendor_kernel_boot_b, dtbo_b, userdata.");
    assert!(screen.body[1].starts_with("Slot B is the active slot, and the phone was told to restart. It now starts Omarchy Mobile from slot B."));
    // The truth about the first start: the answers are not on the phone
    assert!(screen.body[2].contains("cannot put your answers on the phone"));
    assert!(screen.body[2].contains("no user account"));
    assert!(screen.footer[0].contains("Android is still on slot A"));
    println!("{screen:#?}");
}

#[test]
fn the_flash_list_is_the_releases_descriptor_and_nothing_else() {
    // A descriptor with three boot images: the two others are never
    // written, although the phone has those partitions and the files exist
    let bundle = TestBundle::build(
        |m| {
            let keep = ["boot_b", "vendor_boot_b", "dtbo_b", "userdata"];
            m["install"]["storage_guard_allowlist"] = json!(keep);
            m["install"]["writes"]
                .as_array_mut()
                .unwrap()
                .retain(|w| keep.contains(&w["partition"].as_str().unwrap()));
        },
        |c| {
            c.replace(
                "boot init_boot vendor_boot vendor_kernel_boot dtbo",
                "boot vendor_boot dtbo",
            )
            .replace(
                "boot_b init_boot_b vendor_boot_b vendor_kernel_boot_b dtbo_b userdata",
                "boot_b vendor_boot_b dtbo_b userdata",
            )
            .replace("boot:0x4000 init_boot:0x2000", "boot:0x4000")
        },
    );
    let phone = ready();
    let flow = run(&bundle, &phone, writing());
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    let flashed: Vec<String> = phone
        .log()
        .into_iter()
        .filter(|c| c.starts_with("flash:"))
        .collect();
    assert_eq!(
        flashed[..3],
        ["flash:boot_b", "flash:vendor_boot_b", "flash:dtbo_b"]
    );
    assert!(flashed[3..].iter().all(|c| c == "flash:userdata"));
    assert_eq!(
        phone.written(),
        ["boot_b", "dtbo_b", "userdata", "vendor_boot_b"]
    );

    // A release that names any other partition, signed by the trusted key
    // with valid checksums, is not a release: the flow stops before the
    // warning, and no phone is asked anything
    for forbidden in [
        "boot_a",
        "init_boot_a",
        "vendor_boot_a",
        "vendor_kernel_boot_a",
        "dtbo_a",
        "boot",
        "init_boot",
        "vendor_boot",
        "vendor_kernel_boot",
        "dtbo",
        "bootloader",
        "bootloader_a",
        "bootloader_b",
        "radio",
        "radio_b",
        "vbmeta",
        "vbmeta_a",
        "vbmeta_b",
        "devinfo",
        "persist",
        "efs",
        "efs_backup",
        "modem",
        "modem_a",
        "modem_b",
        "modem_userdata",
        "misc",
        "super",
        "omamobile_made_up",
        "made_up_b",
        "userdata_b",
    ] {
        // As a boot image's target, and as the last write in userdata's place
        for at in [0, 5] {
            let bundle = TestBundle::build(
                |m| m["install"]["writes"][at]["partition"] = json!(forbidden),
                |c| c,
            );
            let phone = ready();
            let flow = run(&bundle, &phone, writing());
            assert_eq!(
                stop(&flow).id(),
                "release-not-verified",
                "{forbidden} at {at}"
            );
            assert!(phone.log().is_empty(), "{forbidden}: {:?}", phone.log());
        }
        // On the allowlist, in the manifest and in the descriptor alike
        let bundle = TestBundle::build(
            |m| {
                m["install"]["storage_guard_allowlist"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(forbidden))
            },
            |c| c.replace("dtbo_b userdata", &format!("dtbo_b userdata {forbidden}")),
        );
        let phone = ready();
        let flow = run(&bundle, &phone, writing());
        assert_eq!(stop(&flow).id(), "release-not-verified", "{forbidden}");
        assert!(phone.log().is_empty());
    }
}

// ---- Rule 2: each condition of a write, taken away -----------------------

#[test]
fn nothing_is_written_for_a_release_or_an_index_that_did_not_verify() {
    let good = TestBundle::new();
    let cases: Vec<(&str, TestBundle, Option<Vec<u8>>)> = vec![
        // A manifest changed after signing
        (
            "release-not-verified",
            {
                let mut b = TestBundle::new();
                let at = b.manifest_json.len() / 2;
                b.manifest_json[at] ^= 1;
                b
            },
            None,
        ),
        // Signed by a key the installer does not trust
        (
            "release-not-verified",
            {
                let mut b = TestBundle::new();
                b.fingerprint = good.fingerprint.clone();
                b
            },
            None,
        ),
        // No build index, or one that is not the signed bytes
        (
            "no-build-index",
            TestBundle::with_index(|i| *i = json!(null)),
            None,
        ),
        (
            "build-index-refused",
            TestBundle::new(),
            Some(b"{}".to_vec()),
        ),
    ];
    for (want, bundle, index) in cases {
        let phone = ready();
        let mut flow = Flow::begin(
            &bundle.files(),
            index.as_deref().or(bundle.index()),
            &bundle.trusted(),
            writing(),
        );
        assert_eq!(stop(&flow).id(), want);
        // No step is taken any more: not the confirmation, not the write
        assert!(flow.confirm_install("cheetah").is_err());
        assert!(matches!(
            block_on(flow.begin_write(phone.clone())),
            Err(Error::Refused(_))
        ));
        drive(&mut flow, &bundle, &phone, "cheetah", "");
        assert_eq!(stop(&flow).id(), want);
        assert!(phone.log().is_empty(), "{want}: {:?}", phone.log());
        nothing_written(&phone);
    }
}

#[test]
fn nothing_is_written_to_a_phone_that_does_not_pass_every_stop() {
    let bundle = TestBundle::new();
    // A phone the flow stops for is never asked to confirm, and never written
    for (scenario, want) in [
        ("other-model", "wrong-model"),
        ("slot-b", "runs-from-slot-b"),
        ("one-slot", "bootloader-slots-unknown"),
        ("slots-differ", "bootloader-slots-differ"),
        ("unknown-build", "build-not-listed"),
        ("untested-build", "build-not-tested"),
        ("untested-old-build", "build-not-tested"),
    ] {
        let phone = FakePhone::scenario(scenario).unwrap();
        let mut flow = run(&bundle, &phone, writing());
        assert_eq!(stop(&flow).id(), want, "{scenario}");
        assert!(flow.confirm_install("cheetah").is_err());
        assert!(block_on(flow.begin_write(phone.clone())).is_err());
        nothing_written(&phone);
    }
    // The stops are judged again at the write itself: a phone that passed
    // them, was confirmed, and no longer passes gets nothing
    type Change = fn(&FakePhone);
    let changes: [(&str, Change); 5] = [
        ("bootloader-slots-differ", |p| {
            p.edit(|s| {
                s.vars
                    .insert("version-bootloader-b".into(), "fake-0.9".into());
            });
        }),
        ("bootloader-slots-unknown", |p| {
            p.edit(|s| {
                s.vars.remove("version-bootloader-a");
                s.vars.remove("version-bootloader-b");
            });
        }),
        ("runs-from-slot-b", |p| {
            p.edit(|s| {
                s.vars.insert("current-slot".into(), "b".into());
            });
        }),
        ("baseband-unknown", |p| {
            p.edit(|s| {
                s.vars.remove("version-baseband");
            });
        }),
        // The build the factory image was chosen for is no longer the phone's
        ("build-changed", |p| {
            p.edit(|s| {
                for key in [
                    "version-bootloader",
                    "version-bootloader-a",
                    "version-bootloader-b",
                ] {
                    s.vars.insert(key.into(), "fake-1.1".into());
                }
                s.vars
                    .insert("version-baseband".into(), "fake-radio-1.1".into());
            });
        }),
    ];
    for (want, change) in changes {
        let phone = ready();
        let mut flow = flow_at(&bundle, &phone, "write");
        change(&phone);
        drive(&mut flow, &bundle, &phone, "cheetah", "");
        assert_eq!(stop(&flow).id(), want);
        assert!(
            stop(&flow).footer().join(" ").contains("written nothing"),
            "{want}: {:?}",
            stop(&flow).footer()
        );
        nothing_written(&phone);
    }
}

#[test]
fn nothing_is_written_to_a_locked_phone() {
    let bundle = TestBundle::new();
    // Locked from the start: the unlock steps, and no way to a write
    let phone = FakePhone::scenario("locked").unwrap();
    let mut flow = run(&bundle, &phone, writing());
    assert_eq!(flow.screen().state, "unlock");
    assert!(flow.confirm_install("cheetah").is_err());
    assert!(block_on(flow.begin_write(phone.clone())).is_err());
    assert!(phone
        .not_reads()
        .iter()
        .all(|c| !c.starts_with("flash:") && !c.starts_with("download:")));
    assert!(phone.written().is_empty());

    // Unlocked through the confirmation, locked again at the write
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "write");
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
    });
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert_eq!(stop(&flow).id(), "locked-at-write");
    assert!(matches!(stop(&flow).kind, StopKind::LockedAtWrite));
    nothing_written(&phone);
}

#[test]
fn nothing_is_written_to_another_phone_than_the_pinned_one() {
    let bundle = TestBundle::new();
    // Another phone answers at the write: it gets nothing
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "write");
    let seen = phone.log().len();
    phone.edit(|s| s.swap_serial_after = Some(seen));
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert_eq!(stop(&flow).id(), "phone-changed");
    nothing_written(&phone);

    // Another phone answers partway through the writes: from the moment it
    // does, no command that changes a phone is sent. The serial number is
    // asked again before every write, so this holds at every command.
    let whole = ready();
    assert!(matches!(
        run(&bundle, &whole, writing()).step(),
        Step::Installed(_)
    ));
    let log = whole.log();
    let first = log.iter().position(|c| c.starts_with("download:")).unwrap();
    // Up to the last time the install asks: what follows that question is
    // `set_active:b` and `reboot`, on the phone that just answered it
    let last_asked = log.iter().rposition(|c| c == "getvar:serialno").unwrap();
    for swap_after in first - 1..=last_asked {
        let phone = ready();
        phone.edit(|s| s.swap_serial_after = Some(swap_after));
        let flow = run(&bundle, &phone, writing());
        let got = phone.log();
        // The first question about the serial number after the swap
        let noticed = got
            .iter()
            .enumerate()
            .position(|(n, c)| n >= swap_after && c == "getvar:serialno")
            .unwrap_or_else(|| panic!("swap after {swap_after}: never asked again: {got:?}"));
        assert!(
            got[noticed..].iter().all(|c| c.starts_with("getvar:")),
            "swap after {swap_after}: {:?}",
            &got[noticed..]
        );
        // At most the one write that was already under way when it changed
        let late: Vec<&String> = got[swap_after..noticed]
            .iter()
            .filter(|c| !c.starts_with("getvar:"))
            .collect();
        assert!(late.len() <= 2, "swap after {swap_after}: {late:?}");
        assert!(
            matches!(&stop(&flow).kind, StopKind::WriteInterrupted(Error::Refused(why)) if why.contains("the phone changed")),
            "swap after {swap_after}: {:?}",
            flow.step()
        );
        assert!(!phone.0.borrow().rebooted);
        only_our_commands(&phone);
    }
}

#[test]
fn nothing_is_written_without_the_typed_codename() {
    let bundle = TestBundle::new();
    // Anything but the codename, exactly, stops the install
    for typed in [
        "",
        "yes",
        "y",
        "Cheetah",
        "CHEETAH",
        "cheetah ",
        " cheetah",
        "cheeta",
        "cheetahh",
        "panther",
        "Pixel 7 Pro",
        "cheetah\n",
    ] {
        let phone = ready();
        let mut flow = Flow::begin(
            &bundle.files(),
            bundle.index(),
            &bundle.trusted(),
            writing(),
        );
        drive(&mut flow, &bundle, &phone, typed, "");
        let stop = stop(&flow);
        assert_eq!(stop.id(), "not-confirmed", "{typed:?}");
        assert_eq!(stop.message, ["What was typed is not the phone's codename (cheetah), so the install stops here. Nothing was written."]);
        assert_eq!(stop.phone, PhoneFact::OnlyRead);
        assert!(block_on(flow.begin_write(phone.clone())).is_err());
        nothing_written(&phone);
    }
    // The write can't be begun at any step before the confirmation, nor at
    // the confirmation itself: only after the codename was typed
    for state in [
        "warning",
        "phone",
        "google-terms",
        "factory-image",
        "answers",
        "dry-run",
        "confirm",
    ] {
        let phone = ready();
        let mut flow = flow_at(&bundle, &phone, state);
        let before = phone.log().len();
        assert!(
            matches!(
                block_on(flow.begin_write(phone.clone())),
                Err(Error::Refused(_))
            ),
            "{state}"
        );
        assert_eq!(phone.log().len(), before, "{state}: the phone was asked");
        assert_eq!(flow.screen().state, state);
        nothing_written(&phone);
    }
    // And the confirmation is not taken at any other step
    for state in [
        "warning",
        "phone",
        "google-terms",
        "factory-image",
        "answers",
        "dry-run",
        "write",
    ] {
        let phone = ready();
        let mut flow = flow_at(&bundle, &phone, state);
        assert!(flow.confirm_install("cheetah").is_err(), "{state}");
        assert_eq!(flow.screen().state, state);
    }
}

#[test]
fn a_dry_run_has_no_way_to_a_write() {
    let bundle = TestBundle::new();
    let phone = ready();
    // The same run without `write`: it ends at the dry run's last screen
    let mut flow = run(&bundle, &phone, FlowOptions::default());
    assert_eq!(flow.screen().state, "done");
    assert_eq!(flow.screen().action, "none");
    assert!(flow.confirm_install("cheetah").is_err());
    assert!(matches!(
        block_on(flow.begin_write(phone.clone())),
        Err(Error::Refused(_))
    ));
    assert!(phone.log().iter().all(|c| c.starts_with("getvar:")));
    nothing_written(&phone);

    // The session of a dry run is not one the flow takes as a write, and it
    // sends nothing whatever it is asked to flash
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    let release = flow.release().unwrap().clone();
    block_on(async {
        let mut session = flow.dry_run(phone.clone()).await.unwrap().unwrap();
        walk(&mut session, &bundle, &release).await.unwrap();
        assert_eq!(session.reach(), WriteReach::Nothing);
        assert!(flow.write_done(session).is_err());
    });
    nothing_written(&phone);
}

#[test]
fn the_maintainers_override_never_reaches_a_write() {
    let bundle = TestBundle::new();
    let overridden = || FlowOptions {
        write: true,
        maintainer_skip_bootloader_slot_check: true,
        ..Default::default()
    };
    // Asked for together with a write: stopped before the release is read,
    // on a phone the override would matter for and on one it would not
    for scenario in ["one-slot", "ready"] {
        let phone = FakePhone::scenario(scenario).unwrap();
        let mut flow = run(&bundle, &phone, overridden());
        let stop = stop(&flow);
        assert_eq!(stop.id(), "override-in-an-install");
        assert_eq!(stop.phone, PhoneFact::NotAsked);
        assert!(stop.message[0].contains("for dry runs only"));
        assert!(flow.acknowledge_warning().is_err());
        assert!(flow.confirm_install("cheetah").is_err());
        assert!(matches!(
            block_on(flow.begin_write(phone.clone())),
            Err(Error::Refused(_))
        ));
        assert!(phone.log().is_empty(), "{:?}", phone.log());
    }
    // In a dry run the override passes the one-slot stop, and that run ends
    // at the dry run: no confirmation, no write
    let phone = FakePhone::scenario("one-slot").unwrap();
    let mut flow = run(
        &bundle,
        &phone,
        FlowOptions {
            maintainer_skip_bootloader_slot_check: true,
            ..Default::default()
        },
    );
    assert_eq!(flow.screen().state, "done");
    assert!(flow.screen().notice.is_some());
    assert!(flow.confirm_install("cheetah").is_err());
    assert!(block_on(flow.begin_write(phone.clone())).is_err());
    nothing_written(&phone);
    // Without the override, a flow that writes stops at that phone
    let phone = FakePhone::scenario("one-slot").unwrap();
    assert_eq!(
        stop(&run(&bundle, &phone, writing())).id(),
        "bootloader-slots-unknown"
    );
    nothing_written(&phone);
}

/// A run that writes, with the owner's statement about the other slot.
fn stating(version: &str) -> FlowOptions {
    FlowOptions {
        write: true,
        bootloader_slots_match: Some(version.into()),
        ..Default::default()
    }
}

#[test]
fn a_phone_that_tells_one_bootloader_slot_is_written_only_on_the_owners_statement() {
    let bundle = TestBundle::new();
    let one_slot = || FakePhone::scenario("one-slot").unwrap();
    // Nothing stated: the stop there always was
    let phone = one_slot();
    assert_eq!(
        stop(&run(&bundle, &phone, writing())).id(),
        "bootloader-slots-unknown"
    );
    nothing_written(&phone);

    // Stated, and it is the version the phone reports for the slot in use
    // (tools/install-phone's --bootloader-slots-match): the install runs,
    // and every screen from the look at the phone on says whose word it
    // rests on, the confirmation among them
    let phone = one_slot();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        stating("fake-1.0"),
    );
    let mut said_at = Vec::new();
    loop {
        let screen = flow.screen();
        if screen.notice.as_deref() == Some(SLOTS_ON_THE_OWNERS_WORD) {
            said_at.push(screen.state);
        } else {
            assert_eq!(screen.notice, None, "{}", screen.state);
        }
        if screen.action == "none" {
            break;
        }
        let next = match screen.state {
            "warning" => "phone",
            "phone" => "google-terms",
            "google-terms" => "factory-image",
            "factory-image" => "answers",
            "answers" => "dry-run",
            "dry-run" => "confirm",
            "confirm" => "write",
            _ => "",
        };
        drive(&mut flow, &bundle, &phone, "cheetah", next);
    }
    assert_eq!(
        said_at,
        [
            "google-terms",
            "factory-image",
            "answers",
            "dry-run",
            "confirm",
            "write",
            "installed"
        ]
    );
    assert_eq!(flow.screen().state, "installed");
    holds_the_release(&bundle, &phone);
    only_our_commands(&phone);

    // Stated, and not the version the phone reports: a stop that names
    // both, at the first look, with only questions sent
    let phone = one_slot();
    let flow = run(&bundle, &phone, stating("fake-0.9"));
    let stopped = stop(&flow);
    assert_eq!(stopped.id(), "bootloader-slots-stated-other");
    assert_eq!(
        stopped.kind,
        StopKind::BootloaderSlotsStatedOther {
            stated: "fake-0.9".into(),
            phone: "fake-1.0".into()
        }
    );
    assert!(
        stopped.message[0].contains("is fake-0.9")
            && stopped.message[0].contains("reports fake-1.0")
    );
    assert!(phone.log().iter().all(|c| c.starts_with("getvar:")));
    nothing_written(&phone);

    // It is compared again at the write: a phone whose bootloader is
    // another one by then gets nothing
    let phone = one_slot();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        stating("fake-1.0"),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "write");
    assert_eq!(flow.screen().state, "write");
    phone.edit(|s| {
        s.vars
            .insert("version-bootloader".into(), "fake-0.9".into());
    });
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert_eq!(stop(&flow).id(), "bootloader-slots-stated-other");
    nothing_written(&phone);

    // A phone that reports both slots is judged by what it reports, as
    // install-phone does: a statement neither saves two that differ nor
    // stops two that match, and no screen mentions it
    let phone = FakePhone::scenario("slots-differ").unwrap();
    assert_eq!(
        stop(&run(&bundle, &phone, stating("fake-1.0"))).id(),
        "bootloader-slots-differ"
    );
    nothing_written(&phone);
    let phone = ready();
    let flow = run(&bundle, &phone, stating("fake-0.9"));
    assert_eq!(flow.screen().state, "installed");
    assert_eq!(flow.screen().notice, None);

    // It passes that one stop and no other
    let phone = one_slot();
    phone.edit(|s| {
        s.vars.insert("current-slot".into(), "b".into());
    });
    assert_eq!(
        stop(&run(&bundle, &phone, stating("fake-1.0"))).id(),
        "runs-from-slot-b"
    );
    nothing_written(&phone);

    // With the maintainer's override too, in a dry run: the statement is
    // what the run rests on, and a wrong one still stops
    let both = |version: &str| FlowOptions {
        maintainer_skip_bootloader_slot_check: true,
        bootloader_slots_match: Some(version.into()),
        ..Default::default()
    };
    let phone = one_slot();
    let flow = run(&bundle, &phone, both("fake-1.0"));
    assert_eq!(flow.screen().state, "done");
    assert_eq!(
        flow.screen().notice.as_deref(),
        Some(SLOTS_ON_THE_OWNERS_WORD)
    );
    let phone = one_slot();
    assert_eq!(
        stop(&run(&bundle, &phone, both("fake-0.9"))).id(),
        "bootloader-slots-stated-other"
    );

    // A locked phone: the unlock screen says that the second copy was not
    // read, before the owner erases anything
    let phone = one_slot();
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
    });
    let flow = run(&bundle, &phone, stating("fake-1.0"));
    let screen = flow.screen();
    assert_eq!(screen.state, "unlock");
    assert_eq!(
        screen.notice,
        Some(format!("{NO_USABLE_PHONE_YET} {SLOTS_ON_THE_OWNERS_WORD}"))
    );
    assert!(screen.body[1].contains("was not read by the installer"));
    assert!(!screen.body[1].contains("its two bootloader copies match"));
    nothing_written(&phone);
}

/// A bootloader that answers the per-slot versions with nothing after the
/// colon has not reported them. As in tools/install-phone, which takes an
/// empty answer as none: two empty answers are not "both reported and
/// equal", and the phone is written only on its owner's statement.
#[test]
fn a_phone_whose_per_slot_bootloader_versions_are_empty_has_not_reported_them() {
    let bundle = TestBundle::new();
    let empty_slots = |a: &str, b: &str| {
        let phone = ready();
        phone.edit(|s| {
            s.vars.insert("version-bootloader-a".into(), a.into());
            s.vars.insert("version-bootloader-b".into(), b.into());
        });
        phone
    };
    for (a, b) in [("", ""), (" ", " "), ("", "fake-1.0"), ("fake-1.0", "")] {
        // The flow's judge: the stop of a phone that tells one slot
        let phone = empty_slots(a, b);
        let flow = run(&bundle, &phone, writing());
        assert_eq!(stop(&flow).id(), "bootloader-slots-unknown", "{a:?} {b:?}");
        nothing_written(&phone);
        // A session reads the same lines the same way
        let phone = empty_slots(a, b);
        let release = bundle.verify().unwrap();
        let err = block_on(Session::connect(
            phone.clone(),
            &release,
            SERIAL,
            Default::default(),
        ))
        .unwrap_err();
        assert!(
            matches!(&err, Error::Phone(why) if why.contains("only reports slot a's bootloader")),
            "{a:?} {b:?}: {err}"
        );
        nothing_written(&phone);
    }
    // With the owner's statement such a phone is written, and every screen
    // says whose word the run rests on: nothing was read from the phone
    let phone = empty_slots("", "");
    let flow = run(&bundle, &phone, stating("fake-1.0"));
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );
    assert_eq!(flow.screen().notice, Some(SLOTS_ON_THE_OWNERS_WORD.into()));
    holds_the_release(&bundle, &phone);
}

#[test]
fn a_flow_that_writes_says_before_the_unlock_steps_what_its_install_leaves() {
    let bundle = TestBundle::new();
    // A dry run: this version installs nothing, so there is no reason to
    // unlock for it
    let phone = FakePhone::scenario("locked").unwrap();
    let flow = run(&bundle, &phone, FlowOptions::default());
    assert!(flow.ends_at_dry_run());
    let screen = flow.screen();
    assert_eq!(screen.state, "unlock");
    assert_eq!(screen.notice.as_deref(), Some(NO_INSTALL_YET));
    // A run that writes does install, so that sentence would be false. It
    // says what the install leaves: a phone nobody can sign in to
    let phone = FakePhone::scenario("locked").unwrap();
    let flow = run(&bundle, &phone, writing());
    assert!(!flow.ends_at_dry_run());
    let screen = flow.screen();
    assert_eq!(screen.state, "unlock");
    assert_eq!(screen.notice.as_deref(), Some(NO_USABLE_PHONE_YET));
    assert_eq!(NO_USABLE_PHONE_YET, "This version of the installer can install Omarchy Mobile, but it cannot put your answers on the phone: the phone it installs has no user account, and there is nothing to unlock it with. Unlocking the bootloader erases the phone. Unlock it for this version only if the phone is one you can do without.");
    let all = screen.body.join(" ");
    assert!(!all.contains("cannot install") && !all.contains("not for now"));
    assert!(
        all.contains("The installer does not send the unlock command. You do, when you are ready:")
    );
    nothing_written(&phone);
}

#[test]
fn the_one_gate_has_the_releases_list_only_in_the_session_that_writes() {
    let bundle = TestBundle::new();
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    let release = flow.release().unwrap().clone();
    block_on(async {
        // The dry run of a flow that will write: its gate passes no flash
        // at all, whatever the session above it does
        let mut session = flow.dry_run(phone.clone()).await.unwrap().unwrap();
        assert!(session.is_dry_run());
        assert_eq!(session.may_flash(), [] as [String; 0]);
        walk(&mut session, &bundle, &release).await.unwrap();
        flow.dry_run_done(session).unwrap();
        nothing_written(&phone);
        flow.confirm_install("cheetah").unwrap();
        // The session that writes: the release's partitions, whole and in
        // its order, and nothing else
        let session = flow.begin_write(phone.clone()).await.unwrap().unwrap();
        assert!(!session.is_dry_run());
        assert_eq!(session.may_flash(), OURS);
    });
    nothing_written(&phone);
}

#[test]
fn a_release_for_another_phone_model_is_not_written() {
    // Signed and valid, for a phone whose slot handling this write path
    // was not written for: a dry run goes on, a write stops at once
    let bundle = TestBundle::for_device("tegu");
    assert!(bundle.verify().is_ok());
    let phone = FakePhone::new("tegu");
    let mut flow = run(&bundle, &phone, writing());
    let stop = stop(&flow);
    assert_eq!(stop.id(), "writes-not-for-this-device");
    assert!(matches!(&stop.kind, StopKind::WritesNotForDevice { codename } if codename == "tegu"));
    assert_eq!(stop.phone, PhoneFact::NotAsked);
    assert!(stop.message[0].contains("writes only to: cheetah"));
    assert!(flow.confirm_install("tegu").is_err());
    assert!(block_on(flow.begin_write(phone.clone())).is_err());
    assert!(phone.log().is_empty());
}

#[test]
fn a_write_needs_every_image_checked_in_this_run() {
    // A front end that did not walk the images through the dry run (it does
    // not have them) is not asked to confirm anything
    let bundle = TestBundle::new();
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    block_on(async {
        let session = flow.dry_run(phone.clone()).await.unwrap().unwrap();
        flow.dry_run_done(session).unwrap();
    });
    assert_eq!(stop(&flow).id(), "images-not-checked");
    assert!(flow.confirm_install("cheetah").is_err());
    assert!(block_on(flow.begin_write(phone.clone())).is_err());
    nothing_written(&phone);
}

// ---- Rule 3: sizes, before the first write -------------------------------

#[test]
fn every_image_is_checked_against_its_partition_before_the_first_write() {
    let bundle = TestBundle::new();
    type Change = fn(&FakePhone);
    let wrong: [(&str, Change); 5] = [
        // An image larger than its partition
        ("larger than vendor_kernel_boot_b", |p| {
            p.edit(|s| {
                s.partitions
                    .insert("vendor_kernel_boot_b".into(), vec![SENTINEL; 8 << 10]);
            });
        }),
        // A footer image whose partition is larger than the image was
        // padded for: its AVB footer would not be at the partition's end
        ("AVB footer", |p| {
            p.edit(|s| {
                s.partitions
                    .insert("boot_b".into(), vec![SENTINEL; 32 << 10]);
            });
        }),
        ("AVB footer", |p| {
            p.edit(|s| {
                s.partitions
                    .insert("init_boot_b".into(), vec![SENTINEL; 16 << 10]);
            });
        }),
        // The root image, unpacked, larger than userdata
        ("larger than userdata", |p| {
            p.edit(|s| {
                s.partitions
                    .insert("userdata".into(), vec![SENTINEL; 1 << 20]);
            });
        }),
        // A partition the phone reports no size for
        ("getvar:partition-size:dtbo_b", |p| {
            p.edit(|s| {
                s.partitions.remove("dtbo_b");
            });
        }),
    ];
    for (why, change) in wrong {
        // Found by the dry run: the owner is never asked to confirm
        let phone = ready();
        change(&phone);
        let flow = run(&bundle, &phone, writing());
        let stop = stop(&flow);
        assert_eq!(stop.id(), "dry-run-refused", "{why}");
        assert!(stop.message[0].contains(why), "{why}: {:?}", stop.message);
        nothing_written(&phone);

        // And checked again by the session that writes, before its first
        // write: a phone that changed after the confirmation gets nothing,
        // although every boot image before the wrong one would have fitted
        let phone = ready();
        let mut flow = flow_at(&bundle, &phone, "write");
        change(&phone);
        drive(&mut flow, &bundle, &phone, "cheetah", "");
        let stop = self::stop(&flow);
        assert_eq!(stop.id(), "write-refused", "{why}");
        assert!(stop.message[0].contains(why), "{why}: {:?}", stop.message);
        assert_eq!(
            stop.title,
            "The install stopped before anything was written"
        );
        assert!(stop.footer().join(" ").contains("written nothing"));
        assert!(
            phone.not_reads().is_empty(),
            "{why}: {:?}",
            phone.not_reads()
        );
        nothing_written(&phone);
    }
    // An image over the phone's max-download-size
    let phone = ready();
    phone.edit(|s| s.max_download = 12 << 10);
    assert_eq!(
        stop(&run(&bundle, &phone, writing())).id(),
        "dry-run-refused"
    );
    nothing_written(&phone);
}

// ---- Rule 4: interrupted anywhere, and run again -------------------------

/// What the stop says the writes reached must be what the phone received.
fn reach_is_true(phone: &FakePhone, reach: WriteReach) {
    let changes = phone.changes();
    let flashes = |part: fn(&str) -> bool| {
        changes
            .iter()
            .filter(|c| c.strip_prefix("flash:").is_some_and(part))
            .count() as u32
    };
    let (boot, userdata) = (flashes(|p| p != "userdata"), flashes(|p| p == "userdata"));
    let slot = changes.iter().any(|c| c == "set_active:b");
    match reach {
        WriteReach::Nothing => assert!(changes.is_empty(), "{changes:?}"),
        WriteReach::BootImages { done, of, in_doubt } => {
            assert_eq!(of, 5);
            assert!(
                done <= boot && boot <= done + u32::from(in_doubt),
                "{reach:?}"
            );
            assert_eq!(userdata, 0);
            assert!(!slot);
        }
        WriteReach::Userdata { pieces } => {
            assert_eq!(boot, 5);
            assert!(pieces <= userdata && userdata <= pieces + 1, "{reach:?}");
            assert!(!slot);
        }
        WriteReach::Written => assert!(boot == 5 && userdata > 4 && !slot),
        WriteReach::SlotInDoubt => assert!(boot == 5 && userdata > 4),
        WriteReach::SlotSet => {
            assert!(slot);
            assert_eq!(phone.0.borrow().active_slot, "b");
        }
        WriteReach::Rebooted => panic!("a stop after the restart was confirmed"),
    }
}

#[test]
fn an_install_interrupted_anywhere_can_be_run_again() {
    let bundle = TestBundle::new();
    let whole = ready();
    assert!(matches!(
        run(&bundle, &whole, writing()).step(),
        Step::Installed(_)
    ));
    let log = whole.log();
    let first = log.iter().position(|c| c.starts_with("download:")).unwrap();
    // The cable is pulled before each command from the first write on (so
    // before and after every write), and during each command that is not a
    // question (it reaches the phone, and the answer is lost)
    let mut cuts: Vec<(usize, bool)> = (first..log.len()).map(|n| (n, false)).collect();
    cuts.extend(
        (first..log.len())
            .filter(|n| !log[*n].starts_with("getvar:"))
            .map(|n| (n, true)),
    );
    let mut reaches = Vec::new();
    for (at, answer_lost) in cuts {
        let what = format!(
            "{} {} (command {at})",
            if answer_lost { "during" } else { "before" },
            log[at]
        );
        let phone = ready();
        phone.edit(|s| {
            if answer_lost {
                s.lose_answer_at = Some(at)
            } else {
                s.unplug_at = Some(at)
            }
        });
        let flow = run(&bundle, &phone, writing());
        let stop = stop(&flow);
        assert_eq!(stop.id(), "write-interrupted", "{what}");
        assert!(
            matches!(&stop.kind, StopKind::WriteInterrupted(Error::Transport(_))),
            "{what}"
        );
        let PhoneFact::WriteStopped { reach, on_slot_b } = stop.phone else {
            panic!("{what}: {:?}", stop.phone)
        };
        assert!(!on_slot_b);
        reach_is_true(&phone, reach);
        only_our_commands(&phone);
        // The stop says what state the phone is in and what a rerun does,
        // and never that nothing was written unless that is so
        let footer = stop.footer().join(" ");
        match reach {
            WriteReach::Nothing => {
                assert!(footer.starts_with("No write reached the phone"), "{what}");
                assert_eq!(
                    stop.title,
                    "The install stopped before anything was written"
                );
            }
            WriteReach::BootImages { .. } => {
                assert!(footer.contains("userdata was not written"), "{what}");
                assert!(footer.contains("Slot A, where Android is, was not written"));
            }
            WriteReach::Userdata { .. } => {
                assert!(footer.contains("is erased") && footer.contains("factory reset"));
            }
            WriteReach::Written | WriteReach::SlotInDoubt => {
                assert!(footer.starts_with("Every image was written"), "{what}");
                assert!(footer.contains("is erased"));
            }
            WriteReach::SlotSet => {
                assert!(footer.contains("slot B is the active slot"), "{what}");
                assert!(footer.contains("Restart the phone"));
            }
            WriteReach::Rebooted => unreachable!(),
        }
        if reach != WriteReach::Nothing {
            assert_eq!(stop.title, "The install stopped partway");
            assert!(!footer.contains("written nothing") && !footer.contains("No write reached"));
            assert!(
                footer.contains("writes every image again, from the first"),
                "{what}"
            );
        }
        if !reaches.contains(&reach) {
            reaches.push(reach);
        }

        // The phone restarted: the answer to `reboot` is all that was lost
        if phone.0.borrow().rebooted {
            assert_eq!(reach, WriteReach::SlotSet);
            holds_the_release(&bundle, &phone);
            continue;
        }
        // The owner plugs it back in, still in Fastboot Mode, and starts
        // the installer again
        phone.replug();
        let before = phone.changes().len();
        let on_b = phone.0.borrow().vars["current-slot"] == "b";
        let mut again = run(&bundle, &phone, writing());
        if on_b {
            // Slot B was made active before the cable went: the installer
            // sees a phone on slot B, stops, and writes nothing; said to be
            // a reinstall, it runs through
            assert!(matches!(
                reach,
                WriteReach::SlotInDoubt | WriteReach::SlotSet
            ));
            assert_eq!(self::stop(&again).id(), "runs-from-slot-b", "{what}");
            assert_eq!(phone.changes().len(), before, "{what}");
            again = run(
                &bundle,
                &phone,
                FlowOptions {
                    write: true,
                    reinstall: true,
                    ..Default::default()
                },
            );
        }
        assert!(
            matches!(again.step(), Step::Installed(_)),
            "{what}: {:?}",
            again.step()
        );
        holds_the_release(&bundle, &phone);
        only_our_commands(&phone);
    }
    // Every place an install can stop was met
    for want in [
        WriteReach::Nothing,
        WriteReach::BootImages {
            done: 0,
            of: 5,
            in_doubt: true,
        },
        WriteReach::BootImages {
            done: 3,
            of: 5,
            in_doubt: false,
        },
        WriteReach::BootImages {
            done: 5,
            of: 5,
            in_doubt: false,
        },
        WriteReach::Userdata { pieces: 0 },
        WriteReach::Userdata { pieces: 3 },
        WriteReach::Written,
        WriteReach::SlotInDoubt,
        WriteReach::SlotSet,
    ] {
        assert!(reaches.contains(&want), "never stopped at {want:?}");
    }
}

#[test]
fn a_phone_that_refuses_a_write_stops_the_install_and_says_how_far_it_got() {
    let bundle = TestBundle::new();
    // FAIL to a boot image's flash: that partition is in doubt, userdata
    // is untouched, and no slot is made active
    let phone = ready();
    phone.edit(|s| s.fail_on.push("flash:vendor_boot_b".into()));
    let flow = run(&bundle, &phone, writing());
    let stop = stop(&flow);
    assert_eq!(stop.id(), "write-interrupted");
    assert_eq!(
        stop.phone,
        PhoneFact::WriteStopped {
            reach: WriteReach::BootImages {
                done: 2,
                of: 5,
                in_doubt: true
            },
            on_slot_b: false
        }
    );
    assert_eq!(stop.footer(), [
        "The install stopped while it was writing slot B's boot images: 2 of 5 were written, and the next one may be partly written.",
        "userdata was not written: what the phone had stored is still there.",
        "Slot A, where Android is, was not written, and it is still the slot the phone starts from.",
        "Bring the phone to Fastboot Mode and start the installer again: it checks the phone again and writes every image again, from the first.",
    ]);
    assert_eq!(phone.written(), ["boot_b", "init_boot_b"]);
    assert!(!phone
        .log()
        .iter()
        .any(|c| c == "set_active:b" || c == "reboot" || c == "flash:userdata"));

    // FAIL to set_active: everything is written, the slot is unchanged
    let phone = ready();
    phone.edit(|s| s.fail_on.push("set_active".into()));
    let flow = run(&bundle, &phone, writing());
    assert_eq!(
        self::stop(&flow).phone,
        PhoneFact::WriteStopped {
            reach: WriteReach::Written,
            on_slot_b: false
        }
    );
    assert!(!phone.0.borrow().rebooted);
    assert_eq!(phone.0.borrow().active_slot, "a");

    // A reinstall that stops partway: slot B is the slot the phone starts
    // from, and the stop says so instead of pointing at Android
    let phone = FakePhone::scenario("slot-b").unwrap();
    phone.edit(|s| s.fail_on.push("flash:dtbo_b".into()));
    let flow = run(
        &bundle,
        &phone,
        FlowOptions {
            write: true,
            reinstall: true,
            ..Default::default()
        },
    );
    let footer = self::stop(&flow).footer().join(" ");
    assert!(footer.contains("The phone was running from slot B, which is the slot being written"));
    assert!(!footer.contains("still the slot the phone starts from"));

    // A session that has not run through can't be handed in as done
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "write");
    block_on(async {
        let session = flow.begin_write(phone.clone()).await.unwrap().unwrap();
        // A second write can't be opened beside the first
        assert!(flow.begin_write(phone.clone()).await.is_err());
        assert!(flow.write_done(session).is_err());
    });
    assert_eq!(flow.screen().state, "write");
    nothing_written(&phone);
}

// ---- Rule 9: what the owner is shown before the confirmation --------------

/// The root image is hashed before the first write and then read again to
/// be sent. If what is read the second time is not what was hashed (the
/// file changed on disk, or another file is behind the name), the session
/// finds out from its own hash of what it was sent, and slot B is never
/// made active for it.
#[test]
fn a_root_image_that_is_not_the_one_that_was_checked_never_makes_slot_b_active() {
    let bundle = TestBundle::new();
    // The proof, from the release's own bytes
    let proof = |release: &omamobile_installer_core::bundle::VerifiedRelease| {
        let mut check = release.root_image_check();
        check.update(&bundle.root_zst);
        check.finish().unwrap()
    };
    // What is streamed afterwards: a well-formed image of the same size,
    // one bit away, which no check of sizes or of the sparse format can tell
    let mut other_raw = bundle.root_raw.clone();
    other_raw[200 * 4096 + 17] ^= 1;
    let other_zst = support::bundle::zstd_store(&other_raw, true);
    assert_eq!(other_zst.len(), bundle.root_zst.len());

    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "write");
    let release = flow.release().unwrap().clone();
    let root = proof(&release);
    let mut session = block_on(flow.begin_write(phone.clone())).unwrap().unwrap();
    let err = block_on(async {
        for write in &release.manifest().boot_writes {
            let image = release
                .verify_boot_image(write, bundle.image(&write.image))
                .unwrap();
            session.flash_boot(&image).await.unwrap();
        }
        flash_userdata_from_reader(&mut session, &root, &other_zst[..], None).await
    })
    .unwrap_err();
    assert!(
        matches!(&err, Error::Checksum(why) if why.contains("is not the one that was checked before the first write") && why.contains("slot B is not made active")),
        "{err}"
    );
    // The session is over: slot B can't be made active through it
    assert!(matches!(
        block_on(session.activate_and_reboot()),
        Err(Error::Refused(_))
    ));
    assert!(!session.is_done());
    assert!(!phone
        .log()
        .iter()
        .any(|c| c.starts_with("set_active") || c == "reboot"));
    assert_eq!(phone.0.borrow().active_slot, "a");
    assert!(!phone.0.borrow().rebooted);
    only_our_commands(&phone);
    // The stop does not say that userdata holds the release: it says that
    // userdata was begun and the install did not run through
    assert!(matches!(session.reach(), WriteReach::Userdata { .. }));
    flow.write_failed(session, &err).unwrap();
    let stop = stop(&flow);
    assert_eq!(stop.id(), "write-interrupted");
    assert!(stop.message[0].contains("is not the one that was checked"));
    let footer = stop.footer().join(" ");
    assert!(footer.contains("userdata"), "{footer}");
    assert!(!footer.contains("Every image was written"), "{footer}");

    // The same through a session used without the helper: pieces of the
    // right image, but the session was never shown the bytes they were cut
    // from. It can't vouch for them, and refuses to finish userdata
    let phone = ready();
    let mut flow = flow_at(&bundle, &phone, "write");
    let release = flow.release().unwrap().clone();
    let root = proof(&release);
    let mut session = block_on(flow.begin_write(phone.clone())).unwrap().unwrap();
    let err = block_on(async {
        for write in &release.manifest().boot_writes {
            let image = release
                .verify_boot_image(write, bundle.image(&write.image))
                .unwrap();
            session.flash_boot(&image).await.unwrap();
        }
        let mut stream =
            omamobile_installer_core::install::UserdataStream::new(session.max_download_size());
        stream.feed(&bundle.root_zst).unwrap();
        session
            .begin_userdata(&root, stream.image_len().unwrap())
            .await
            .unwrap();
        while let Some(piece) = stream.next_segment().unwrap() {
            session.flash_userdata_segment(&piece).await.unwrap();
        }
        stream.end().unwrap();
        while let Some(piece) = stream.next_segment().unwrap() {
            session.flash_userdata_segment(&piece).await.unwrap();
        }
        session.finish_userdata().await
    })
    .unwrap_err();
    assert!(matches!(err, Error::Checksum(_)), "{err}");
    assert!(matches!(
        block_on(session.activate_and_reboot()),
        Err(Error::Refused(_))
    ));
    assert_eq!(phone.0.borrow().active_slot, "a");

    // The control: the right bytes, the same way, run through
    let phone = ready();
    let flow = run(&bundle, &phone, writing());
    assert!(matches!(flow.step(), Step::Installed(_)));
    holds_the_release(&bundle, &phone);
}

#[test]
fn the_confirmation_lists_every_partition_its_size_and_what_is_erased() {
    let bundle = TestBundle::new();
    let phone = ready();
    let flow = flow_at(&bundle, &phone, "confirm");
    // Nothing but questions reached the phone up to here
    assert!(phone.log().iter().all(|c| c.starts_with("getvar:")));
    let Step::Confirm(plan) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    let pieces = plan.writes[5].pieces.unwrap();
    let screen = flow.screen();
    println!("{screen:#?}");
    assert_eq!(screen.state, "confirm");
    assert_eq!(screen.action, "confirm");
    assert_eq!(screen.title, "This is what the installer will write");
    assert_eq!(screen.body, [
        "The Pixel 7 Pro (cheetah) is unlocked and runs from slot a; bootloader fake-1.0, baseband fake-radio-1.0.".to_string(),
        format!("Google's factory image for build TEST.260101.001 is the right file ({}).", bundle.factory_zip_name),
        "Your answers: user alex (PIN), phone name alex-phone, time zone Europe/Paris, Wi-Fi not set.".into(),
        "Every image of the release was checked against its signed checksum, and each fits its partition on this phone. The installer will write these partitions, in this order, and no other:".into(),
    ]);
    // Exactly the partitions of the release, each with the image's size and
    // the partition's size on this phone; then the slot and the restart
    assert_eq!(screen.plan, [
        "boot_b: images/boot.img, 16384 bytes (the partition holds 16384 bytes)".to_string(),
        "init_boot_b: images/init_boot.img, 8192 bytes (the partition holds 8192 bytes)".into(),
        "vendor_boot_b: images/vendor_boot.img, 4096 bytes (the partition holds 16384 bytes)".into(),
        "vendor_kernel_boot_b: images/vendor_kernel_boot.img, 12288 bytes (the partition holds 32768 bytes)".into(),
        "dtbo_b: images/dtbo.img, 4096 bytes (the partition holds 8192 bytes)".into(),
        format!("userdata: omamobile-rootfs-cheetah-2026.01.02-aarch64.img.zst, 2097152 bytes unpacked, sent in {pieces} pieces (the partition holds 8388608 bytes). ERASES userdata"),
        "then: make slot B the active slot (set_active:b)".into(),
        "then: restart the phone (reboot)".into(),
    ]);
    assert_eq!(screen.footer, [
        "userdata will be ERASED: everything Android stored on the phone is lost, and Android on slot A needs a factory reset if it is ever started again.",
        "Slot B's boot partitions, listed above, are overwritten.",
        "Slot A itself, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written.",
        "This version of the installer cannot put your answers on the phone: it has no way yet to write them into the system image. So the phone starts Omarchy Mobile with no user account, no PIN or password and no Wi-Fi set up, and there is nothing to unlock it with. An install made with this version shows that the install itself works; it does not give you a phone you can use.",
        "To go ahead, type the phone's codename: cheetah",
        "Anything else stops the install here, with nothing written.",
    ]);

    // What is then written is that list, and the list is the release's
    let listed: Vec<&str> = plan.writes.iter().map(|w| w.partition.as_str()).collect();
    assert_eq!(listed, OURS);
    let mut flow = flow;
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert!(matches!(flow.step(), Step::Installed(_)));
    let flashed: Vec<String> = phone
        .log()
        .iter()
        .filter_map(|c| c.strip_prefix("flash:").map(str::to_string))
        .collect();
    let mut once = flashed.clone();
    once.dedup();
    assert_eq!(once, OURS);
    assert_eq!(
        flashed.iter().filter(|p| *p == "userdata").count() as u32,
        pieces
    );

    // A reinstall says that the Omarchy Mobile on the phone is replaced
    let phone = FakePhone::scenario("slot-b").unwrap();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions {
            write: true,
            reinstall: true,
            ..Default::default()
        },
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    let screen = flow.screen();
    assert!(screen.body[0].contains("runs from slot b"));
    assert_eq!(screen.footer[1], "Slot B's boot partitions, listed above, are overwritten: the Omarchy Mobile that is on the phone now is replaced.");

    // The screen before the writes, and the steps before the confirmation
    // that say a write follows
    let phone = ready();
    let flow = flow_at(&bundle, &phone, "write");
    let screen = flow.screen();
    assert_eq!(screen.title, "Writing to the phone");
    assert!(screen.body[0].contains("writes the 6 partitions that were listed"));
    assert!(screen.body[1].contains("Don't unplug it"));
    let flow = flow_at(&bundle, &ready(), "dry-run");
    assert!(flow.screen().body[1].contains(
        "Nothing is written before you have seen the list and typed the phone's codename"
    ));
    let flow = flow_at(&bundle, &ready(), "answers");
    assert!(flow.screen().body[2].contains("starts with no user account"));
}

#[test]
fn a_flow_that_writes_goes_through_these_states_in_this_order() {
    // The order of the states of a flow that writes
    let bundle = TestBundle::new();
    let phone = ready();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    let mut seen = Vec::new();
    drive_with(&mut flow, &bundle, &phone, "cheetah", "", |state| {
        seen.push(state.to_string())
    });
    assert_eq!(
        seen,
        [
            "warning",
            "phone",
            "google-terms",
            "factory-image",
            "answers",
            "dry-run",
            "confirm",
            "write",
            "installed"
        ]
    );
}
