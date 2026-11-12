// SPDX-License-Identifier: MIT
//! Whole installs, and refused ones, against the fake phone. No USB.
mod support;
use omamobile_installer_core::bundle::VerifiedRelease;
use omamobile_installer_core::fastboot::{PhoneOptions, Session};
use omamobile_installer_core::install::flash_userdata_from_reader;
use omamobile_installer_core::Error;
use pollster::block_on;
use serde_json::json;
use support::bundle::TestBundle;
use support::fake::{FakePhone, SENTINEL, SERIAL};

const OURS: [&str; 6] = [
    "boot_b",
    "dtbo_b",
    "init_boot_b",
    "userdata",
    "vendor_boot_b",
    "vendor_kernel_boot_b",
];

fn options() -> PhoneOptions {
    PhoneOptions {
        bootloader_slots_confirmed: Some("fake-1.0".into()),
        ..Default::default()
    }
}

/// The whole install, as a front end drives it.
async fn install(
    bundle: &TestBundle,
    release: &VerifiedRelease,
    phone: &FakePhone,
    options: PhoneOptions,
) -> Result<Vec<String>, Error> {
    let mut check = release.root_image_check();
    check.update(&bundle.root_zst);
    let root = check.finish()?;
    let mut session = Session::connect(phone.clone(), release, SERIAL, options).await?;
    for write in &release.manifest().boot_writes {
        let image = release.verify_boot_image(write, bundle.image(&write.image))?;
        session.flash_boot(&image).await?;
    }
    flash_userdata_from_reader(&mut session, &root, &bundle.root_zst[..], None).await?;
    session.activate_and_reboot().await?;
    assert!(session.is_done());
    Ok(session.log().to_vec())
}

fn writes_in(log: &[String]) -> Vec<&String> {
    log.iter().filter(|c| !c.starts_with("getvar:")).collect()
}

#[test]
fn an_install_writes_slot_b_and_userdata_and_nothing_else() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    block_on(install(&bundle, &release, &phone, options())).unwrap();

    assert_eq!(phone.written(), OURS);
    for (name, size, _) in support::bundle::IMAGES {
        let part = phone.partition(&format!("{name}_b"));
        assert!(part[..size] == *bundle.image(name), "{name}_b");
        assert!(part[size..].iter().all(|b| *b == SENTINEL));
    }
    let userdata = phone.partition("userdata");
    assert!(
        userdata[..bundle.root_raw.len()] == bundle.root_raw[..],
        "userdata is not the root image"
    );
    assert!(userdata[bundle.root_raw.len()..]
        .iter()
        .all(|b| *b == SENTINEL));
    let state = phone.0.borrow();
    assert_eq!(state.active_slot, "b");
    assert!(state.rebooted);

    // The phone saw only these commands, in this order
    let log = phone.log();
    let writes = writes_in(&log);
    let flashes: Vec<&str> = writes
        .iter()
        .filter(|c| c.starts_with("flash:"))
        .map(|c| c.as_str())
        .collect();
    assert_eq!(
        flashes[..5],
        [
            "flash:boot_b",
            "flash:init_boot_b",
            "flash:vendor_boot_b",
            "flash:vendor_kernel_boot_b",
            "flash:dtbo_b"
        ]
    );
    // 2 MiB through a 256 KiB max-download-size: several sparse images
    assert!(flashes.len() > 5 + 4 && flashes[5..].iter().all(|c| *c == "flash:userdata"));
    assert_eq!(writes[writes.len() - 2..], ["set_active:b", "reboot"]);
    for c in &log {
        assert!(
            c.starts_with("getvar:")
                || c.starts_with("download:")
                || c.starts_with("flash:")
                || c == "set_active:b"
                || c == "reboot",
            "unexpected command {c}"
        );
    }
    // Every download fit the phone's limit
    for c in log.iter().filter_map(|c| c.strip_prefix("download:")) {
        assert!(usize::from_str_radix(c, 16).unwrap() <= 256 << 10);
    }
}

#[test]
fn a_dry_run_checks_everything_and_writes_nothing() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    let log = block_on(install(
        &bundle,
        &release,
        &phone,
        PhoneOptions {
            dry_run: true,
            ..options()
        },
    ))
    .unwrap();
    assert!(phone.written().is_empty());
    assert!(writes_in(&phone.log()).is_empty(), "a dry run sent a write");
    assert!(!phone.0.borrow().rebooted);
    assert!(log
        .iter()
        .any(|l| l.starts_with("would: download") && l.ends_with("flash:boot_b")));
    assert_eq!(
        log[log.len() - 2..],
        ["would: set_active:b", "would: reboot"]
    );
}

