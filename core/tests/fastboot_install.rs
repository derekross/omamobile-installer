// SPDX-License-Identifier: MIT
//! Whole installs, and refused ones, against the fake phone. No USB.
mod support;
use omamobile_installer_core::bundle::VerifiedRelease;
use omamobile_installer_core::fastboot::{PhoneOptions, Session};
use omamobile_installer_core::flow::FlowOptions;
use omamobile_installer_core::sparse::MAX_PIECE_BLOCKS;
use omamobile_installer_core::Error;
use pollster::block_on;
use serde_json::json;
use support::bundle::TestBundle;
use support::drive::{walk, writing_session};
use support::fake::{FakePhone, SENTINEL, SERIAL};

const OURS: [&str; 6] = [
    "boot_b",
    "dtbo_b",
    "init_boot_b",
    "userdata",
    "vendor_boot_b",
    "vendor_kernel_boot_b",
];

/// What the owner said of a phone that tells one bootloader slot, for a
/// session opened directly. Such a session is a dry run: [`Session::connect`]
/// opens no other kind.
fn options() -> PhoneOptions {
    PhoneOptions {
        bootloader_slots_confirmed: Some("fake-1.0".into()),
        ..Default::default()
    }
}

/// The same statement, for a flow that writes.
fn stated() -> FlowOptions {
    FlowOptions {
        write: true,
        bootloader_slots_match: Some("fake-1.0".into()),
        ..Default::default()
    }
}

/// The whole install through a session opened directly: a dry run.
async fn dry_install(
    bundle: &TestBundle,
    release: &VerifiedRelease,
    phone: &FakePhone,
    options: PhoneOptions,
) -> Result<Vec<String>, Error> {
    let mut session = Session::connect(phone.clone(), release, SERIAL, options).await?;
    assert!(session.is_dry_run());
    walk(&mut session, bundle, release).await?;
    assert!(session.is_done());
    Ok(session.log().to_vec())
}

/// The whole install through the session that writes. Only the flow opens
/// one (`Flow::begin_write`), so the flow is taken to its write step first;
/// `then` runs on the phone once that session is open, for a fault that is
/// to meet the writes and not the flow's questions before them.
fn install_then(
    bundle: &TestBundle,
    phone: &FakePhone,
    options: FlowOptions,
    then: impl FnOnce(&FakePhone),
) -> Result<Vec<String>, Error> {
    let (flow, mut session) = writing_session(bundle, phone, options);
    assert!(!session.is_dry_run());
    then(phone);
    block_on(walk(&mut session, bundle, flow.release().unwrap()))?;
    assert!(session.is_done());
    Ok(session.log().to_vec())
}

fn install(
    bundle: &TestBundle,
    phone: &FakePhone,
    options: FlowOptions,
) -> Result<Vec<String>, Error> {
    install_then(bundle, phone, options, |_| {})
}

fn writes_in(log: &[String]) -> Vec<&String> {
    log.iter().filter(|c| !c.starts_with("getvar:")).collect()
}

