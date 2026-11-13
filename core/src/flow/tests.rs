// SPDX-License-Identifier: MIT
//! What only this module can reach: the backstops in `begin_write`, and
//! the session that writes, which takes a [`WriteToken`] that nothing
//! outside this module can make.
//!
//! `begin_write` checks every condition of a write again, whatever the
//! steps before it did. Two of those conditions cannot be taken away
//! through the flow's public steps: `Step::Write` is only ever reached
//! through `confirm_install` with the right codename, which sets
//! `confirmed`, and `Step::Confirm` only in a flow begun with
//! `FlowOptions::write`. So the tests in `core/tests` cannot show that
//! `begin_write` itself holds them. These tests take a real flow to the
//! write step, against the fake phone, then change the one private field
//! and call `begin_write`: it must refuse, and not one command, not even a
//! question, may reach the phone.
#[path = "../../tests/support/mod.rs"]
mod support;

use super::*;
use pollster::block_on;
use support::bundle::TestBundle;
use support::drive::drive;
use support::fake::FakePhone;

/// A flow that writes, with the codename typed: at the write step.
fn at_the_write(bundle: &TestBundle, phone: &FakePhone) -> Flow {
    let options = FlowOptions {
        write: true,
        ..Default::default()
    };
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, bundle, phone, "cheetah", "write");
    assert!(matches!(flow.step, Step::Write(_)), "{:?}", flow.step);
    assert!(flow.confirmed && flow.options.write);
    flow
}

/// `begin_write` refused with `Error::Refused` saying `why`, sent the
/// phone nothing at all, and left the flow where it was.
fn refused_with_nothing_sent(mut flow: Flow, phone: &FakePhone, why: &str) {
    let sent = phone.log().len();
    let err = block_on(flow.begin_write(phone.clone()))
        .map(|session| session.map(|s| s.may_flash().to_vec()))
        .expect_err("begin_write opened a session, or stopped, where it must refuse");
    assert!(
        matches!(&err, Error::Refused(said) if said.contains(why)),
        "{err}"
    );
    assert_eq!(phone.log().len(), sent, "{:?}", &phone.log()[sent..]);
    assert!(phone.changes().is_empty() && phone.written().is_empty());
    assert!(matches!(flow.step, Step::Write(_)) && !flow.write_open);
}

#[test]
fn begin_write_itself_refuses_a_run_in_which_the_codename_was_not_typed() {
    let bundle = TestBundle::new();
    let phone = FakePhone::scenario("ready").unwrap();
    let mut flow = at_the_write(&bundle, &phone);
    flow.confirmed = false;
    refused_with_nothing_sent(flow, &phone, "codename was not typed");
}

#[test]
fn begin_write_itself_refuses_a_flow_that_was_not_begun_to_write() {
    let bundle = TestBundle::new();
    let phone = FakePhone::scenario("ready").unwrap();
    let mut flow = at_the_write(&bundle, &phone);
    flow.options.write = false;
    refused_with_nothing_sent(flow, &phone, "this run is a dry run");
}

/// The control for the two above: the same flow, with nothing changed,
/// does open the session that writes.
#[test]
fn begin_write_opens_the_writing_session_when_every_condition_holds() {
    let bundle = TestBundle::new();
    let phone = FakePhone::scenario("ready").unwrap();
    let mut flow = at_the_write(&bundle, &phone);
    let session = block_on(flow.begin_write(phone.clone()))
        .unwrap()
        .unwrap_or_else(|| panic!("a session: {:?}", flow.step));
    assert!(!session.is_dry_run());
    assert_eq!(session.may_flash().len(), 6);
    assert!(phone.changes().is_empty());
}