fn refused_before_any_write(phone: &FakePhone, options: PhoneOptions, serial: &str) -> Error {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let err = block_on(Session::connect(phone.clone(), &release, serial, options)).unwrap_err();
    assert!(phone.written().is_empty());
    assert!(
        writes_in(&phone.log()).is_empty(),
        "something other than getvar was sent"
    );
    err
}

#[test]
fn a_wrong_codename_is_refused() {
    let phone = FakePhone::new("panther");
    let err = refused_before_any_write(&phone, options(), SERIAL);
    assert!(
        matches!(&err, Error::Phone(why) if why.contains("'panther', not cheetah")),
        "{err}"
    );
}

#[test]
fn the_phone_checks_refuse_before_any_write() {
    // Another phone than the one chosen
    let err = refused_before_any_write(&FakePhone::new("cheetah"), options(), "SOMEOTHERSERIAL");
    assert!(matches!(err, Error::Refused(_)), "{err}");
    let err = refused_before_any_write(&FakePhone::new("cheetah"), options(), "");
    assert!(matches!(err, Error::Refused(_)), "{err}");
    // Locked
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.vars.insert("unlocked".into(), "no".into());
    });
    assert!(
        matches!(refused_before_any_write(&phone, options(), SERIAL), Error::Phone(why) if why.contains("locked"))
    );
    // Running from slot b, and not a reinstall
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.vars.insert("current-slot".into(), "b".into());
    });
    assert!(
        matches!(refused_before_any_write(&phone, options(), SERIAL), Error::Phone(why) if why.contains("slot b"))
    );
    // The other bootloader slot unknown, or confirmed as something else
    let unconfirmed =
        refused_before_any_write(&FakePhone::new("cheetah"), PhoneOptions::default(), SERIAL);
    assert!(matches!(unconfirmed, Error::Phone(why) if why.contains("other slot")));
    let wrong = PhoneOptions {
        bootloader_slots_confirmed: Some("fake-0.9".into()),
        ..Default::default()
    };
    assert!(matches!(
        refused_before_any_write(&FakePhone::new("cheetah"), wrong, SERIAL),
        Error::Phone(_)
    ));
    // Slots reported, and different
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.vars
            .insert("version-bootloader-a".into(), "fake-1.0".into());
        s.vars
            .insert("version-bootloader-b".into(), "fake-0.9".into());
    });
    assert!(
        matches!(refused_before_any_write(&phone, options(), SERIAL), Error::Phone(why) if why.contains("slots differ"))
    );
    // A footer image's partition is not the size it was built for
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.partitions
            .insert("boot_b".into(), vec![SENTINEL; 32 << 10]);
    });
    assert!(
        matches!(refused_before_any_write(&phone, options(), SERIAL), Error::Phone(why) if why.contains("AVB footer"))
    );
    // An image larger than its partition
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.partitions
            .insert("vendor_kernel_boot_b".into(), vec![SENTINEL; 8 << 10]);
    });
    assert!(
        matches!(refused_before_any_write(&phone, options(), SERIAL), Error::Phone(why) if why.contains("larger than"))
    );
}

#[test]
fn slots_the_bootloader_reports_as_equal_need_no_confirmation() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.vars
            .insert("version-bootloader-a".into(), "fake-1.0".into());
        s.vars
            .insert("version-bootloader-b".into(), "fake-1.0".into());
    });
    block_on(install(&bundle, &release, &phone, PhoneOptions::default())).unwrap();
    // And a reinstall from slot b, when the owner says so
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.vars.insert("current-slot".into(), "b".into());
    });
    block_on(install(
        &bundle,
        &release,
        &phone,
        PhoneOptions {
            reinstall: true,
            ..options()
        },
    ))
    .unwrap();
}

