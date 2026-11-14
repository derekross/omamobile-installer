// SPDX-License-Identifier: MIT
//! A first install's measurement of the other bootloader slot
//! (docs/DESIGN.md §6, "Measuring the other slot"), against the pretend
//! phone: a Pixel 7 Pro from stock Android, which tells the version of the
//! bootloader that runs only, with two slots, their versions and flags, and
//! each way a restart into the bootloader can end. No USB.
//!
//! Every test reads the pretend phone's record of what it was sent, and
//! every way the measurement can fail ends with slot A the active slot:
//! at once where the phone still answered, or after the next run's
//! putting back where it did not.
mod support;
use omamobile_installer_core::fastboot::GET_UNLOCK_ABILITY;
use omamobile_installer_core::flow::{
    Flow, FlowOptions, PhoneFact, ProbeAnswer, Retry, Step, Stop, StopKind, OTHER_SLOT_MEASURED,
};
use omamobile_installer_core::slot_probe::{
    Active, ProbeNext, ProbeRecord, ProbeTrouble, Restart, SlotProbe,
};
use pollster::block_on;
use support::bundle::TestBundle;
use support::drive::{drive, id};
use support::fake::{FakePhone, FakeRestart};

const OURS: [&str; 6] = [
    "boot_b",
    "init_boot_b",
    "vendor_boot_b",
    "vendor_kernel_boot_b",
    "dtbo_b",
    "userdata",
];

/// The commands of a measurement that reads slot B and comes back, in order.
const ROUND_TRIP: [&str; 4] = [
    "set_active:b",
    "reboot-bootloader",
    "set_active:a",
    "reboot-bootloader",
];

/// A release whose descriptor lets the installer measure the other slot.
fn probing_release() -> TestBundle {
    TestBundle::build(|_| {}, |c| c + "BOOTLOADER_SLOT_PROBE=yes\n")
}

fn options() -> FlowOptions {
    FlowOptions {
        write: true,
        can_probe_slots: true,
        ..Default::default()
    }
}

fn phone(name: &str) -> FakePhone {
    FakePhone::scenario(name).unwrap_or_else(|| panic!("no pretend phone {name}"))
}

fn stop(flow: &Flow) -> &Stop {
    match flow.step() {
        Step::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}"),
    }
}

/// A flow begun with `options`, taken to the question whether to measure.
fn at_question(bundle: &TestBundle, phone: &FakePhone, options: FlowOptions) -> Flow {
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, bundle, phone, "cheetah", "slot-probe");
    flow
}

/// What a front end does while the phone restarts: watch the bus as time
/// passes, as the tool's USB loop does, within the core's limits.
fn watch(phone: &FakePhone, leave_within: u32, back_within: u32) -> Restart {
    let mut gone = false;
    for _ in 0..leave_within {
        if !phone.listed() {
            gone = true;
            break;
        }
        phone.tick();
    }
    if !gone {
        return Restart::NeverLeft;
    }
    for _ in 0..back_within {
        phone.tick();
        if phone.listed() {
            return Restart::LeftAndBack;
        }
    }
    Restart::NotBack
}

/// The measurement run as a front end runs it; each record it asks to keep
/// is kept, as the front end writes it after each step.
fn run(probe: &mut SlotProbe, phone: &FakePhone, kept: &mut Option<String>) {
    loop {
        let next = block_on(probe.step(&mut phone.clone()));
        *kept = probe.record().map(|r| r.to_text());
        match next {
            ProbeNext::Finished => return,
            ProbeNext::Restart {
                leave_within,
                back_within,
                note,
                ..
            } => {
                assert!(note.contains("Leave it connected"), "{note}");
                let seen = watch(phone, leave_within, back_within);
                probe.restart_seen(seen).unwrap();
                *kept = probe.record().map(|r| r.to_text());
            }
        }
    }
}

/// Measure, from the question, and hand the measurement back.
fn measure(flow: &mut Flow, phone: &FakePhone) -> Option<String> {
    assert_eq!(flow.screen().state, "slot-probe", "{:?}", flow.step());
    let mut probe = flow.slot_probe(ProbeAnswer::Measure).unwrap().unwrap();
    // Nothing was sent by the answer itself
    let before = phone.log().len();
    assert_eq!(flow.screen().state, "slot-probe-running");
    assert_eq!(phone.log().len(), before);
    let mut kept = None;
    run(&mut probe, phone, &mut kept);
    flow.slot_probe_done(probe).unwrap();
    assert_eq!(
        flow.slot_probe_record().map(|r| r.to_text()),
        kept,
        "the flow keeps what the measurement said last"
    );
    kept
}

