// SPDX-License-Identifier: MIT
//! What only this module can reach: the backstops in `begin_write`.
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
        .expect("a session");
    assert!(!session.is_dry_run());
    assert_eq!(session.may_flash().len(), 6);
    assert!(phone.changes().is_empty());
}