#[test]
fn a_partition_off_the_allowlist_is_refused_even_when_signed() {
    // Signed by the trusted key, checksums valid: the rules still refuse
    for target in ["vbmeta_b", "boot_a", "persist", "boot"] {
        let bundle = TestBundle::build(
            |m| m["install"]["writes"][0]["partition"] = json!(target),
            |c| c,
        );
        let err = bundle.verify().unwrap_err();
        assert!(
            matches!(&err, Error::Manifest(why) if why.contains("not a slot B boot image")),
            "{target}: {err}"
        );
    }
    let bundle = TestBundle::build(
        |m| {
            m["install"]["storage_guard_allowlist"]
                .as_array_mut()
                .unwrap()
                .push(json!("vbmeta_b"))
        },
        |c| c.replace("dtbo_b userdata", "dtbo_b userdata vbmeta_b"),
    );
    assert!(matches!(bundle.verify(), Err(Error::Manifest(_))));
    // The descriptor and the manifest must agree, too
    let bundle = TestBundle::build(
        |_| {},
        |c| c.replace("CODENAME=cheetah", "CODENAME=panther"),
    );
    assert!(matches!(bundle.verify(), Err(Error::DeviceConf(_))));
    // And a bundle signed by a key the installer doesn't trust is nothing
    let (a, b) = (TestBundle::new(), TestBundle::new());
    assert!(matches!(
        omamobile_installer_core::bundle::verify_release(&a.files(), &b.trusted()),
        Err(Error::Signature(_))
    ));
}

#[test]
fn the_session_only_takes_this_releases_bytes_in_order() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    let writes = &release.manifest().boot_writes;
    block_on(async {
        let mut session = Session::connect(phone.clone(), &release, SERIAL, options())
            .await
            .unwrap();
        // Bytes that aren't the signed image never become a VerifiedImage
        let mut wrong = bundle.image("boot").to_vec();
        wrong[5] ^= 1;
        assert!(matches!(
            release.verify_boot_image(&writes[0], &wrong),
            Err(Error::Checksum(_))
        ));
        assert!(matches!(
            release.verify_boot_image(&writes[0], bundle.image("dtbo")),
            Err(Error::Checksum(_))
        ));
        // Out of order
        let second = release
            .verify_boot_image(&writes[1], bundle.image("init_boot"))
            .unwrap();
        assert!(matches!(
            session.flash_boot(&second).await,
            Err(Error::Refused(_))
        ));
        // An image verified against another release
        let other = TestBundle::new();
        let other_release = other.verify().unwrap();
        let foreign = other_release
            .verify_boot_image(
                &other_release.manifest().boot_writes[0],
                other.image("boot"),
            )
            .unwrap();
        assert!(matches!(
            session.flash_boot(&foreign).await,
            Err(Error::Refused(_))
        ));
        // userdata, or slot B made active, before the boot images
        let mut check = release.root_image_check();
        check.update(&bundle.root_zst);
        let root = check.finish().unwrap();
        assert!(matches!(
            session.begin_userdata(&root, 2 << 20).await,
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            session.flash_userdata_segment(&[0u8; 64]).await,
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            session.activate_and_reboot().await,
            Err(Error::Refused(_))
        ));
        // The same image twice
        let first = release
            .verify_boot_image(&writes[0], bundle.image("boot"))
            .unwrap();
        session.flash_boot(&first).await.unwrap();
        assert!(matches!(
            session.flash_boot(&first).await,
            Err(Error::Refused(_))
        ));
    });
    assert_eq!(phone.written(), ["boot_b"]);
    assert_eq!(phone.0.borrow().active_slot, "a");
    // A root image that isn't the signed one gives no proof to write with
    let mut check = release.root_image_check();
    check.update(&bundle.root_zst[..100]);
    assert!(matches!(check.finish(), Err(Error::Checksum(_))));
}