/// The commands the phone received that are not questions.
fn not_reads(phone: &FakePhone) -> Vec<String> {
    phone.not_reads()
}

fn probe_stop(flow: &Flow) -> (ProbeTrouble, Active) {
    match &stop(flow).kind {
        StopKind::SlotProbeStopped { why, active } => (why.clone(), *active),
        other => panic!("not a measurement's stop: {other:?}"),
    }
}

/// A new run of the installer, with what the last one kept, to the putting
/// back of slot A, which it does; then to the next question.
fn put_back(bundle: &TestBundle, phone: &FakePhone, kept: Option<String>) -> Flow {
    assert!(
        kept.is_some(),
        "a run that left slot B active keeps a record"
    );
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions {
            slot_probe_record: kept,
            ..options()
        },
    );
    flow.acknowledge_warning().unwrap();
    block_on(flow.read_phone(&mut phone.clone(), &id())).unwrap();
    assert_eq!(
        flow.screen().state,
        "slot-probe-put-back",
        "{:?}",
        flow.step()
    );
    let screen = flow.screen();
    let answers: Vec<&str> = screen.choices.iter().map(|c| c.answer).collect();
    assert_eq!(answers, ["put-back", "stop"]);
    let mut probe = flow.slot_probe(ProbeAnswer::PutBack).unwrap().unwrap();
    let mut kept = None;
    run(&mut probe, phone, &mut kept);
    flow.slot_probe_done(probe).unwrap();
    assert_eq!(kept, None);
    assert_eq!(flow.slot_probe_record(), None);
    // The next look judges the phone afresh
    assert_eq!(flow.screen().state, "phone");
    block_on(flow.read_phone(&mut phone.clone(), &id())).unwrap();
    flow
}

#[test]
fn a_first_install_measures_slot_b_and_goes_on_to_the_write() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    // The question, in the core's words: what it does, the two restarts,
    // that nothing is written, the risk and the way back first
    let screen = flow.screen();
    assert_eq!(screen.action, "slot-probe");
    let text = screen.body.join(" ");
    assert!(
        text.contains("restarts into its bootloader twice"),
        "{text}"
    );
    assert!(text.contains("Nothing is written to the phone's partitions"));
    assert!(text.contains("before its version is known"));
    let steps: Vec<String> = screen.steps.iter().map(|s| s.text.clone()).collect();
    assert!(steps.join(" ").contains("start the installer again"));
    assert_eq!(
        screen.steps.last().unwrap().command.as_deref(),
        Some("fastboot --set-active=a && fastboot reboot-bootloader")
    );
    let answers: Vec<&str> = screen.choices.iter().map(|c| c.answer).collect();
    assert_eq!(answers, ["measure", "stop"]);
    // Only questions so far
    assert!(not_reads(&phone).is_empty(), "{:?}", phone.log());

    assert_eq!(measure(&mut flow, &phone), None);
    assert_eq!(not_reads(&phone), ROUND_TRIP);
    assert_eq!(phone.active(), "a");
    assert_eq!(phone.0.borrow().restarts, 2);
    assert_eq!(flow.slot_measured().map(|m| m.version()), Some("fake-1.0"));
    // On to Google's terms, as a phone that tells both copies would be
    assert_eq!(flow.screen().state, "google-terms");
    // The confirmation says how the version was read and what changed
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    let confirm = flow.screen();
    let text = confirm.body.join(" ");
    assert!(text.contains(&format!(
        "Bootloader fake-1.0 on both copies: {OTHER_SLOT_MEASURED}"
    )));
    assert!(text.contains("slot-successful:a: yes -> no"), "{text}");
    assert!(confirm
        .footer
        .join(" ")
        .contains("slot flags were changed by the measurement"));
    assert!(confirm.notice.is_none(), "nothing rests on anyone's word");
    // And the install writes as on any phone, ending in set_active:b
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert_eq!(flow.screen().state, "installed", "{:?}", flow.step());
    let mut ours = OURS.to_vec();
    ours.sort();
    assert_eq!(phone.written(), ours);
    let mut changes = phone.changes();
    assert_eq!(changes.drain(..4).collect::<Vec<_>>(), ROUND_TRIP);
    assert_eq!(changes.last().map(String::as_str), Some("reboot"));
    assert_eq!(changes[changes.len() - 2], "set_active:b");
    assert_eq!(phone.active(), "b");
}

