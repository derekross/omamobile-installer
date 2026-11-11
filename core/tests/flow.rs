// SPDX-License-Identifier: MIT
//! The install flow, state by state and refusal by refusal, against the
//! fake phone. No USB. Every test also checks what reached the phone:
//! `getvar:` and nothing else.
mod support;
use omamobile_installer_core::bundle::ReleaseFiles;
use omamobile_installer_core::fastboot::{PhoneId, Session, Transport};
use omamobile_installer_core::flow::{
    AnswerField, AnswersInput, Flow, FlowOptions, OwnerAnswers, PhoneFact, Retry, Step, Stop,
    StopKind, ZipRefusal, UNLOCK_COMMAND,
};
use omamobile_installer_core::install::flash_userdata_from_reader;
use omamobile_installer_core::Error;
use pollster::block_on;
use serde_json::{json, Value};
use support::bundle::{TestBundle, PHONE_BUILD};
use support::fake::{FakePhone, SERIAL};

fn begin(bundle: &TestBundle) -> Flow {
    Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions::default(),
    )
}
fn id() -> PhoneId {
    PhoneId::of_serial(SERIAL).unwrap()
}
fn phone(scenario: &str) -> FakePhone {
    FakePhone::scenario(scenario).unwrap()
}
fn look(flow: &mut Flow, phone: &FakePhone) -> Result<(), Error> {
    block_on(flow.read_phone(&mut phone.clone(), &id()))
}
/// Hand a file over as a front end does: name and size, then its bytes in
/// pieces, never whole.
fn give(flow: &mut Flow, name: &str, bytes: &[u8]) -> Result<(), Error> {
    if let Some(mut check) = flow.factory_zip(name, bytes.len() as u64)? {
        for chunk in bytes.chunks(7001) {
            check.update(chunk);
        }
        flow.factory_zip_done(check)?;
    }
    Ok(())
}
fn answers() -> OwnerAnswers {
    OwnerAnswers::check(&AnswersInput {
        username: "alex",
        password: "4812",
        password_again: "4812",
        hostname: "",
        timezone: "Europe/Paris",
        wifi_ssid: "",
        wifi_psk: "",
    })
    .unwrap()
}
fn stop(flow: &Flow) -> &Stop {
    match flow.step() {
        Step::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}"),
    }
}
fn only_getvar(phone: &FakePhone) {
    let log = phone.log();
    assert!(
        log.iter().all(|c| c.starts_with("getvar:")),
        "something other than getvar was sent: {log:?}"
    );
    assert!(phone.written().is_empty());
    assert_eq!(phone.0.borrow().active_slot, "a");
    assert!(!phone.0.borrow().rebooted);
}
/// A flow taken to the given step on a ready phone.
fn flow_at(bundle: &TestBundle, phone: &FakePhone, until: &str) -> Flow {
    let mut flow = begin(bundle);
    for state in ["warning", "phone", "factory-image", "answers", "dry-run"] {
        assert_eq!(flow.screen().state, state);
        if state == until {
            return flow;
        }
        match state {
            "warning" => flow.acknowledge_warning().unwrap(),
            "phone" => look(&mut flow, phone).unwrap(),
            "factory-image" => {
                give(&mut flow, &bundle.factory_zip_name, &bundle.factory_zip).unwrap()
            }
            "answers" => flow.answers(&answers()).unwrap(),
            _ => unreachable!(),
        }
    }
    panic!("no step {until}");
}
/// Every stop says what is true of the phone and never reassures past it.
fn footer_is_honest(stop: &Stop) {
    let text = stop.footer().join(" ");
    assert!(!stop.message.is_empty() && !stop.title.is_empty());
    for claim in ["Nothing is lost", "nothing is lost", "safe to"] {
        assert!(!text.contains(claim) && !stop.message.join(" ").contains(claim));
    }
    match stop.retry {
        Retry::StopsAgain => assert!(text.contains("will stop here again")),
        Retry::AfterFix => assert!(text.contains("once that is done")),
        Retry::Whenever => assert!(text.contains("at any time")),
    }
    match stop.phone {
        PhoneFact::NotAsked => assert!(text.starts_with("Nothing was sent to a phone.")),
        PhoneFact::OnlyRead => {
            assert!(text.contains("written nothing to it") && !text.contains("erased"))
        }
        PhoneFact::OnlyReadUnlockShown => {
            assert!(
                text.contains("written nothing to it") && text.contains("that erased the phone")
            )
        }
        PhoneFact::TwoPhones { unlock_shown } => {
            assert!(text.contains("written nothing to either"));
            assert_eq!(text.contains("erased that phone"), unlock_shown);
        }
    }
}

