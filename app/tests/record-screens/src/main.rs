// SPDX-License-Identifier: MIT
//! Records the screens of the install flow for the desktop app's tests.
//!
//! It drives the core's flow (core/src/flow.rs) against the core's pretend
//! phone and test bundle (core/tests/support), into every state and every
//! kind of stop, and writes what `Flow::screen()` gives, as serde serialises
//! it, to one JSON file each. The app's stand-in tool replays those files, so
//! the app's tests run with no Rust build and no phone.
//!
//! No USB device is opened and nothing is downloaded: the phone and Google's
//! server are functions in this file and in the core's test support.
#[path = "../../../../core/tests/support/mod.rs"]
mod support;
use omamobile_installer_core::download::{FactoryDownload, FetchFailure, FetchNext, FetchResponse};
use omamobile_installer_core::fastboot::PhoneId;
use omamobile_installer_core::flow::{AnswersInput, Flow, FlowOptions, OwnerAnswers};
use pollster::block_on;
use serde_json::Value;
use std::path::Path;
use support::bundle::TestBundle;
use support::fake::{FakePhone, SERIAL};

const NOW: u64 = 1_794_492_239;

fn dump(dir: &Path, name: &str, flow: &Flow) {
    let s = serde_json::to_string_pretty(&flow.screen()).unwrap();
    std::fs::write(dir.join(format!("{name}.json")), s + "\n").unwrap();
    println!("{name}: {} {:?}", flow.screen().state, flow.screen().stop);
}
fn id() -> PhoneId {
    PhoneId::of_serial(SERIAL).unwrap()
}
fn begin(b: &TestBundle, o: FlowOptions) -> Flow {
    Flow::begin(&b.files(), b.index(), &b.trusted(), o)
}
fn look(flow: &mut Flow, phone: &FakePhone) {
    block_on(flow.read_phone(&mut phone.clone(), &id())).unwrap()
}
fn give(flow: &mut Flow, name: &str, bytes: &[u8]) {
    if let Some(mut check) = flow.factory_zip(name, bytes.len() as u64).unwrap() {
        for chunk in bytes.chunks(7001) {
            check.update(chunk);
        }
        flow.factory_zip_done(check).unwrap();
    }
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
fn to(b: &TestBundle, phone: &FakePhone, o: FlowOptions, until: &str) -> Flow {
    let mut flow = begin(b, o);
    for state in [
        "warning",
        "phone",
        "google-terms",
        "factory-image",
        "answers",
        "dry-run",
    ] {
        assert_eq!(flow.screen().state, state);
        if state == until {
            return flow;
        }
        match state {
            "warning" => flow.acknowledge_warning().unwrap(),
            "phone" => look(&mut flow, phone),
            "google-terms" => flow.decline_google_terms().unwrap(),
            "factory-image" => give(&mut flow, &b.factory_zip_name, &b.factory_zip),
            "answers" => flow.answers(&answers()).unwrap(),
            _ => unreachable!(),
        }
    }
    panic!()
}
fn ok(size: u64) -> FetchResponse<'static> {
    FetchResponse {
        status: 200,
        content_length: Some(size),
        ..Default::default()
    }
}
fn feed(d: &mut FactoryDownload, bytes: &[u8]) -> bool {
    bytes.chunks(4099).all(|c| d.body(c))
}