#[test]
fn a_locked_phone_is_shown_the_unlock_steps_first_and_measured_after_the_unlock() {
    let bundle = probing_release();
    let phone = phone("first-install-locked");
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        options(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "unlock");
    let screen = flow.screen();
    assert_eq!(screen.state, "unlock");
    let text = screen.body.join(" ");
    assert!(
        text.contains("can only be measured once the bootloader is unlocked"),
        "{text}"
    );
    assert!(text.contains("after the unlock has already erased the phone"));
    // Nothing but questions to a locked phone
    assert_eq!(phone.unlock_questions(), 1);
    assert!(not_reads(&phone).iter().all(|c| c == GET_UNLOCK_ABILITY));
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "yes".into());
    });
    block_on(flow.read_phone(&mut phone.clone(), &id())).unwrap();
    assert_eq!(flow.screen().state, "slot-probe");
    measure(&mut flow, &phone);
    assert_eq!(flow.screen().state, "google-terms");
    // After the unlock a later stop says the slot flags were changed, and
    // that the unlock erased the phone
    flow.decline_google_terms().unwrap();
    let mut check = flow
        .factory_zip(&bundle.factory_zip_name, bundle.factory_zip.len() as u64)
        .unwrap()
        .unwrap();
    check.update(&bundle.factory_zip);
    flow.factory_zip_done(check).unwrap();
    flow.answers(&support::drive::answers(), support::drive::SALT)
        .unwrap();
    assert_eq!(flow.screen().state, "dry-run");
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
    });
    assert!(block_on(flow.dry_run(phone.clone())).unwrap().is_none());
    let s = stop(&flow);
    assert_eq!(s.kind, StopKind::LockedAtDryRun);
    assert_eq!(s.phone, PhoneFact::SlotsMeasured { unlock_shown: true });
    let footer = s.footer().join(" ");
    assert!(
        footer.contains("measured its second bootloader copy"),
        "{footer}"
    );
    assert!(footer.contains("that erased the phone"));
}

#[test]
fn copies_that_differ_stop_with_slot_a_active_and_nothing_written() {
    let bundle = probing_release();
    let phone = phone("first-install-differ");
    let mut flow = at_question(&bundle, &phone, options());
    measure(&mut flow, &phone);
    let s = stop(&flow);
    assert_eq!(
        s.kind,
        StopKind::BootloaderSlotsDiffer {
            a: "fake-1.0".into(),
            b: "fake-0.9".into()
        }
    );
    assert_eq!(s.retry, Retry::StopsAgain);
    assert!(s.message.join(" ").contains("slot B's is fake-0.9"));
    assert_eq!(not_reads(&phone), ROUND_TRIP);
    assert_eq!(
        (phone.active().as_str(), phone.0.borrow().running.as_str()),
        ("a", "a")
    );
    assert!(phone.written().is_empty());
}

#[test]
fn a_bootloader_whose_current_slot_follows_the_flag_is_not_measured() {
    let bundle = probing_release();
    let phone = phone("first-install-follows");
    let mut flow = at_question(&bundle, &phone, options());
    assert_eq!(measure(&mut flow, &phone), None);
    assert_eq!(
        probe_stop(&flow),
        (ProbeTrouble::CurrentFollowsFlag, Active::First)
    );
    // Set back at once, and never restarted
    assert_eq!(not_reads(&phone), ["set_active:b", "set_active:a"]);
    assert_eq!(phone.0.borrow().restarts, 0);
    assert_eq!(phone.active(), "a");
    let s = stop(&flow);
    assert_eq!(s.retry, Retry::StopsAgain);
    assert!(s
        .footer()
        .join(" ")
        .contains("slot A is the active slot again"));
}

#[test]
fn a_phone_that_never_leaves_the_bus_is_set_back_and_not_measured() {
    let bundle = probing_release();
    let phone = phone("first-install-stays");
    let mut flow = at_question(&bundle, &phone, options());
    assert_eq!(measure(&mut flow, &phone), None);
    assert_eq!(probe_stop(&flow), (ProbeTrouble::NeverLeft, Active::First));
    assert_eq!(
        not_reads(&phone),
        ["set_active:b", "reboot-bootloader", "set_active:a"]
    );
    assert_eq!(phone.active(), "a");
    // The version it would have read is the first slot's: none was taken
    assert!(flow.slot_measured().is_none());
}