#[test]
fn the_whole_flow_ends_at_a_dry_run_that_writes_nothing() {
    let bundle = TestBundle::new();
    let phone = phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    let release = flow.release().unwrap().clone();
    block_on(async {
        let mut session = flow.dry_run(phone.clone()).await.unwrap().unwrap();
        assert!(session.is_dry_run());
        // The front end has the release's files: it walks them through
        for write in &release.manifest().boot_writes {
            let image = release
                .verify_boot_image(write, bundle.image(&write.image))
                .unwrap();
            session.flash_boot(&image).await.unwrap();
        }
        let mut check = release.root_image_check();
        check.update(&bundle.root_zst);
        let root = check.finish().unwrap();
        flash_userdata_from_reader(&mut session, &root, &bundle.root_zst[..], None)
            .await
            .unwrap();
        session.activate_and_reboot().await.unwrap();
        flow.dry_run_done(session).unwrap();
    });
    let Step::Done(plan) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    assert!(plan.images_walked);
    assert_eq!(plan.factory_zip.build, PHONE_BUILD);
    assert_eq!(plan.answers.username, "alex");
    assert!(plan.answers.pin && !plan.answers.wifi);
    assert_eq!(plan.answers.hostname, "alex-phone");
    assert_eq!(plan.would.len(), 8, "{:?}", plan.would);
    assert!(plan.would[0].ends_with("flash:boot_b"));
    assert!(plan.would[4].ends_with("flash:dtbo_b"));
    assert!(plan.would[5].starts_with("flash userdata: the root image, in "));
    assert_eq!(plan.would[6..], ["set_active:b", "reboot"]);
    only_getvar(&phone);
    let screen = flow.screen();
    assert_eq!(screen.state, "done");
    assert!(screen.title.contains("Nothing was written"));
    assert!(screen
        .footer
        .join(" ")
        .contains("no step that writes to a phone"));
    // Done is the end: nothing more is accepted
    assert!(flow.acknowledge_warning().is_err());
    assert!(look(&mut flow, &phone).is_err());
    assert!(block_on(flow.dry_run(phone.clone())).is_err());
}

#[test]
fn a_dry_run_without_the_images_lists_the_signed_plan() {
    let bundle = TestBundle::new();
    let phone = phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    let session = block_on(flow.dry_run(phone.clone())).unwrap().unwrap();
    // A second dry run can't be opened beside the first
    assert!(matches!(
        block_on(flow.dry_run(phone.clone())),
        Err(Error::Refused(_))
    ));
    flow.dry_run_done(session).unwrap();
    let Step::Done(plan) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    assert!(!plan.images_walked);
    assert_eq!(plan.would.len(), 8);
    assert!(plan.would[0].starts_with("flash boot_b images/boot.img"));
    assert!(plan.would[5].contains("userdata") && plan.would[5].contains("erases userdata"));
    assert_eq!(plan.would[6..], ["set_active b", "reboot"]);
    assert!(flow.screen().body.join(" ").contains("were not fetched"));
    only_getvar(&phone);
}

#[test]
fn a_release_that_fails_verification_stops_before_anything() {
    let bundle = TestBundle::new();
    let other = TestBundle::new();
    let phone = phone("ready");
    let mut tampered = bundle.manifest_json.clone();
    tampered[40] ^= 1;
    let cases: [(
        &str,
        ReleaseFiles<'_>,
        &omamobile_installer_core::signature::TrustedKeys,
    ); 3] = [
        (
            "a key the installer doesn't trust",
            bundle.files(),
            &other.trusted(),
        ),
        (
            "a changed manifest",
            ReleaseFiles {
                manifest_json: &tampered,
                ..bundle.files()
            },
            &bundle.trusted(),
        ),
        (
            "another release's signature",
            ReleaseFiles {
                sha256sums_sig: &other.sig,
                ..bundle.files()
            },
            &bundle.trusted(),
        ),
    ];
    for (what, files, trusted) in cases {
        let mut flow = Flow::begin(&files, bundle.index(), trusted, FlowOptions::default());
        let s = stop(&flow).clone();
        assert!(matches!(s.kind, StopKind::ReleaseNotVerified(_)), "{what}");
        assert_eq!((s.phone, s.retry), (PhoneFact::NotAsked, Retry::AfterFix));
        footer_is_honest(&s);
        assert!(flow.release().is_none());
        // Nothing is accepted after a stop, and nothing reaches a phone
        assert!(flow.acknowledge_warning().is_err(), "{what}");
        assert!(flow.decline_warning().is_err());
        assert!(look(&mut flow, &phone).is_err());
        assert!(flow.no_phone().is_err());
        assert!(flow.factory_zip(&bundle.factory_zip_name, 1).is_err());
        assert!(flow.answers(&answers()).is_err());
        assert!(block_on(flow.dry_run(phone.clone())).is_err());
        assert_eq!(flow.screen().state, "stopped");
        assert_eq!(flow.screen().stop, Some("release-not-verified"));
    }
    assert!(phone.log().is_empty());
    // The descriptor's slot rule is part of the release: nonsense is refused
    let odd = TestBundle::build(|_| {}, |c| c + "BOOTLOADER_SLOT_PROBE=maybe\n");
    assert!(matches!(
        &stop(&begin(&odd)).kind,
        StopKind::ReleaseNotVerified(Error::DeviceConf(why)) if why.contains("BOOTLOADER_SLOT_PROBE")
    ));
}