#[test]
fn an_install_writes_slot_b_and_userdata_and_nothing_else() {
    let bundle = TestBundle::new();
    let phone = FakePhone::new("cheetah");
    install(&bundle, &phone, stated()).unwrap();

    assert_eq!(phone.written(), OURS);
    for (name, size, _) in support::bundle::IMAGES {
        let part = phone.partition(&format!("{name}_b"));
        assert!(part[..size] == *bundle.image(name), "{name}_b");
        assert!(part[size..].iter().all(|b| *b == SENTINEL));
    }
    // The root image as the release built it, but for the blocks it keeps
    // for the owner's answers, which hold them
    let userdata = phone.partition("userdata");
    let installed = bundle.root_as_installed();
    assert!(
        userdata[..bundle.root_raw.len()] == installed[..],
        "userdata is not the root image with the answers"
    );
    let differ: Vec<usize> = (0..bundle.root_raw.len() / 4096)
        .filter(|b| {
            installed[b * 4096..(b + 1) * 4096] != bundle.root_raw[b * 4096..(b + 1) * 4096]
        })
        .collect();
    assert_eq!(differ, [300, 302], "only the placeholders' blocks change");
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
    let log = block_on(dry_install(&bundle, &release, &phone, options())).unwrap();
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

/// The checks a session starts with are one function for both kinds of
/// session, and the kind that writes can't be opened from here: these go
/// through a dry run's.
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
    let writing = FlowOptions {
        write: true,
        ..Default::default()
    };
    install(&bundle, &phone, writing).unwrap();
    assert_eq!(phone.written(), OURS);
    // A session opened directly, on the same kind of phone: a dry run
    let untouched = FakePhone::new("cheetah");
    untouched.edit(|s| {
        s.vars
            .insert("version-bootloader-a".into(), "fake-1.0".into());
        s.vars
            .insert("version-bootloader-b".into(), "fake-1.0".into());
    });
    block_on(dry_install(
        &bundle,
        &release,
        &untouched,
        PhoneOptions::default(),
    ))
    .unwrap();
    assert!(untouched.written().is_empty());
    assert!(writes_in(&untouched.log()).is_empty());
    assert!(!untouched.0.borrow().rebooted);
    // And a reinstall from slot b, when the owner says so
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| {
        s.vars.insert("current-slot".into(), "b".into());
    });
    install(
        &bundle,
        &phone,
        FlowOptions {
            reinstall: true,
            ..stated()
        },
    )
    .unwrap();
    assert_eq!(phone.written(), OURS);
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
    let phone = FakePhone::new("cheetah");
    let (flow, mut session) = writing_session(&bundle, &phone, stated());
    let release = flow.release().unwrap().clone();
    let writes = &release.manifest().boot_writes;
    block_on(async {
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
    let err = install(&bundle, &phone, stated()).unwrap_err();
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
    let (_flow, mut session) = writing_session(&bundle, &phone, stated());
    block_on(async {
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
        install(&bundle, &phone, stated()),
        Err(Error::DeviceFail { .. })
    ));
    assert!(!phone.0.borrow().rebooted);
}

#[test]
fn a_short_download_is_refused() {
    let bundle = TestBundle::new();
    // The phone offers to take fewer bytes than the image has
    let phone = FakePhone::new("cheetah");
    phone.edit(|s| s.short_data = true);
    let err = install(&bundle, &phone, stated()).unwrap_err();
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
    let err = install(&bundle, &phone, stated()).unwrap_err();
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
    let phone = FakePhone::new("cheetah");
    // The checks pass on one phone; later, among the boot images, another
    // serial number answers
    let err = install_then(&bundle, &phone, stated(), |phone| {
        phone.edit(|s| s.swap_serial_after = Some(s.log.len() + 10));
    })
    .unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("changed during the install")),
        "{err}"
    );
    assert!(!phone
        .log()
        .iter()
        .any(|c| c == "set_active:b" || c == "reboot"));
    assert_eq!(phone.0.borrow().active_slot, "a");
    // It was the session that writes, and it had begun: three images in
    assert_eq!(phone.written(), ["boot_b", "init_boot_b", "vendor_boot_b"]);
}

#[test]
fn a_root_image_larger_than_userdata_is_refused() {
    let bundle = TestBundle::new();
    let phone = FakePhone::new("cheetah");
    // The flow checks this before the first write, and stops there
    // (write_path.rs). Here userdata shrinks after that check, so the one
    // the session makes when userdata's turn comes is what refuses
    let err = install_then(&bundle, &phone, stated(), |phone| {
        phone.edit(|s| {
            s.partitions
                .insert("userdata".into(), vec![SENTINEL; 1 << 20]);
        });
    })
    .unwrap_err();
    assert!(
        matches!(&err, Error::Phone(why) if why.contains("larger than userdata")),
        "{err}"
    );
    assert!(!phone.written().contains(&"userdata".to_string()));
    assert_eq!(phone.0.borrow().active_slot, "a");
}