#[test]
fn a_phone_that_starts_an_os_from_slot_b_is_put_back_by_the_next_run() {
    let bundle = probing_release();
    let phone = phone("first-install-os");
    let mut flow = at_question(&bundle, &phone, options());
    let kept = measure(&mut flow, &phone);
    assert_eq!(
        probe_stop(&flow),
        (ProbeTrouble::NotBack { into: 'b' }, Active::NotKnown)
    );
    // Nothing more was sent once it was gone, and slot B is active
    assert_eq!(not_reads(&phone), ["set_active:b", "reboot-bootloader"]);
    assert_eq!(phone.active(), "b");
    let s = stop(&flow);
    assert_eq!(s.retry, Retry::AfterFix);
    let text = s.message.join(" ");
    assert!(
        text.contains("did not come back from slot B's bootloader")
            || s.title.contains("slot B's bootloader")
    );
    assert!(text.contains("choose nothing"), "{text}");
    assert!(text.contains("fastboot --set-active=a"));
    assert!(s
        .footer()
        .join(" ")
        .contains("slot B may still be the active slot"));
    let record = ProbeRecord::parse(kept.as_deref().unwrap()).unwrap();
    assert_eq!(record.first(), 'a');
    // The owner's buttons bring it to Fastboot Mode, on slot B's bootloader
    phone.owner_buttons();
    assert_eq!(phone.0.borrow().running, "b");
    // A run without the record would take it for a phone on slot B
    let mut blind = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        options(),
    );
    blind.acknowledge_warning().unwrap();
    block_on(blind.read_phone(&mut phone.clone(), &id())).unwrap();
    assert_eq!(stop(&blind).kind, StopKind::RunsFromSlotB);
    // With it, slot A is put back first: one more set_active:a and restart
    phone.edit(|s| s.restart = FakeRestart::Normal);
    let before = not_reads(&phone).len();
    let flow = put_back(&bundle, &phone, kept);
    assert_eq!(
        not_reads(&phone)[before..],
        ["set_active:a", "reboot-bootloader"]
    );
    assert_eq!(
        (phone.active().as_str(), phone.0.borrow().running.as_str()),
        ("a", "a")
    );
    // And the phone is offered the measurement again
    assert_eq!(flow.screen().state, "slot-probe");
}

#[test]
fn a_phone_that_comes_back_on_slot_a_is_not_measured() {
    let bundle = probing_release();
    let phone = phone("first-install-falls-back");
    let mut flow = at_question(&bundle, &phone, options());
    assert_eq!(measure(&mut flow, &phone), None);
    assert_eq!(
        probe_stop(&flow),
        (ProbeTrouble::CameBackOnFirst, Active::First)
    );
    assert_eq!(
        not_reads(&phone),
        ["set_active:b", "reboot-bootloader", "set_active:a"]
    );
    assert_eq!(phone.active(), "a");
    assert!(flow.slot_measured().is_none());
}

#[test]
fn a_link_lost_on_slot_b_leaves_a_record_and_the_next_run_puts_slot_a_back() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    // The cable comes out as set_active:a is sent from slot B's bootloader
    phone.edit(|s| s.unplug_on = Some(("set_active:a".into(), 1)));
    let kept = measure(&mut flow, &phone);
    let (why, active) = probe_stop(&flow);
    assert!(matches!(why, ProbeTrouble::Lost(_)), "{why:?}");
    assert_eq!(active, Active::NotKnown);
    assert_eq!(phone.active(), "b");
    assert!(stop(&flow)
        .message
        .join(" ")
        .contains("offers to put slot A back"));
    phone.replug();
    let before = not_reads(&phone).len();
    put_back(&bundle, &phone, kept);
    assert_eq!(
        not_reads(&phone)[before..],
        ["set_active:a", "reboot-bootloader"]
    );
    assert_eq!(
        (phone.active().as_str(), phone.0.borrow().running.as_str()),
        ("a", "a")
    );
}

#[test]
fn a_link_lost_before_the_first_restart_leaves_a_record_that_sets_the_flag_back() {
    // The cable comes out as reboot-bootloader is sent: slot B is the active
    // flag and slot A's bootloader still runs
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    phone.edit(|s| s.unplug_on = Some(("reboot-bootloader".into(), 1)));
    let kept = measure(&mut flow, &phone);
    assert_eq!(probe_stop(&flow).1, Active::NotKnown);
    assert_eq!(
        (phone.active().as_str(), phone.0.borrow().running.as_str()),
        ("b", "a")
    );
    phone.replug();
    // The phone runs slot A's bootloader, and the flag is set back all the
    // same, with no restart
    let before = not_reads(&phone).len();
    put_back(&bundle, &phone, kept);
    assert_eq!(not_reads(&phone)[before..], ["set_active:a"]);
    assert_eq!(phone.active(), "a");
}