#[test]
fn a_phone_that_answers_fail_stops_the_install() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| s.fail_on.push("flash:init_boot_b".into()));
    let err = block_on(install(&bundle, &release, &phone, options())).unwrap_err();
    assert!(
        matches!(&err, Error::DeviceFail { command, message } if command == "flash:init_boot_b" && message == "injected failure"),
        "{err}"
    );
    // boot_b was written before the failure; nothing after, and slot A stays active
    assert_eq!(phone.written(), ["boot_b"]);
    assert_eq!(phone.0.borrow().active_slot, "a");
    assert!(!phone
        .log()
        .iter()
        .any(|c| c == "set_active:b" || c == "reboot" || c == "flash:userdata"));

    // After a failure the session sends no more writes
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| s.fail_on.push("download:".into()));
    block_on(async {
        let mut session = Session::connect(phone.clone(), &release, SERIAL, options())
            .await
            .unwrap();
        let first = release
            .verify_boot_image(&release.manifest().boot_writes[0], bundle.image("boot"))
            .unwrap();
        assert!(matches!(
            session.flash_boot(&first).await,
            Err(Error::DeviceFail { .. })
        ));
        phone.edit(|s| s.fail_on.clear());
        let sent = phone.log().len();
        assert!(matches!(
            session.flash_boot(&first).await,
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            session.activate_and_reboot().await,
            Err(Error::Refused(_))
        ));
        assert_eq!(phone.log().len(), sent);
    });
    assert!(phone.written().is_empty());

    // FAIL to set_active: no reboot
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| s.fail_on.push("set_active".into()));
    assert!(matches!(
        block_on(install(&bundle, &release, &phone, options())),
        Err(Error::DeviceFail { .. })
    ));
    assert!(!phone.0.borrow().rebooted);
}

#[test]
fn a_short_download_is_refused() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    // The phone offers to take fewer bytes than the image has
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| s.short_data = true);
    let err = block_on(install(&bundle, &release, &phone, options())).unwrap_err();
    assert_eq!(
        err,
        Error::ShortDownload {
            wanted: 16 << 10,
            accepted: 8 << 10
        }
    );
    assert!(phone.written().is_empty());
    assert!(!phone
        .log()
        .iter()
        .any(|c| c.starts_with("flash:") || c == "set_active:b"));

    // The USB write itself comes up short
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| s.take_at_most = Some(1000));
    let err = block_on(install(&bundle, &release, &phone, options())).unwrap_err();
    assert_eq!(
        err,
        Error::ShortDownload {
            wanted: 16 << 10,
            accepted: 1000
        }
    );
    assert!(phone.written().is_empty());
    assert!(!phone
        .log()
        .iter()
        .any(|c| c.starts_with("flash:") || c == "set_active:b"));
}

#[test]
fn a_phone_swapped_mid_install_gets_nothing_more() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    // The checks pass on one phone; later another serial number answers
    phone.edit(|s| s.swap_serial_after = Some(30));
    let err = block_on(install(&bundle, &release, &phone, options())).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("changed during the install")),
        "{err}"
    );
    assert!(!phone
        .log()
        .iter()
        .any(|c| c == "set_active:b" || c == "reboot"));
    assert_eq!(phone.0.borrow().active_slot, "a");
}

#[test]
fn a_root_image_larger_than_userdata_is_refused() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.partitions
            .insert("userdata".into(), vec![SENTINEL; 1 << 20]);
    });
    let err = block_on(install(&bundle, &release, &phone, options())).unwrap_err();
    assert!(
        matches!(&err, Error::Phone(why) if why.contains("larger than userdata")),
        "{err}"
    );
    assert!(!phone.written().contains(&"userdata".to_string()));
    assert_eq!(phone.0.borrow().active_slot, "a");
}

/// Writes a signed test bundle to fixtures/selftest/, for the browser
/// self-test (web/, feature `selftest`), which can't make keys. Run by hand:
/// `cargo test -p omamobile-installer-core --test fastboot_install -- --ignored`
#[test]
#[ignore = "regenerates fixtures/selftest"]
fn write_the_selftest_fixture() {
    let b = TestBundle::new();
    b.verify().unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest");
    std::fs::create_dir_all(dir.join("images")).unwrap();
    let put = |name: &str, data: &[u8]| std::fs::write(dir.join(name), data).unwrap();
    put("SHA256SUMS", &b.sums);
    put("SHA256SUMS.sig", &b.sig);
    put("test-key.gpg", &b.keyring);
    put("test-key.fingerprint", b.fingerprint.as_bytes());
    put("manifest.json", &b.manifest_json);
    put("MANIFEST", &b.manifest_txt);
    put("device.conf", &b.device_conf);
    put("root.img.zst", &b.root_zst);
    for (name, data) in &b.images {
        put(&format!("images/{name}.img"), data);
    }
    put("factory-builds.json", &b.index_json);
    // The stand-in for Google's factory zip, under the name the index gives
    // it (the name carries its digest, so it changes only with the bytes)
    let _ = std::fs::remove_dir_all(dir.join("factory"));
    std::fs::create_dir_all(dir.join("factory")).unwrap();
    put(&format!("factory/{}", b.factory_zip_name), &b.factory_zip);
}