/// One sparse image of `total` blocks made of a single FILL chunk of
/// `filled` blocks from the start, and a skip over the rest.
fn fill_piece(total: u32, filled: u32) -> Vec<u8> {
    let mut p = Vec::new();
    let chunks = u32::from(filled > 0) + u32::from(filled < total);
    p.extend_from_slice(&0xed26_ff3a_u32.to_le_bytes());
    for v in [1u16, 0, 28, 12] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    for v in [4096u32, total, chunks, 0] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    let mut chunk = |kind: u16, blocks: u32, data: &[u8]| {
        p.extend_from_slice(&kind.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&blocks.to_le_bytes());
        p.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
        p.extend_from_slice(data);
    };
    if filled > 0 {
        chunk(0xcac2, filled, &[0; 4]);
    }
    if filled < total {
        chunk(0xcac3, total - filled, &[]);
    }
    p
}

/// The session bounds what one `flash:userdata` makes the phone write,
/// whoever cut the piece: it reads each piece's chunks and refuses one that
/// covers more than `MAX_PIECE_BLOCKS` blocks, or none.
#[test]
fn a_piece_that_covers_more_than_one_flash_may_is_refused_by_the_session() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let mut check = release.root_image_check();
    check.update(&bundle.root_zst);
    let root = check.finish().unwrap();
    // An image one block larger than a piece may cover, on a phone whose
    // userdata holds it
    let total = MAX_PIECE_BLOCKS + 1;
    let image_len = total as u64 * 4096;
    let at_userdata = || {
        let phone = FakePhone::new("cheetah");
        phone.edit(|s| {
            s.partitions
                .insert("userdata".into(), vec![SENTINEL; image_len as usize]);
        });
        let mut session =
            block_on(Session::connect(phone.clone(), &release, SERIAL, options())).unwrap();
        block_on(async {
            for write in &release.manifest().boot_writes {
                let image = release
                    .verify_boot_image(write, bundle.image(&write.image))
                    .unwrap();
                session.flash_boot(&image).await.unwrap();
            }
            session.begin_userdata(&root, image_len).await.unwrap();
        });
        (phone, session)
    };
    // One small download that would write all of it: refused, and the
    // session takes nothing more
    let (phone, mut session) = at_userdata();
    let too_much = fill_piece(total, total);
    assert!(too_much.len() < 100);
    let err = block_on(session.flash_userdata_segment(&too_much)).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("writes 65537 blocks") && why.contains("at most 65536")),
        "{err}"
    );
    assert!(session.pieces().is_empty());
    let at_most = fill_piece(total, MAX_PIECE_BLOCKS);
    assert!(matches!(
        block_on(session.flash_userdata_segment(&at_most)),
        Err(Error::Refused(_))
    ));
    assert!(writes_in(&phone.log()).is_empty());

    // Exactly the most a piece may cover is taken, and recorded
    let (_phone, mut session) = at_userdata();
    block_on(session.flash_userdata_segment(&at_most)).unwrap();
    assert_eq!(session.pieces().len(), 1);
    assert_eq!(session.pieces()[0].blocks, MAX_PIECE_BLOCKS);
    assert_eq!(session.pieces()[0].bytes, at_most.len() as u64);

    // A piece that writes nothing at all is not a piece
    let (_phone, mut session) = at_userdata();
    let nothing = fill_piece(total, 0);
    let err = block_on(session.flash_userdata_segment(&nothing)).unwrap_err();
    assert!(
        matches!(&err, Error::Refused(why) if why.contains("writes 0 blocks")),
        "{err}"
    );
}