#[test]
fn a_phone_that_answers_too_late_on_slot_b_is_not_measured() {
    // A timeout: the phone took the command and its answer never came
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    let at = phone.log().len() + 7 + 3;
    // The 4th command on slot B's bootloader is getvar:all
    phone.edit(|s| s.lose_answer_at = Some(at));
    let kept = measure(&mut flow, &phone);
    let (why, active) = probe_stop(&flow);
    assert!(matches!(why, ProbeTrouble::Lost(_)), "{why:?}");
    assert_eq!(active, Active::NotKnown);
    phone.replug();
    put_back(&bundle, &phone, kept);
    assert_eq!(phone.active(), "a");
}

#[test]
fn another_phone_after_the_restart_is_sent_nothing_more() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    // The first stretch sends seven commands; from the eighth on another
    // phone answers
    let at = phone.log().len() + 7;
    phone.edit(|s| s.swap_serial_after = Some(at));
    let kept = measure(&mut flow, &phone);
    assert_eq!(
        probe_stop(&flow),
        (ProbeTrouble::OtherPhone, Active::NotKnown)
    );
    assert_eq!(
        phone.log().last().map(String::as_str),
        Some("getvar:serialno")
    );
    assert_eq!(not_reads(&phone), ["set_active:b", "reboot-bootloader"]);
    // The right phone, again
    phone.edit(|s| s.swap_serial_after = None);
    put_back(&bundle, &phone, kept);
    assert_eq!(phone.active(), "a");
}

#[test]
fn a_phone_that_refuses_slot_b_is_set_back_at_once() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    phone.edit(|s| s.fail_on = vec!["set_active:b".into()]);
    measure(&mut flow, &phone);
    let (why, active) = probe_stop(&flow);
    assert!(matches!(why, ProbeTrouble::SetActiveRefused(_)), "{why:?}");
    assert_eq!(active, Active::First);
    assert_eq!(not_reads(&phone), ["set_active:b", "set_active:a"]);
    assert_eq!(phone.active(), "a");
}

#[test]
fn a_phone_that_refuses_slot_a_on_slot_b_gets_the_command_and_the_next_run() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    phone.edit(|s| s.fail_on = vec!["set_active:a".into()]);
    let kept = measure(&mut flow, &phone);
    let (why, active) = probe_stop(&flow);
    assert!(matches!(why, ProbeTrouble::PutBackRefused(_)), "{why:?}");
    assert_eq!(active, Active::NotKnown);
    let text = stop(&flow).message.join(" ");
    assert!(text.contains("fastboot --set-active=a"), "{text}");
    assert_eq!(phone.active(), "b");
    phone.edit(|s| s.fail_on.clear());
    put_back(&bundle, &phone, kept);
    assert_eq!(phone.active(), "a");
}

#[test]
fn a_slot_a_that_reads_unstartable_afterwards_stops_the_install() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    // A bootloader that leaves slot A without tries once it was set again
    phone.edit(|s| {
        s.vars.insert("slot-successful:a".into(), "no".into());
    });
    let mut probe = flow.slot_probe(ProbeAnswer::Measure).unwrap().unwrap();
    loop {
        match block_on(probe.step(&mut phone.clone())) {
            ProbeNext::Finished => break,
            ProbeNext::Restart {
                into,
                leave_within,
                back_within,
                ..
            } => {
                let seen = watch(&phone, leave_within, back_within);
                if into == 'a' {
                    phone.edit(|s| {
                        s.vars.insert("slot-retry-count:a".into(), "0".into());
                    });
                }
                probe.restart_seen(seen).unwrap();
            }
        }
    }
    flow.slot_probe_done(probe).unwrap();
    let (why, active) = probe_stop(&flow);
    assert!(matches!(why, ProbeTrouble::FirstNotStartable(_)), "{why:?}");
    assert_eq!(active, Active::First);
    assert_eq!(phone.active(), "a");
    assert!(phone.written().is_empty());
}