#[test]
fn an_index_that_fails_its_checks_stops_before_the_warning() {
    let phone = phone("ready");
    // Signed, and it names a download on another host
    let bundle = TestBundle::with_index(|i| {
        let url = i["builds"][0]["url"]
            .as_str()
            .unwrap()
            .replace("dl.google.com", "dl.example.org");
        i["builds"][0]["url"] = json!(url);
    });
    let mut flow = begin(&bundle);
    let s = stop(&flow).clone();
    assert!(matches!(
        &s.kind,
        StopKind::BuildIndexRefused(Error::BuildIndex(_))
    ));
    assert_eq!((s.phone, s.retry), (PhoneFact::NotAsked, Retry::StopsAgain));
    assert!(s.message.join(" ").contains("fault in the release"));
    footer_is_honest(&s);
    assert!(flow.acknowledge_warning().is_err());
    assert!(look(&mut flow, &phone).is_err());

    // Well formed, but not the bytes the release signed: maybe a bad fetch
    let bundle = TestBundle::new();
    let mut bytes = bundle.index_json.clone();
    let at = bytes.iter().position(|b| *b == b'2').unwrap();
    bytes[at] = b'3';
    let flow = Flow::begin(
        &bundle.files(),
        Some(&bytes),
        &bundle.trusted(),
        FlowOptions::default(),
    );
    let s = stop(&flow);
    assert!(matches!(
        &s.kind,
        StopKind::BuildIndexRefused(Error::Checksum(_))
    ));
    assert_eq!(s.retry, Retry::AfterFix);
    footer_is_honest(s);
    // Listed by the release and not handed in
    let flow = Flow::begin(
        &bundle.files(),
        None,
        &bundle.trusted(),
        FlowOptions::default(),
    );
    assert!(matches!(
        &stop(&flow).kind,
        StopKind::BuildIndexRefused(Error::Checksum(_))
    ));

    // A release without one can't be installed by this flow, whatever is handed in
    let without = TestBundle::with_index(|i| *i = Value::Null);
    for index in [None, Some(&bundle.index_json[..])] {
        let flow = Flow::begin(
            &without.files(),
            index,
            &without.trusted(),
            FlowOptions::default(),
        );
        let s = stop(&flow);
        assert_eq!(s.kind, StopKind::NoBuildIndex);
        assert_eq!(s.retry, Retry::StopsAgain);
        footer_is_honest(s);
    }
    assert!(phone.log().is_empty());
}

#[test]
fn nothing_happens_before_the_warning_is_accepted() {
    let bundle = TestBundle::new();
    let phone = phone("ready");
    let mut flow = begin(&bundle);
    let screen = flow.screen();
    assert_eq!((screen.state, screen.action), ("warning", "acknowledge"));
    let text: String = screen.steps.iter().map(|s| s.text.as_str()).collect();
    for said in [
        "The phone is erased",
        "unlocking it (the next step) is what erases it",
        "replaces Android",
        "This release is a test",
        "https://omamobile.org/devices/cheetah",
        "You can go back",
    ] {
        assert!(text.contains(said), "the warning doesn't say: {said}");
    }
    // Every later step is refused, and the phone is not even asked what it is
    assert!(matches!(look(&mut flow, &phone), Err(Error::Refused(why)) if why.contains("warning")));
    assert!(flow.no_phone().is_err());
    assert!(flow
        .factory_zip(&bundle.factory_zip_name, bundle.factory_zip.len() as u64)
        .is_err());
    assert!(flow.answers(&answers()).is_err());
    assert!(block_on(flow.dry_run(phone.clone())).is_err());
    assert!(
        phone.log().is_empty(),
        "the phone was asked before the warning: {:?}",
        phone.log()
    );
    assert_eq!(flow.step(), &Step::Warning);

    // Declined: the end
    flow.decline_warning().unwrap();
    let s = stop(&flow).clone();
    assert_eq!(s.kind, StopKind::WarningDeclined);
    assert_eq!((s.phone, s.retry), (PhoneFact::NotAsked, Retry::Whenever));
    footer_is_honest(&s);
    assert!(flow.acknowledge_warning().is_err());
    assert!(look(&mut flow, &phone).is_err());
    assert!(phone.log().is_empty());

    // Accepted once: it is not a step again
    let mut flow = begin(&bundle);
    flow.acknowledge_warning().unwrap();
    assert!(flow.acknowledge_warning().is_err());
    assert!(flow.decline_warning().is_err());
    assert_eq!(flow.screen().state, "phone");
}

#[test]
fn a_wrong_codename_stops() {
    let bundle = TestBundle::new();
    let phone = phone("other-model");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow);
    assert_eq!(
        s.kind,
        StopKind::WrongModel {
            found: "panther".into()
        }
    );
    assert_eq!((s.phone, s.retry), (PhoneFact::OnlyRead, Retry::AfterFix));
    assert!(s.message[0].contains("'panther'") && s.message[0].contains("Pixel 7 Pro (cheetah)"));
    footer_is_honest(s);
    only_getvar(&phone);
}

#[test]
fn a_locked_bootloader_gets_guidance_and_nothing_is_sent() {
    let bundle = TestBundle::new();
    let phone = phone("locked");
    let mut flow = flow_at(&bundle, &phone, "phone");
    // How to reach Fastboot Mode comes from the descriptor, when it says
    assert!(flow.screen().body[0].contains("hold Volume Down, then also hold Power"));
    let silent = TestBundle::build(
        |_| {},
        |c| {
            c.lines()
                .filter(|l| !l.starts_with("INSTALL_KEYS_FASTBOOT"))
                .map(|l| format!("{l}\n"))
                .collect()
        },
    );
    let elsewhere = flow_at(&silent, &phone, "phone").screen().body[0].clone();
    assert!(
        elsewhere.contains("https://omamobile.org/devices/cheetah"),
        "{elsewhere}"
    );
    // Nothing found yet: said, and the step stays
    flow.no_phone().unwrap();
    assert_eq!(flow.step(), &Step::Phone { not_found: true });
    assert!(flow
        .screen()
        .problem
        .unwrap()
        .contains("No phone in Fastboot Mode"));

    look(&mut flow, &phone).unwrap();
    assert_eq!(flow.step(), &Step::Unlock);
    let screen = flow.screen();
    assert_eq!((screen.state, screen.action), ("unlock", "read-phone"));
    // The command is text for the owner, in one place
    let commands: Vec<&str> = screen
        .steps
        .iter()
        .filter_map(|s| s.command.as_deref())
        .collect();
    assert_eq!(commands, [UNLOCK_COMMAND]);
    assert_eq!(UNLOCK_COMMAND, "fastboot flashing unlock");
    assert!(screen
        .body
        .join(" ")
        .contains("does not send the unlock command"));
    assert!(screen.body.join(" ").contains("moment the phone is erased"));
    only_getvar(&phone);
    assert!(!phone.log().iter().any(
        |c| c.contains("flashing") || c.contains("unlock") && !c.starts_with("getvar:unlocked")
    ));

    // Still locked on the next look: the same step, and still only getvar
    look(&mut flow, &phone).unwrap();
    flow.no_phone().unwrap();
    assert_eq!(flow.step(), &Step::Unlock);
    // Later steps are not open yet
    assert!(flow.factory_zip(&bundle.factory_zip_name, 1).is_err());
    assert!(block_on(flow.dry_run(phone.clone())).is_err());

    // A stop after the unlock steps were shown says what unlocking did
    let mut stopped = flow_at(&bundle, &phone, "phone");
    look(&mut stopped, &phone).unwrap();
    assert_eq!(stopped.step(), &Step::Unlock);
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "yes".into());
        s.vars.insert("current-slot".into(), "b".into());
    });
    look(&mut stopped, &phone).unwrap();
    let s = stop(&stopped);
    assert_eq!(
        (s.id(), s.phone),
        ("runs-from-slot-b", PhoneFact::OnlyReadUnlockShown)
    );
    assert!(s.footer().join(" ").contains("that erased the phone"));
    footer_is_honest(s);
    phone.edit(|s| {
        s.vars.insert("current-slot".into(), "a".into());
    });

    // The owner unlocked it: on to the factory image
    look(&mut flow, &phone).unwrap();
    assert!(matches!(flow.step(), Step::FactoryImage { build, .. } if build.build == PHONE_BUILD));
    only_getvar(&phone);
}