/// The session that writes refuses the maintainer's override whole, before
/// a question is asked of the phone, whatever the phone would have said.
/// `begin_write` never passes it; this is the session's own refusal, which
/// only a holder of a token can reach.
#[test]
fn the_session_that_writes_refuses_the_maintainers_override_before_anything_is_sent() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    for scenario in ["one-slot", "ready"] {
        let phone = FakePhone::scenario(scenario).unwrap();
        let options = PhoneOptions {
            maintainer_skip_bootloader_slot_check: true,
            ..Default::default()
        };
        let err = block_on(Session::connect_writing(
            phone.clone(),
            &release,
            &support::drive::id(),
            options,
            WriteToken(()),
        ))
        .unwrap_err();
        assert_eq!(err, Error::OverrideOutsideDryRun, "{scenario}");
        assert!(phone.log().is_empty(), "{scenario}: {:?}", phone.log());
    }
}

/// A session writes if and only if it holds a token: with the same options
/// on the same phone, the one opened without is a dry run with an empty
/// flash list, and sends no write.
#[test]
fn a_session_writes_only_with_a_token() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let id = support::drive::id();
    let phone = FakePhone::scenario("ready").unwrap();
    let options = PhoneOptions::default();
    let mut dry = block_on(Session::connect_pinned(
        phone.clone(),
        &release,
        &id,
        options.clone(),
    ))
    .unwrap();
    assert!(dry.is_dry_run() && dry.may_flash().is_empty());
    block_on(support::drive::walk(&mut dry, &bundle, &release)).unwrap();
    assert!(phone.not_reads().is_empty(), "{:?}", phone.not_reads());
    assert!(phone.written().is_empty());

    let mut writing = block_on(Session::connect_writing(
        phone.clone(),
        &release,
        &id,
        options,
        WriteToken(()),
    ))
    .unwrap();
    assert!(!writing.is_dry_run());
    assert_eq!(writing.may_flash().len(), 6);
    writing
        .set_answers(
            crate::firstboot::root_answers(&release, &support::drive::answers(), "testsalt0123")
                .unwrap(),
        )
        .unwrap();
    block_on(support::drive::walk(&mut writing, &bundle, &release)).unwrap();
    assert_eq!(phone.written().len(), 6);
}

/// A session that writes takes the root image only with the owner's
/// answers, and only from a release whose image keeps a place for them: it
/// refuses before its first write, so the phone gets nothing at all. A dry
/// run walks without them.
#[test]
fn the_session_that_writes_refuses_userdata_without_the_answers() {
    let id = support::drive::id();
    let with_places = TestBundle::new();
    let without = TestBundle::build(
        |m| {
            m["root_image"]
                .as_object_mut()
                .unwrap()
                .remove("placeholders");
        },
        |c| c,
    );
    for (bundle, why) in [
        (&with_places, "the owner's answers were not given"),
        (&without, "keeps no place for the owner's answers"),
    ] {
        let release = bundle.verify().unwrap();
        let phone = FakePhone::scenario("ready").unwrap();
        let mut writing = block_on(Session::connect_writing(
            phone.clone(),
            &release,
            &id,
            PhoneOptions::default(),
            WriteToken(()),
        ))
        .unwrap();
        let err = block_on(writing.preflight_userdata(bundle.root_raw.len() as u64)).unwrap_err();
        assert!(
            matches!(&err, Error::Refused(s) if s.contains(why)),
            "{err}"
        );
        let err = block_on(support::drive::walk(&mut writing, bundle, &release)).unwrap_err();
        assert!(
            matches!(&err, Error::Refused(s) if s.contains(why)),
            "{err}"
        );
        // Not even the first boot image is written
        assert!(phone.written().is_empty(), "{:?}", phone.written());
        assert!(phone.changes().is_empty());

        let phone = FakePhone::scenario("ready").unwrap();
        let mut dry = block_on(Session::connect_pinned(
            phone.clone(),
            &release,
            &id,
            PhoneOptions::default(),
        ))
        .unwrap();
        block_on(support::drive::walk(&mut dry, bundle, &release)).unwrap();
        assert!(phone.written().is_empty());
    }
}