#[test]
fn a_slot_b_that_reads_as_started_is_never_measured() {
    let bundle = probing_release();
    let phone = phone("first-install-b-started");
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        options(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    let s = stop(&flow);
    assert_eq!(
        s.kind,
        StopKind::BootloaderSlotsUnknown {
            probe_declared: true
        }
    );
    assert!(s
        .message
        .join(" ")
        .contains("reads as a slot that has started"));
    assert!(not_reads(&phone).is_empty());
}

#[test]
fn declining_sends_nothing_and_stops() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    assert!(flow.slot_probe(ProbeAnswer::Stop).unwrap().is_none());
    let s = stop(&flow);
    assert_eq!(s.kind, StopKind::SlotProbeDeclined);
    assert_eq!(s.retry, Retry::Whenever);
    assert_eq!(s.phone, PhoneFact::OnlyRead);
    assert!(not_reads(&phone).is_empty());
    // The answer of the other question is not this one's
    let mut flow = at_question(&bundle, &phone, options());
    assert!(flow.slot_probe(ProbeAnswer::PutBack).is_err());
    assert_eq!(flow.screen().state, "slot-probe");
}

#[test]
fn only_a_run_that_installs_in_a_front_end_that_can_restart_measures() {
    let phone_ = || phone("first-install");
    for (bundle, opts, declared, words) in [
        // A dry run never measures: a measurement writes the slot flags
        (
            probing_release(),
            FlowOptions {
                write: false,
                ..options()
            },
            true,
            "so a dry run does not do it",
        ),
        // A front end that can't do the restarts (the page that ships)
        (
            probing_release(),
            FlowOptions {
                can_probe_slots: false,
                ..options()
            },
            true,
            "so a dry run does not do it",
        ),
        // A release whose descriptor does not allow it: as before
        (
            TestBundle::new(),
            options(),
            false,
            "does not do that to this phone model yet",
        ),
    ] {
        let phone = phone_();
        let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), opts);
        drive(&mut flow, &bundle, &phone, "cheetah", "");
        let s = stop(&flow);
        assert_eq!(
            s.kind,
            StopKind::BootloaderSlotsUnknown {
                probe_declared: declared
            }
        );
        assert!(s.message.join(" ").contains(words), "{:?}", s.message);
        assert!(not_reads(&phone).is_empty());
    }
    // A reinstall is not measured either
    let bundle = probing_release();
    let phone = phone_();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions {
            reinstall: true,
            ..options()
        },
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "");
    assert!(matches!(
        stop(&flow).kind,
        StopKind::BootloaderSlotsUnknown { .. }
    ));
    assert!(not_reads(&phone).is_empty());
}

#[test]
fn a_measurement_is_handed_back_only_when_it_has_finished() {
    let bundle = probing_release();
    let phone = phone("first-install");
    let mut flow = at_question(&bundle, &phone, options());
    let mut probe = flow.slot_probe(ProbeAnswer::Measure).unwrap().unwrap();
    // A restart seen before any was asked for is refused
    assert!(probe.restart_seen(Restart::LeftAndBack).is_err());
    let next = block_on(probe.step(&mut phone.clone()));
    assert!(matches!(next, ProbeNext::Restart { into: 'b', .. }));
    // Handed back mid-way: refused, and the flow still waits for it
    let flow_err = flow.slot_probe_done(probe);
    assert!(flow_err.is_err());
    assert_eq!(flow.screen().state, "slot-probe-running");
}

#[test]
fn a_record_is_only_what_the_measurement_wrote() {
    for bad in [
        "",
        "omamobile-slot-probe 1\nphone abc\nfirst a\n",
        "omamobile-slot-probe 2\nphone 0000000000000000000000000000000000000000000000000000000000000000\nfirst a\n",
        "omamobile-slot-probe 1\nphone 0000000000000000000000000000000000000000000000000000000000000000\nfirst c\n",
        "omamobile-slot-probe 1\nphone 0000000000000000000000000000000000000000000000000000000000000000\nfirst a\nmore\n",
        "omamobile-slot-probe 1\nphone 000000000000000000000000000000000000000000000000000000000000000G\nfirst a\n",
    ] {
        assert!(ProbeRecord::parse(bad).is_none(), "{bad:?}");
    }
    // A record for another phone changes nothing for this one
    let bundle = probing_release();
    let phone = phone("first-install");
    let other = "omamobile-slot-probe 1\nphone 0000000000000000000000000000000000000000000000000000000000000000\nfirst a\n";
    assert!(ProbeRecord::parse(other).is_some());
    let flow = at_question(
        &bundle,
        &phone,
        FlowOptions {
            slot_probe_record: Some(other.into()),
            ..options()
        },
    );
    assert_eq!(flow.screen().state, "slot-probe");
}