#[test]
fn every_stop_comes_before_the_unlock_steps() {
    // A locked phone the install can't go on with is told so before its
    // owner is shown how to erase it
    let bundle = TestBundle::new();
    for (scenario, kind) in [
        ("slot-b", "runs-from-slot-b"),
        ("one-slot", "bootloader-slots-unknown"),
        ("slots-differ", "bootloader-slots-differ"),
        ("unknown-build", "build-not-listed"),
        ("untested-build", "build-not-tested"),
        ("other-model", "wrong-model"),
    ] {
        let phone = phone(scenario);
        phone.edit(|s| {
            s.vars.insert("unlocked".into(), "no".into());
        });
        let mut flow = flow_at(&bundle, &phone, "phone");
        look(&mut flow, &phone).unwrap();
        let s = stop(&flow);
        assert_eq!(s.id(), kind);
        assert_eq!(s.phone, PhoneFact::OnlyRead, "{scenario}");
        footer_is_honest(s);
        only_getvar(&phone);
    }
}

#[test]
fn a_phone_that_reports_one_slots_bootloader_stops_and_no_slot_is_switched() {
    let bundle = TestBundle::new();
    let phone = phone("one-slot");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow).clone();
    assert_eq!(
        s.kind,
        StopKind::BootloaderSlotsUnknown {
            probe_declared: false
        }
    );
    assert_eq!((s.phone, s.retry), (PhoneFact::OnlyRead, Retry::StopsAgain));
    let text = s.message.join(" ");
    assert!(text.contains("only tells the version of the copy in use (fake-1.0)"));
    assert!(text.contains("does not do that to this phone"));
    // No way around is handed out
    assert!(!text.contains("--") && !text.contains("slots-match"));
    footer_is_honest(&s);
    only_getvar(&phone);
    assert!(!phone.log().iter().any(|c| c.starts_with("set_active")));

    // A descriptor that allows the probe changes the words, and nothing else:
    // this version still switches no slot
    let allowed = TestBundle::build(|_| {}, |c| c + "BOOTLOADER_SLOT_PROBE=yes\n");
    assert!(allowed.verify().unwrap().rules().bootloader_slot_probe);
    assert!(!bundle.verify().unwrap().rules().bootloader_slot_probe);
    let phone = self::phone("one-slot");
    let mut flow = flow_at(&allowed, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow);
    assert_eq!(
        s.kind,
        StopKind::BootloaderSlotsUnknown {
            probe_declared: true
        }
    );
    assert_eq!(s.retry, Retry::StopsAgain);
    assert!(s
        .message
        .join(" ")
        .contains("this version of the installer does not do it yet"));
    only_getvar(&phone);
    assert!(!phone.log().iter().any(|c| c.starts_with("set_active")));
    // With both versions reported, the same descriptor installs as usual
    let phone = self::phone("ready");
    let mut flow = flow_at(&allowed, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    assert_eq!(flow.screen().state, "factory-image");

    // Both reported, and different
    let phone = self::phone("slots-differ");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow);
    assert_eq!(
        s.kind,
        StopKind::BootloaderSlotsDiffer {
            a: "fake-1.0".into(),
            b: "fake-0.9".into()
        }
    );
    assert_eq!(s.retry, Retry::StopsAgain);
    footer_is_honest(s);
    only_getvar(&phone);
}