/// The session says, for each piece of userdata, how many blocks its flash
/// covered and how long the phone took to answer, by the front end's clock.
#[test]
fn the_session_reports_each_piece_its_blocks_and_how_long_its_flash_took() {
    use core::time::Duration;
    use std::cell::Cell;
    use std::rc::Rc;
    let bundle = TestBundle::new();
    let blocks = (bundle.root_raw.len() / 4096) as u32;
    // A clock that moves 250 ms each time it is read
    let ticking = || {
        let ticks = Rc::new(Cell::new(0u64));
        Box::new(move || {
            ticks.set(ticks.get() + 1);
            Duration::from_millis(250 * ticks.get())
        })
    };

    let phone = FakePhone::new("cheetah");
    let (flow, mut session) = writing_session(&bundle, &phone, stated());
    session.time_flashes_with(ticking());
    block_on(walk(&mut session, &bundle, flow.release().unwrap())).unwrap();
    let pieces = session.pieces().to_vec();
    println!("{pieces:#?}");
    let flashed = phone
        .log()
        .iter()
        .filter(|c| *c == "flash:userdata")
        .count();
    assert!(pieces.len() > 4 && pieces.len() == flashed);
    // Zero blocks are written too, so the pieces cover every block once
    assert_eq!(pieces.iter().map(|p| p.blocks).sum::<u32>(), blocks);
    let downloads: Vec<u64> = phone
        .log()
        .iter()
        .filter_map(|c| c.strip_prefix("download:"))
        .map(|hex| u64::from_str_radix(hex, 16).unwrap())
        .skip(5)
        .collect();
    assert_eq!(
        pieces.iter().map(|p| p.bytes).collect::<Vec<_>>(),
        downloads
    );
    for piece in &pieces {
        assert!(piece.blocks >= 1 && piece.blocks <= MAX_PIECE_BLOCKS);
        // Read once as the flash leaves and once at its answer
        assert_eq!(piece.flash, Some(Duration::from_millis(250)));
    }

    // Without a clock the blocks are still reported, and no time
    let phone = FakePhone::new("cheetah");
    let (flow, mut session) = writing_session(&bundle, &phone, stated());
    block_on(walk(&mut session, &bundle, flow.release().unwrap())).unwrap();
    assert_eq!(session.pieces().len(), pieces.len());
    assert!(session.pieces().iter().all(|p| p.flash.is_none()));

    // A dry run reports the same pieces, and no time: nothing was flashed
    let phone = FakePhone::new("cheetah");
    let release = bundle.verify().unwrap();
    let mut dry = block_on(Session::connect(phone.clone(), &release, SERIAL, options())).unwrap();
    dry.time_flashes_with(ticking());
    block_on(walk(&mut dry, &bundle, &release)).unwrap();
    let held_back: Vec<(u64, u32)> = dry.pieces().iter().map(|p| (p.bytes, p.blocks)).collect();
    assert_eq!(
        held_back,
        pieces
            .iter()
            .map(|p| (p.bytes, p.blocks))
            .collect::<Vec<_>>()
    );
    assert!(dry.pieces().iter().all(|p| p.flash.is_none()));

    // A flash whose answer never comes: that piece is the last one
    // reported, with its blocks and no time, so a stop can say what was in
    // doubt
    let phone = FakePhone::new("cheetah");
    let (flow, mut session) = writing_session(&bundle, &phone, stated());
    session.time_flashes_with(ticking());
    // Four commands a write (two questions, download, flash): five boot
    // images, userdata's size, two pieces, and the third piece's flash
    let third = phone.log().len() + 5 * 4 + 1 + 2 * 4 + 3;
    phone.edit(|s| s.lose_answer_at = Some(third));
    let err = block_on(walk(&mut session, &bundle, flow.release().unwrap())).unwrap_err();
    assert!(matches!(err, Error::Transport(_)), "{err}");
    assert_eq!(phone.log().last().unwrap(), "flash:userdata");
    let reported = session.pieces();
    assert_eq!(reported.len(), 3);
    assert!(reported[..2].iter().all(|p| p.flash.is_some()));
    assert_eq!(
        (reported[2].blocks, reported[2].flash),
        (pieces[2].blocks, None)
    );
}

