// SPDX-License-Identifier: MIT
//! A front end for the tests: it does what the flow's screen asks, against
//! the fake phone. Shared by the write path's tests, the session's tests
//! (the flow's `begin_write` is the only way to a session that writes) and
//! the unit tests in `core/src/flow/tests.rs`.
#![allow(dead_code)]
use super::bundle::TestBundle;
use super::fake::{FakePhone, SERIAL};
use omamobile_installer_core::bundle::VerifiedRelease;
use omamobile_installer_core::fastboot::{PhoneId, Session, Transport};
use omamobile_installer_core::flow::{AnswersInput, Flow, FlowOptions, OwnerAnswers};
use omamobile_installer_core::install::flash_userdata_from_reader;
use omamobile_installer_core::Error;
use pollster::block_on;

pub fn id() -> PhoneId {
    PhoneId::of_serial(SERIAL).unwrap()
}

pub fn answers() -> OwnerAnswers {
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

/// The release's images through a session, as a front end sends them.
pub async fn walk<T: Transport>(
    session: &mut Session<T>,
    bundle: &TestBundle,
    release: &VerifiedRelease,
) -> Result<(), Error> {
    for write in &release.manifest().boot_writes {
        let image = release.verify_boot_image(write, bundle.image(&write.image))?;
        session.flash_boot(&image).await?;
    }
    let mut check = release.root_image_check();
    check.update(&bundle.root_zst);
    let root = check.finish()?;
    flash_userdata_from_reader(session, &root, &bundle.root_zst[..], None).await?;
    session.activate_and_reboot().await
}

/// Do what the flow's screen asks, as a front end does, until `until` is
/// the state on the screen or the flow has nothing more to ask. `typed` is
/// what the owner types at the confirmation. `hook` runs before each step,
/// with the state about to be acted on.
pub fn drive_with(
    flow: &mut Flow,
    bundle: &TestBundle,
    phone: &FakePhone,
    typed: &str,
    until: &str,
    mut hook: impl FnMut(&str),
) {
    loop {
        let screen = flow.screen();
        if screen.state == until {
            return;
        }
        hook(screen.state);
        match (screen.state, screen.action) {
            (_, "acknowledge") => flow.acknowledge_warning().unwrap(),
            // A locked phone stays locked here: nobody unlocks it
            ("unlock", _) => return,
            // What slot B holds is the test's to answer
            ("slot-b", _) => return,
            (_, "read-phone") => block_on(flow.read_phone(&mut phone.clone(), &id())).unwrap(),
            (_, "accept-terms") => flow.decline_google_terms().unwrap(),
            (_, "take-file") => {
                let size = bundle.factory_zip.len() as u64;
                let mut check = flow
                    .factory_zip(&bundle.factory_zip_name, size)
                    .unwrap()
                    .unwrap();
                check.update(&bundle.factory_zip);
                flow.factory_zip_done(check).unwrap();
            }
            (_, "answers") => flow.answers(&answers()).unwrap(),
            (_, "dry-run") => {
                let release = flow.release().unwrap().clone();
                block_on(async {
                    if let Some(mut session) = flow.dry_run(phone.clone()).await.unwrap() {
                        match walk(&mut session, bundle, &release).await {
                            Ok(()) => {
                                flow.dry_run_done(session).unwrap();
                            }
                            Err(e) => flow.dry_run_failed(&e).unwrap(),
                        }
                    }
                });
            }
            (_, "confirm") => flow.confirm_install(typed).unwrap(),
            (_, "write") => {
                let release = flow.release().unwrap().clone();
                block_on(async {
                    if let Some(mut session) = flow.begin_write(phone.clone()).await.unwrap() {
                        match walk(&mut session, bundle, &release).await {
                            Ok(()) => {
                                flow.write_done(session).unwrap();
                            }
                            Err(e) => {
                                flow.write_failed(session, &e).unwrap();
                            }
                        }
                    }
                });
            }
            _ => return,
        }
    }
}

pub fn drive(flow: &mut Flow, bundle: &TestBundle, phone: &FakePhone, typed: &str, until: &str) {
    drive_with(flow, bundle, phone, typed, until, |_| {});
}

/// A flow begun with `options` (which must say `write`), taken through
/// every step on this phone up to and including `Flow::begin_write`: the
/// flow, and the one session that can write. Panics if the flow stops on
/// the way.
pub fn writing_session(
    bundle: &TestBundle,
    phone: &FakePhone,
    options: FlowOptions,
) -> (Flow, Session<FakePhone>) {
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, bundle, phone, "cheetah", "write");
    assert_eq!(flow.screen().state, "write", "{:?}", flow.step());
    let session = block_on(flow.begin_write(phone.clone()))
        .unwrap()
        .unwrap_or_else(|| panic!("{:?}", flow.step()));
    (flow, session)
}