#[test]
fn the_manifest_and_the_descriptor_must_agree_on_the_slot_probe() {
    let probe = |m: &mut Value, v: bool| m["install"]["bootloader_slot_probe"] = json!(v);
    // Stated by both, the same: accepted, and carried
    let both = TestBundle::build(|m| probe(m, true), |c| c + "BOOTLOADER_SLOT_PROBE=yes\n");
    let release = both.verify().unwrap();
    assert_eq!(release.manifest().bootloader_slot_probe, Some(true));
    assert!(release.rules().bootloader_slot_probe);
    let neither = TestBundle::build(|m| probe(m, false), |c| c + "BOOTLOADER_SLOT_PROBE=no\n");
    assert!(!neither.verify().unwrap().rules().bootloader_slot_probe);
    // One says yes and the other doesn't: refused, either way round
    let only_manifest = TestBundle::build(|m| probe(m, true), |c| c);
    assert!(
        matches!(only_manifest.verify(), Err(Error::DeviceConf(why)) if why.contains("BOOTLOADER_SLOT_PROBE"))
    );
    let only_conf = TestBundle::build(|m| probe(m, false), |c| c + "BOOTLOADER_SLOT_PROBE=yes\n");
    assert!(matches!(only_conf.verify(), Err(Error::DeviceConf(_))));
    // Not a yes or a no
    let odd = TestBundle::build(
        |m| m["install"]["bootloader_slot_probe"] = json!("yes"),
        |c| c,
    );
    assert!(matches!(odd.verify(), Err(Error::Manifest(_))));
}

#[test]
fn a_phone_on_slot_b_stops_unless_it_is_a_reinstall() {
    let bundle = TestBundle::new();
    let phone = phone("slot-b");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow);
    assert_eq!(s.kind, StopKind::RunsFromSlotB);
    assert_eq!(s.retry, Retry::StopsAgain);
    footer_is_honest(s);
    only_getvar(&phone);

    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions { reinstall: true },
    );
    flow.acknowledge_warning().unwrap();
    look(&mut flow, &phone).unwrap();
    give(&mut flow, &bundle.factory_zip_name, &bundle.factory_zip).unwrap();
    flow.answers(&answers()).unwrap();
    let session = block_on(flow.dry_run(phone.clone())).unwrap().unwrap();
    flow.dry_run_done(session).unwrap();
    assert!(matches!(flow.step(), Step::Done(plan) if plan.current_slot == "b"));
    only_getvar(&phone);
}

#[test]
fn a_phone_whose_pair_is_not_listed_or_not_tested_stops() {
    let bundle = TestBundle::new();
    // No build has both versions
    let phone = phone("unknown-build");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow).clone();
    assert_eq!(
        s.kind,
        StopKind::BuildNotListed {
            bootloader: "fake-7.7".into(),
            baseband: "fake-radio-7.7".into()
        }
    );
    assert_eq!(s.retry, Retry::AfterFix);
    let text = s.message.join(" ");
    assert!(text.contains("there is no nearest build"));
    assert!(text.contains("System update"));
    // The advice is followed by what it leads to
    assert!(text.contains("Android runs from slot B"));
    footer_is_honest(&s);
    only_getvar(&phone);

    // One version matches a tested build and the other doesn't: still nothing
    let phone = self::phone("ready");
    phone.edit(|s| {
        s.vars
            .insert("version-baseband".into(), "fake-radio-1.1".into());
    });
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    assert_eq!(stop(&flow).id(), "build-not-listed");

    // Listed, not tested, and nothing newer was tested: updating can't help,
    // and the stop doesn't say it can
    let phone = self::phone("untested-build");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow).clone();
    assert_eq!(
        s.kind,
        StopKind::BuildNotTested {
            build: "TEST.260301.001".into(),
            newer_tested: None
        }
    );
    assert_eq!(s.retry, Retry::StopsAgain);
    let text = s.message.join(" ");
    assert!(text.contains("updating Android does not help"));
    assert!(!text.contains("System update"));
    footer_is_honest(&s);
    only_getvar(&phone);

    // Listed, not tested, and a newer build was: that one is named
    let phone = self::phone("untested-old-build");
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    let s = stop(&flow).clone();
    assert_eq!(
        s.kind,
        StopKind::BuildNotTested {
            build: "TEST.251201.001".into(),
            newer_tested: Some(PHONE_BUILD.into())
        }
    );
    assert_eq!(s.retry, Retry::AfterFix);
    assert!(s.message.join(" ").contains("System update"));
    assert!(s.message.join(" ").contains("Android runs from slot B"));
    footer_is_honest(&s);

    // A phone that tells no baseband can't be looked up
    let phone = self::phone("ready");
    phone.edit(|s| {
        s.vars.remove("version-baseband");
    });
    let mut flow = flow_at(&bundle, &phone, "phone");
    look(&mut flow, &phone).unwrap();
    assert_eq!(stop(&flow).kind, StopKind::BasebandUnknown);
    footer_is_honest(stop(&flow));
    only_getvar(&phone);
}