fn main() {
    let out = std::env::args().nth(1).unwrap();
    let dir = Path::new(&out);
    std::fs::create_dir_all(dir).unwrap();
    let b = TestBundle::new();
    let d = FlowOptions::default;
    let ready = || FakePhone::scenario("ready").unwrap();
    let scen = |n: &str| FakePhone::scenario(n).unwrap();

    // The states
    let mut f = begin(&b, d());
    dump(dir, "warning", &f);
    f.acknowledge_warning().unwrap();
    dump(dir, "phone", &f);
    f.no_phone().unwrap();
    dump(dir, "phone-not-found", &f);
    look(&mut f, &ready());
    dump(dir, "google-terms", &f);
    f.accept_google_terms(NOW).unwrap();
    dump(dir, "factory-image-download", &f);
    // A download that broke off
    let mut dl = f.factory_download(0).unwrap();
    let size = b.factory_zip.len() as u64;
    assert!(matches!(
        dl.response(&ok(size)).unwrap(),
        FetchNext::Body { at: 0 }
    ));
    feed(&mut dl, &b.factory_zip[..20000]);
    f.factory_download_done(dl, Some(FetchFailure::Network("connection reset".into())))
        .unwrap();
    dump(dir, "factory-image-download-interrupted", &f);
    // A file handed in with another name, while the download is on offer
    give(&mut f, "some-other-file.zip", &b.factory_zip);
    dump(dir, "factory-image-download-wrong-file", &f);
    // The download, complete
    let mut dl = f.factory_download(0).unwrap();
    dl.response(&ok(size)).unwrap();
    assert!(feed(&mut dl, &b.factory_zip));
    f.factory_download_done(dl, None).unwrap();
    dump(dir, "answers", &f);
    f.answers(&answers()).unwrap();
    dump(dir, "dry-run", &f);
    let p = ready();
    let session = block_on(f.dry_run(p.clone())).unwrap().unwrap();
    f.dry_run_done(session).unwrap();
    dump(dir, "done", &f);

    // Terms declined, and a server that has no such file
    let mut f = to(&b, &ready(), d(), "google-terms");
    f.decline_google_terms().unwrap();
    dump(dir, "factory-image-take-file", &f);
    give(&mut f, &b.factory_zip_name, &b.factory_zip[..1000]);
    dump(dir, "factory-image-take-file-incomplete", &f);
    let mut f = to(&b, &ready(), d(), "google-terms");
    f.accept_google_terms(NOW).unwrap();
    let mut dl = f.factory_download(0).unwrap();
    dl.response(&FetchResponse {
        status: 404,
        ..Default::default()
    })
    .unwrap();
    f.factory_download_done(dl, None).unwrap();
    dump(dir, "factory-image-unavailable", &f);

    // The unlock steps, for each answer to the unlock question
    for (name, scenario, answer) in [
        ("unlock", "locked", None),
        ("unlock-oem-off", "locked-oem-off", None),
        ("unlock-unknown", "locked", Some("something else")),
    ] {
        let phone = scen(scenario);
        if let Some(a) = answer {
            phone.edit(|s| s.unlock_ability = Some(a.into()));
        }
        let mut f = to(&b, &phone, d(), "phone");
        look(&mut f, &phone);
        dump(dir, name, &f);
    }

    // The maintainer's override: the notice
    let over = || FlowOptions {
        maintainer_skip_bootloader_slot_check: true,
        ..Default::default()
    };
    let phone = scen("one-slot");
    let mut f = to(&b, &phone, over(), "google-terms");
    dump(dir, "google-terms-notice", &f);
    f.decline_google_terms().unwrap();
    give(&mut f, &b.factory_zip_name, &b.factory_zip);
    f.answers(&answers()).unwrap();
    let session = block_on(f.dry_run(phone.clone())).unwrap().unwrap();
    f.dry_run_done(session).unwrap();
    dump(dir, "done-notice", &f);

    // The stops
    let mut bad = b.manifest_json.clone();
    bad[100] ^= 1;
    let files = omamobile_installer_core::bundle::ReleaseFiles {
        manifest_json: &bad,
        ..b.files()
    };
    dump(
        dir,
        "stop-release-not-verified",
        &Flow::begin(&files, b.index(), &b.trusted(), d()),
    );
    let without = TestBundle::with_index(|i| *i = Value::Null);
    dump(
        dir,
        "stop-no-build-index",
        &Flow::begin(&without.files(), None, &without.trusted(), d()),
    );
    dump(
        dir,
        "stop-build-index-refused",
        &Flow::begin(&b.files(), None, &b.trusted(), d()),
    );
    let mut f = begin(&b, d());
    f.decline_warning().unwrap();
    dump(dir, "stop-warning-declined", &f);
    for (name, scenario) in [
        ("stop-wrong-model", "other-model"),
        ("stop-runs-from-slot-b", "slot-b"),
        ("stop-bootloader-slots-unknown", "one-slot"),
        ("stop-bootloader-slots-differ", "slots-differ"),
        ("stop-build-not-listed", "unknown-build"),
        ("stop-build-not-tested", "untested-build"),
        ("stop-build-not-tested-newer-tested", "untested-old-build"),
    ] {
        let phone = scen(scenario);
        let mut f = to(&b, &phone, d(), "phone");
        look(&mut f, &phone);
        dump(dir, name, &f);
    }
    let phone = ready();
    phone.edit(|s| {
        s.vars.remove("version-baseband");
    });
    let mut f = to(&b, &phone, d(), "phone");
    look(&mut f, &phone);
    dump(dir, "stop-baseband-unknown", &f);
    // Another phone after the unlock steps
    let first = scen("locked");
    let mut f = to(&b, &first, d(), "phone");
    look(&mut f, &first);
    let second = ready();
    second.edit(|s| {
        s.vars.insert("serialno".into(), "ANOTHERPHONE0002".into());
    });
    block_on(f.read_phone(
        &mut second.clone(),
        &PhoneId::of_serial("ANOTHERPHONE0002").unwrap(),
    ))
    .unwrap();
    dump(dir, "stop-phone-changed", &f);
    // A download that is not the listed file
    let mut f = to(&b, &ready(), d(), "google-terms");
    f.accept_google_terms(NOW).unwrap();
    let mut dl = f.factory_download(0).unwrap();
    dl.response(&ok(size)).unwrap();
    let mut wrong = b.factory_zip.clone();
    let mid = wrong.len() / 2;
    wrong[mid] ^= 1;
    feed(&mut dl, &wrong);
    f.factory_download_done(dl, None).unwrap();
    dump(dir, "stop-factory-download-mismatch", &f);
    // At the dry run
    let phone = ready();
    let mut f = to(&b, &phone, d(), "dry-run");
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
    });
    assert!(block_on(f.dry_run(phone.clone())).unwrap().is_none());
    dump(dir, "stop-locked-at-dry-run", &f);
    let phone = ready();
    let mut f = to(&b, &phone, d(), "dry-run");
    phone.edit(|s| {
        for (k, v) in [
            ("version-bootloader", "fake-1.1"),
            ("version-bootloader-a", "fake-1.1"),
            ("version-bootloader-b", "fake-1.1"),
            ("version-baseband", "fake-radio-1.1"),
        ] {
            s.vars.insert(k.into(), v.into());
        }
    });
    assert!(block_on(f.dry_run(phone.clone())).unwrap().is_none());
    dump(dir, "stop-build-changed", &f);
    let phone = ready();
    let mut f = to(&b, &phone, d(), "dry-run");
    phone.edit(|s| {
        s.partitions
            .insert("vendor_kernel_boot_b".into(), vec![0; 8 << 10]);
    });
    assert!(block_on(f.dry_run(phone.clone())).unwrap().is_none());
    dump(dir, "stop-dry-run-refused", &f);
}