/// Writes the signed test bundles the other front ends' tests read: to
/// fixtures/selftest/, for the browser self-test (web/, feature
/// `selftest`), which can't make keys, the tool's tests and the app's; and
/// to fixtures/after/, the same with the stand-ins for the tools a release
/// ships (fixtures/after-stand-ins/tools), for the steps after the write.
/// Run by hand:
/// `cargo test -p omamobile-installer-core --test fastboot_install -- --ignored`
#[test]
#[ignore = "regenerates fixtures/selftest and fixtures/after"]
fn write_the_selftest_fixture() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let stand_ins = root.join("after-stand-ins/tools");
    let mut tools: Vec<(String, Vec<u8>)> = std::fs::read_dir(&stand_ins)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            (format!("tools/{name}"), std::fs::read(&p).unwrap())
        })
        .collect();
    tools.sort();
    for (name, bundle) in [
        ("selftest", TestBundle::new()),
        ("after", TestBundle::with_files(tools)),
    ] {
        bundle.verify().unwrap();
        let dir = root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("images")).unwrap();
        let put = |name: &str, data: &[u8]| std::fs::write(dir.join(name), data).unwrap();
        put("SHA256SUMS", &bundle.sums);
        put("SHA256SUMS.sig", &bundle.sig);
        put("test-key.gpg", &bundle.keyring);
        put("test-key.fingerprint", bundle.fingerprint.as_bytes());
        put("manifest.json", &bundle.manifest_json);
        put("MANIFEST", &bundle.manifest_txt);
        put("device.conf", &bundle.device_conf);
        put("root.img.zst", &bundle.root_zst);
        for (name, data) in &bundle.images {
            put(&format!("images/{name}.img"), data);
        }
        put("factory-builds.json", &bundle.index_json);
        // The stand-in for Google's factory zip, under the name the index
        // gives it (the name carries its digest, so it changes only with
        // the bytes)
        std::fs::create_dir_all(dir.join("factory")).unwrap();
        put(
            &format!("factory/{}", bundle.factory_zip_name),
            &bundle.factory_zip,
        );
        for (path, data) in &bundle.extra {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, data).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }
}

#[test]
fn a_session_takes_slot_bs_version_on_a_reinstall_from_slot_b_and_nowhere_else() {
    // The session's own check, as the flow's: a phone that tells one
    // bootloader copy's version and runs from slot B, in a reinstall, is
    // past it with nobody's word about slot A (the maintainer's decision of
    // 2026-11-13)
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    let on = |slot: &str| {
        let phone = FakePhone::new("cheetah");
        phone.edit(|s| {
            s.vars.insert("current-slot".into(), slot.into());
        });
        phone
    };
    let reinstall = PhoneOptions {
        reinstall: true,
        ..Default::default()
    };
    let phone = on("b");
    let log = block_on(dry_install(&bundle, &release, &phone, reinstall.clone())).unwrap();
    assert!(log
        .iter()
        .any(|l| l.starts_with("bootloader: slot b's, which the phone runs")));
    assert!(phone.written().is_empty());
    // From slot A the version it tells is slot A's: refused, reinstall or not
    let err = refused_before_any_write(&on("a"), reinstall, SERIAL);
    assert!(
        matches!(&err, Error::Phone(why) if why.contains("only reports slot a")),
        "{err}"
    );
    // From slot B without the reinstall: refused, as before
    let err = refused_before_any_write(&on("b"), PhoneOptions::default(), SERIAL);
    assert!(
        matches!(&err, Error::Phone(why) if why.contains("only reports slot b")),
        "{err}"
    );
    // A statement of another version is refused first
    let err = refused_before_any_write(
        &on("b"),
        PhoneOptions {
            reinstall: true,
            bootloader_slots_confirmed: Some("fake-0.9".into()),
            ..Default::default()
        },
        SERIAL,
    );
    assert!(
        matches!(&err, Error::Phone(why) if why.contains("confirmed as fake-0.9")),
        "{err}"
    );
    // And the session that writes, reached through the flow, writes it
    let phone = on("b");
    install(
        &bundle,
        &phone,
        FlowOptions {
            write: true,
            reinstall: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(phone.written(), OURS);
}