#[test]
fn a_factory_zip_with_a_wrong_name_size_or_digest_is_not_taken() {
    let bundle = TestBundle::new();
    let phone = phone("ready");
    let mut flow = flow_at(&bundle, &phone, "factory-image");
    let (name, zip) = (bundle.factory_zip_name.clone(), bundle.factory_zip.clone());
    let refused = |flow: &Flow| match flow.step() {
        Step::FactoryImage { refused, .. } => refused.clone(),
        other => panic!("left the step: {other:?}"),
    };
    let screen = flow.screen();
    assert_eq!(
        (screen.state, screen.action),
        ("factory-image", "take-file")
    );
    assert_eq!(
        screen.link.as_deref(),
        Some("https://developers.google.com/android/images#cheetah")
    );
    let file = screen.file.unwrap();
    assert_eq!(
        (file.name.as_str(), file.size, file.build.as_str()),
        (name.as_str(), zip.len() as u64, PHONE_BUILD)
    );
    assert!(screen.body.join(" ").contains("never downloads them"));
    assert!(
        screen.body.join(" ").contains("TEST.260101.001.A1"),
        "the carrier variant is named"
    );
    assert!(screen.problem.is_none());

    // Another build's file, by its name: refused before a byte is read
    let other = "cheetah-test.260201.001-factory-00000000.zip";
    assert!(flow.factory_zip(other, zip.len() as u64).unwrap().is_none());
    assert_eq!(
        refused(&flow),
        Some(ZipRefusal::WrongName { got: other.into() })
    );
    assert!(flow.screen().problem.unwrap().contains(&name));
    for bad in [
        "factory.zip",
        "",
        &name.replace(".zip", ".zip.part"),
        &name.replace(".zip", " (x).zip"),
        &format!("x{name}"),
    ] {
        assert!(
            flow.factory_zip(bad, zip.len() as u64).unwrap().is_none(),
            "{bad}"
        );
    }
    // The right name, a download that isn't finished
    assert!(flow
        .factory_zip(&name, zip.len() as u64 - 1)
        .unwrap()
        .is_none());
    assert_eq!(
        refused(&flow),
        Some(ZipRefusal::WrongSize {
            name: name.clone(),
            got: zip.len() as u64 - 1,
            want: zip.len() as u64
        })
    );
    assert!(flow.screen().problem.unwrap().contains("isn't complete"));
    // Too large is not "incomplete"
    assert!(flow
        .factory_zip(&name, zip.len() as u64 + 1)
        .unwrap()
        .is_none());
    assert!(flow
        .screen()
        .problem
        .unwrap()
        .contains("not the file Google publishes"));
    // The size it should have, but fewer bytes actually arrive
    let mut check = flow.factory_zip(&name, zip.len() as u64).unwrap().unwrap();
    check.update(&zip[..1000]);
    assert_eq!(check.progress(), (1000, zip.len() as u64));
    flow.factory_zip_done(check).unwrap();
    assert!(matches!(
        refused(&flow),
        Some(ZipRefusal::WrongSize { got: 1000, .. })
    ));
    // The right name and size, one bit wrong
    let mut wrong = zip.clone();
    wrong[zip.len() / 2] ^= 1;
    give(&mut flow, &name, &wrong).unwrap();
    assert_eq!(
        refused(&flow),
        Some(ZipRefusal::WrongSha256 { name: name.clone() })
    );
    assert!(flow.screen().problem.unwrap().contains("SHA-256 differs"));
    assert!(flow.factory_zip_verified().is_none());
    // Nothing later opens on a refused file
    assert!(flow.answers(&answers()).is_err());
    assert!(block_on(flow.dry_run(phone.clone())).is_err());

    // A check made by another install is not this one's
    let other_bundle = TestBundle::new();
    let mut other_flow = flow_at(&other_bundle, &phone, "factory-image");
    let mut foreign = other_flow
        .factory_zip(&name, zip.len() as u64)
        .unwrap()
        .unwrap();
    foreign.update(&zip);
    assert!(matches!(
        flow.factory_zip_done(foreign),
        Err(Error::Refused(_))
    ));
    assert_eq!(flow.screen().state, "factory-image");

    // The file itself, also under the name a browser gives a second download
    let twice = name.replace(".zip", " (1).zip");
    give(&mut flow, &twice, &zip).unwrap();
    assert_eq!(flow.step(), &Step::Answers);
    let taken = flow.factory_zip_verified().unwrap();
    assert_eq!(
        (taken.build.as_str(), taken.size),
        (PHONE_BUILD, zip.len() as u64)
    );
    // Taken once
    assert!(flow.factory_zip(&name, zip.len() as u64).is_err());
    only_getvar(&phone);
}

#[test]
fn a_phone_swapped_between_steps_stops_the_install() {
    let bundle = TestBundle::new();
    let other_serial = "ANOTHERPHONE0002";
    let swap = |p: &FakePhone| {
        p.edit(|s| {
            s.vars.insert("serialno".into(), other_serial.into());
        });
    };
    // The phone the person chose is not the one that answers: nothing is
    // decided on it, and the step stays open
    let phone = phone("ready");
    let mut flow = flow_at(&bundle, &phone, "phone");
    let chosen = PhoneId::of_serial(other_serial).unwrap();
    assert!(matches!(
        block_on(flow.read_phone(&mut phone.clone(), &chosen)),
        Err(Error::Refused(why)) if why.contains("not the one that was chosen")
    ));
    assert_eq!(flow.screen().state, "phone");

    // Locked phone shown the unlock steps; another phone comes back
    let first = self::phone("locked");
    let mut flow = flow_at(&bundle, &first, "phone");
    look(&mut flow, &first).unwrap();
    assert_eq!(flow.step(), &Step::Unlock);
    let second = self::phone("ready");
    swap(&second);
    block_on(flow.read_phone(&mut second.clone(), &chosen)).unwrap();
    let s = stop(&flow).clone();
    assert_eq!(s.kind, StopKind::PhoneChanged);
    assert_eq!(s.phone, PhoneFact::TwoPhones { unlock_shown: true });
    assert_eq!(s.retry, Retry::AfterFix);
    footer_is_honest(&s);
    only_getvar(&first);
    only_getvar(&second);

    // Between the factory image and the dry run
    let first = self::phone("ready");
    let mut flow = flow_at(&bundle, &first, "dry-run");
    let second = self::phone("ready");
    swap(&second);
    assert!(block_on(flow.dry_run(second.clone())).unwrap().is_none());
    let s = stop(&flow).clone();
    assert_eq!(s.kind, StopKind::PhoneChanged);
    assert_eq!(
        s.phone,
        PhoneFact::TwoPhones {
            unlock_shown: false
        }
    );
    footer_is_honest(&s);
    only_getvar(&second);
    // And a second phone of another model is not "the phone" either
    let mut flow = flow_at(&bundle, &first, "dry-run");
    let second = self::phone("other-model");
    swap(&second);
    assert!(block_on(flow.dry_run(second.clone())).unwrap().is_none());
    assert_eq!(stop(&flow).id(), "wrong-model");
    assert_eq!(
        stop(&flow).phone,
        PhoneFact::TwoPhones {
            unlock_shown: false
        }
    );
    footer_is_honest(stop(&flow));

    // During the dry run itself: the session notices, and the flow stops
    let phone = self::phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    let release = flow.release().unwrap().clone();
    let mut session: Session<FakePhone> = block_on(flow.dry_run(phone.clone())).unwrap().unwrap();
    swap(&phone);
    let write = &release.manifest().boot_writes[0];
    let image = release
        .verify_boot_image(write, bundle.image("boot"))
        .unwrap();
    let err = block_on(session.flash_boot(&image)).unwrap_err();
    assert!(matches!(&err, Error::Refused(why) if why.contains("changed during the install")));
    flow.dry_run_failed(&err).unwrap();
    let s = stop(&flow);
    assert!(matches!(
        &s.kind,
        StopKind::DryRunRefused(Error::Refused(_))
    ));
    footer_is_honest(s);
    only_getvar(&phone);
    assert!(flow.dry_run_done(session).is_err());
}

#[test]
fn a_phone_that_tells_no_serial_number_is_not_taken() {
    // Nothing could tie an install to it, so nothing is decided on it
    let bundle = TestBundle::new();
    let phone = phone("ready");
    phone.edit(|s| {
        s.vars.insert("serialno".into(), String::new());
    });
    let mut flow = flow_at(&bundle, &phone, "phone");
    assert!(matches!(
        look(&mut flow, &phone),
        Err(Error::Phone(why)) if why.contains("tells no serial number")
    ));
    assert_eq!(flow.screen().state, "phone");
    assert!(PhoneId::of_serial("").is_none());
    assert_eq!(phone.log(), ["getvar:serialno"]);
    // And the same at the dry run, for a phone that told one before
    let phone = self::phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    phone.edit(|s| {
        s.vars.insert("serialno".into(), String::new());
    });
    assert!(matches!(
        block_on(flow.dry_run(phone.clone())),
        Err(Error::Phone(_))
    ));
    assert_eq!(flow.screen().state, "dry-run");
    only_getvar(&phone);
}

#[test]
fn the_dry_run_judges_the_phone_again() {
    let bundle = TestBundle::new();
    let set = |p: &FakePhone, pairs: &[(&str, &str)]| {
        p.edit(|s| {
            for (k, v) in pairs {
                s.vars.insert(k.to_string(), v.to_string());
            }
        });
    };
    // Locked again since the first look
    let phone = phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    set(&phone, &[("unlocked", "no")]);
    assert!(block_on(flow.dry_run(phone.clone())).unwrap().is_none());
    assert_eq!(stop(&flow).kind, StopKind::LockedAtDryRun);
    footer_is_honest(stop(&flow));
    only_getvar(&phone);

    // Another tested build's versions than the factory image was chosen for
    let phone = self::phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    set(
        &phone,
        &[
            ("version-bootloader", "fake-1.1"),
            ("version-bootloader-a", "fake-1.1"),
            ("version-bootloader-b", "fake-1.1"),
            ("version-baseband", "fake-radio-1.1"),
        ],
    );
    assert!(block_on(flow.dry_run(phone.clone())).unwrap().is_none());
    assert_eq!(stop(&flow).kind, StopKind::BuildChanged);
    footer_is_honest(stop(&flow));

    // The per-slot versions gone: the owner's word is never taken instead
    let phone = self::phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    phone.edit(|s| {
        s.vars.remove("version-bootloader-b");
    });
    assert!(block_on(flow.dry_run(phone.clone())).unwrap().is_none());
    assert_eq!(stop(&flow).id(), "bootloader-slots-unknown");

    // A check only the session makes: an image that doesn't fit its partition
    let phone = self::phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    phone.edit(|s| {
        s.partitions
            .insert("vendor_kernel_boot_b".into(), vec![0; 8 << 10]);
    });
    assert!(block_on(flow.dry_run(phone.clone())).unwrap().is_none());
    let s = stop(&flow);
    assert!(
        matches!(&s.kind, StopKind::DryRunRefused(Error::Phone(why)) if why.contains("larger than"))
    );
    assert_eq!(s.retry, Retry::StopsAgain);
    footer_is_honest(s);
    assert!(phone.log().iter().all(|c| c.starts_with("getvar:")));

    // The cable pulled: the step stays open, to try again
    struct Dead;
    impl Transport for Dead {
        async fn send(&mut self, _: &[u8]) -> Result<usize, Error> {
            Err(Error::Transport("the phone left the bus".into()))
        }
        async fn recv(&mut self) -> Result<Vec<u8>, Error> {
            Err(Error::Transport("the phone left the bus".into()))
        }
    }
    let phone = self::phone("ready");
    let mut flow = flow_at(&bundle, &phone, "dry-run");
    assert!(matches!(
        block_on(flow.dry_run(Dead)),
        Err(Error::Transport(_))
    ));
    assert_eq!(flow.screen().state, "dry-run");
    let mut flow = flow_at(&bundle, &phone, "phone");
    assert!(matches!(
        block_on(flow.read_phone(&mut Dead, &id())),
        Err(Error::Transport(_))
    ));
    assert_eq!(flow.screen().state, "phone");
}

#[test]
fn the_owners_answers_follow_install_phones_rules() {
    let good = AnswersInput {
        username: "alex",
        password: "correct horse",
        password_again: "correct horse",
        hostname: "",
        timezone: "America/New_York",
        wifi_ssid: "<SSID>",
        wifi_psk: "eight chars",
    };
    let a = OwnerAnswers::check(&good).unwrap();
    assert_eq!(a.hostname, "alex-phone");
    let s = a.summary();
    assert!(!s.pin && s.wifi);
    // Neither secret nor the network's name in debug output
    let debug = format!("{a:?} {s:?}");
    assert!(
        !debug.contains("correct horse")
            && !debug.contains("eight chars")
            && !debug.contains("<SSID>")
    );

    let fields = |input: AnswersInput<'_>| -> Vec<AnswerField> {
        OwnerAnswers::check(&input)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.field)
            .collect()
    };
    for bad in [
        "",
        "Alex",
        "1alex",
        "al ex",
        "root",
        "admin",
        "systemd-network",
        "a-name-that-is-longer-than-thirty-two",
    ] {
        assert_eq!(
            fields(AnswersInput {
                username: bad,
                hostname: "phone",
                ..good.clone()
            }),
            [AnswerField::Username],
            "{bad}"
        );
    }
    assert!(OwnerAnswers::check(&AnswersInput {
        username: "_al-ex_9",
        hostname: "phone",
        ..good.clone()
    })
    .is_ok());
    assert_eq!(
        fields(AnswersInput {
            password: "",
            password_again: "",
            ..good.clone()
        }),
        [AnswerField::Password]
    );
    assert_eq!(
        fields(AnswersInput {
            password_again: "correct horses",
            ..good.clone()
        }),
        [AnswerField::Password]
    );
    assert_eq!(
        fields(AnswersInput {
            password: "a\nb",
            password_again: "a\nb",
            ..good.clone()
        }),
        [AnswerField::Password]
    );
    for bad in ["-phone", "phone-", "my phone", "my_phone", &"x".repeat(64)] {
        assert_eq!(
            fields(AnswersInput {
                hostname: bad,
                ..good.clone()
            }),
            [AnswerField::Hostname],
            "{bad}"
        );
    }
    // The default name of a user with an underscore is not a host name
    assert_eq!(
        fields(AnswersInput {
            username: "al_ex",
            ..good.clone()
        }),
        [AnswerField::Hostname]
    );
    for bad in [
        "",
        "Europe/",
        "/Paris",
        "Europe//Paris",
        "../etc/passwd",
        "Europe/Par is",
    ] {
        assert_eq!(
            fields(AnswersInput {
                timezone: bad,
                ..good.clone()
            }),
            [AnswerField::Timezone],
            "{bad}"
        );
    }
    assert_eq!(
        fields(AnswersInput {
            wifi_ssid: &"n".repeat(33),
            ..good.clone()
        }),
        [AnswerField::WifiSsid]
    );
    assert_eq!(
        fields(AnswersInput {
            wifi_ssid: "",
            ..good.clone()
        }),
        [AnswerField::WifiSsid]
    );
    for bad in ["short", &"p".repeat(64), "tab\there"] {
        assert_eq!(
            fields(AnswersInput {
                wifi_psk: bad,
                ..good.clone()
            }),
            [AnswerField::WifiPsk],
            "{bad}"
        );
    }
    // An open network, a 64-digit key, and no Wi-Fi at all
    assert!(OwnerAnswers::check(&AnswersInput {
        wifi_psk: "",
        ..good.clone()
    })
    .unwrap()
    .wifi
    .unwrap()
    .1
    .is_none());
    assert!(OwnerAnswers::check(&AnswersInput {
        wifi_psk: &"0f".repeat(32),
        ..good.clone()
    })
    .is_ok());
    assert!(OwnerAnswers::check(&AnswersInput {
        wifi_ssid: "",
        wifi_psk: "",
        ..good.clone()
    })
    .unwrap()
    .wifi
    .is_none());
    // Every problem at once, for a form
    assert_eq!(
        fields(AnswersInput {
            username: "Alex",
            password: "",
            password_again: "",
            hostname: "",
            timezone: "",
            wifi_ssid: "",
            wifi_psk: ""
        }),
        [
            AnswerField::Username,
            AnswerField::Password,
            AnswerField::Timezone
        ]
    );
}

#[test]
fn a_screen_is_plain_data_for_any_front_end() {
    let bundle = TestBundle::new();
    let phone = phone("ready");
    let flow = flow_at(&bundle, &phone, "factory-image");
    let v: Value = serde_json::to_value(flow.screen()).unwrap();
    assert_eq!(v["state"], "factory-image");
    assert_eq!(v["action"], "take-file");
    assert_eq!(v["file"]["name"], json!(bundle.factory_zip_name));
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);
    assert!(v["stop"].is_null());
    // The page to open is Google's, from the signed index, and never the download itself
    assert_eq!(
        v["link"],
        "https://developers.google.com/android/images#cheetah"
    );
    assert!(!v.to_string().contains("dl.google.com"));
    // Each state names itself and what the front end does next
    let mut seen = Vec::new();
    let mut flow = begin(&bundle);
    for _ in 0..5 {
        let s = flow.screen();
        seen.push((s.state, s.action));
        match s.state {
            "warning" => flow.acknowledge_warning().unwrap(),
            "phone" => look(&mut flow, &phone).unwrap(),
            "factory-image" => {
                give(&mut flow, &bundle.factory_zip_name, &bundle.factory_zip).unwrap()
            }
            "answers" => flow.answers(&answers()).unwrap(),
            _ => {}
        }
    }
    assert_eq!(
        seen,
        [
            ("warning", "acknowledge"),
            ("phone", "read-phone"),
            ("factory-image", "take-file"),
            ("answers", "answers"),
            ("dry-run", "dry-run")
        ]
    );
}
