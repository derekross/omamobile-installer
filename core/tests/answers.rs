// SPDX-License-Identifier: MIT
//! The owner's answers in the root image's placeholders (docs/DESIGN.md
//! §12, the decision of 2026-11-13), against the fake phone. No USB.
//!
//! The first test makes a small real ext4 image the way Omarchy Mobile's
//! `rootfs/build-inside` makes the root image (`mkfs.ext4 -d`, 4096-byte
//! blocks, the two placeholder files), installs it through the flow, and
//! reads what the fake phone holds with e2fsprogs: `e2fsck -fn` must find
//! it clean, and `debugfs` must find the answers in the two files. It needs
//! `mkfs.ext4`, `debugfs` and `e2fsck` on this computer (e2fsprogs; CI has
//! them), and makes its image under the build's own scratch directory.
//! `OMAMOBILE_SKIP_EXT4_TEST=1` skips it where e2fsprogs can't be had.
mod support;
use omamobile_installer_core::firstboot::{root_answers, BOOT_UPDATE_CONF, FIRSTBOOT_CONF};
use omamobile_installer_core::flow::{AnswersInput, Flow, FlowOptions, OwnerAnswers, Step};
use omamobile_installer_core::hash::sha256_hex;
use omamobile_installer_core::Error;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::bundle::TestBundle;
use support::drive::{drive, SALT};
use support::fake::FakePhone;

fn writing() -> FlowOptions {
    FlowOptions {
        write: true,
        bootloader_slots_match: Some("fake-1.0".into()),
        ..Default::default()
    }
}

/// Made-up answers, with a Wi-Fi network and its password.
fn with_wifi() -> OwnerAnswers {
    OwnerAnswers::check(&AnswersInput {
        username: "alex",
        password: "4812",
        password_again: "4812",
        hostname: "",
        timezone: "Europe/Paris",
        wifi_ssid: "Test network",
        wifi_psk: "not-a-real-passphrase",
    })
    .unwrap()
}

/// A made-up SSH public key line, new for each test run: an Ed25519 key's
/// form (its 68 characters of key), random characters, never a real key.
fn test_key() -> String {
    use rand::Rng;
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rng = rand::thread_rng();
    let body: String = (0..68).map(|_| B64[rng.gen_range(0..64)] as char).collect();
    format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5{body} installer-test")
}

/// A flow started with `options`, taken to its end with `answers` given at the
/// answers step.
fn install_with(
    bundle: &TestBundle,
    phone: &FakePhone,
    answers: &OwnerAnswers,
    options: FlowOptions,
) -> Flow {
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, bundle, phone, "cheetah", "answers");
    assert_eq!(flow.screen().state, "answers", "{:?}", flow.step());
    flow.answers(answers, SALT).unwrap();
    drive(&mut flow, bundle, phone, "cheetah", "installed");
    flow
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/sbin"].map(PathBuf::from))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn run(tool: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(tool).args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn cat(debugfs: &Path, img: &Path, path: &str) -> Vec<u8> {
    let out = Command::new(debugfs)
        .args(["-R", &format!("cat {path}"), img.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}

/// The lines a reader of these files takes: neither comments nor blank.
fn read_lines(text: &[u8]) -> Vec<String> {
    String::from_utf8(text.to_vec())
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_answers_land_in_a_real_ext4_image_that_stays_clean() {
    let tools: Vec<Option<PathBuf>> = ["mkfs.ext4", "debugfs", "e2fsck"]
        .iter()
        .map(|t| find_tool(t))
        .collect();
    if tools.iter().any(Option::is_none) {
        if std::env::var_os("OMAMOBILE_SKIP_EXT4_TEST").is_some() {
            eprintln!("skipped: no e2fsprogs here (OMAMOBILE_SKIP_EXT4_TEST)");
            return;
        }
        panic!("this test needs mkfs.ext4, debugfs and e2fsck (e2fsprogs)");
    }
    let [mkfs, debugfs, e2fsck] = [0, 1, 2].map(|i| tools[i].clone().unwrap());

    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("answers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let etc = dir.join("root/etc/omamobile");
    std::fs::create_dir_all(&etc).unwrap();
    std::fs::write(dir.join("root/etc/hostname"), "omamobile\n").unwrap();
    for (path, mode) in [(FIRSTBOOT_CONF, 0o600), (BOOT_UPDATE_CONF, 0o644)] {
        let file = dir.join("root").join(&path[1..]);
        std::fs::write(&file, support::bundle::placeholder(path)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    // As big as the fake phone's userdata: 8 MiB, with a 4 MiB journal
    let img = dir.join("root.img");
    std::fs::File::create(&img)
        .unwrap()
        .set_len(8 << 20)
        .unwrap();
    let (ok, said) = run(
        &mkfs,
        &[
            "-q",
            "-F",
            "-b",
            "4096",
            "-L",
            "omamobile-root",
            "-m",
            "1",
            "-d",
            dir.join("root").to_str().unwrap(),
            img.to_str().unwrap(),
        ],
    );
    assert!(ok, "mkfs.ext4: {said}");
    let img_s = img.to_str().unwrap();

    // Where each file's one block is, as rootfs/placeholders finds it
    let raw = std::fs::read(&img).unwrap();
    let mut places: Vec<Value> = Vec::new();
    for path in [FIRSTBOOT_CONF, BOOT_UPDATE_CONF] {
        let (ok, stat) = run(&debugfs, &["-R", &format!("stat {path}"), img_s]);
        assert!(
            ok && stat.contains("Flags: 0x80000") && stat.contains("Size: 4096"),
            "{stat}"
        );
        let (ok, blocks) = run(&debugfs, &["-R", &format!("blocks {path}"), img_s]);
        assert!(ok, "{blocks}");
        let blocks: Vec<u64> = blocks
            .split_whitespace()
            .filter_map(|b| b.parse().ok())
            .collect();
        assert_eq!(blocks.len(), 1, "{path} is one block");
        let offset = blocks[0] * 4096;
        let bytes = &raw[offset as usize..offset as usize + 4096];
        assert_eq!(bytes, &support::bundle::placeholder(path)[..], "{path}");
        places.push(
            json!({"path": path, "offset": offset, "length": 4096, "sha256": sha256_hex(bytes)}),
        );
    }

    // A run that hands the flow this computer's key, as the tool's json
    // --write does: it goes into firstboot.conf with the answers
    let key = test_key();
    let bundle = TestBundle::with_root_image(raw.clone(), json!(places));
    let phone = FakePhone::new("cheetah");
    let flow = install_with(
        &bundle,
        &phone,
        &with_wifi(),
        FlowOptions {
            owner_ssh_key: Some(key.clone()),
            ..writing()
        },
    );
    assert!(
        matches!(flow.step(), Step::Installed(_)),
        "{:?}",
        flow.step()
    );

    let installed = dir.join("installed.img");
    std::fs::write(&installed, &phone.partition("userdata")[..raw.len()]).unwrap();
    let inst = installed.to_str().unwrap();
    let (ok, said) = run(&e2fsck, &["-fn", inst]);
    assert!(ok, "e2fsck -fn finds the installed image clean: {said}");

    let conf = cat(&debugfs, &installed, FIRSTBOOT_CONF);
    assert_eq!(conf.len(), 4096);
    assert_eq!(conf.last(), Some(&b'\n'));
    let lines = read_lines(&conf);
    let hash = &lines[1];
    assert!(
        hash.starts_with(&format!("PASSWORD_HASH=$6${SALT}$")),
        "{hash}"
    );
    assert_eq!(
        lines,
        [
            "USERNAME=alex",
            hash.as_str(),
            "HOSTNAME=alex-phone",
            "TIMEZONE=Europe/Paris",
            &format!("SSH_KEY={key}"),
            "CREDENTIAL=pin",
            "WIFI_SSID=Test network",
            "WIFI_PSK=not-a-real-passphrase",
        ]
    );
    // The image keeps no place for root's authorized_keys: it is
    // omamobile-firstboot's to make from SSH_KEY at the first start
    let (_, stat) = run(&debugfs, &["-R", "stat /root/.ssh/authorized_keys", inst]);
    assert!(!stat.contains("Size:"), "{stat}");
    // The PIN itself is nowhere in the image
    let image = std::fs::read(&installed).unwrap();
    assert!(!String::from_utf8_lossy(&conf).contains("4812"));
    let boot = cat(&debugfs, &installed, BOOT_UPDATE_CONF);
    assert_eq!(boot.len(), 4096);
    assert_eq!(read_lines(&boot), ["AUTO_WRITE=yes"]);

    // Exactly the two blocks changed; the file system's metadata is the
    // release's, byte for byte
    let differ: Vec<usize> = (0..raw.len() / 4096)
        .filter(|b| image[b * 4096..(b + 1) * 4096] != raw[b * 4096..(b + 1) * 4096])
        .collect();
    let mut want: Vec<usize> = places
        .iter()
        .map(|p| p["offset"].as_u64().unwrap() as usize / 4096)
        .collect();
    want.sort();
    assert_eq!(differ, want);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A placeholder whose bytes are not where the signed manifest says (the
/// release's own mistake): the dry run finds it, before anything is
/// written, and so does a write that never had a dry run's walk.
#[test]
fn a_placeholder_that_is_not_where_the_manifest_says_stops_the_dry_run() {
    let bundle = TestBundle::build(
        |m| {
            // One block early: the stand-in image has zeros there
            m["root_image"]["placeholders"][1]["offset"] = json!(301 * 4096);
        },
        |c| c,
    );
    let phone = FakePhone::new("cheetah");
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        writing(),
    );
    drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
    let Step::Stopped(stop) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    assert_eq!(stop.id(), "dry-run-refused");
    assert!(
        stop.message[0].contains("does not hold the place for /etc/omamobile/firstboot.conf"),
        "{:?}",
        stop.message
    );
    assert!(phone.written().is_empty() && phone.changes().is_empty());
}

/// The rules for `root_image.placeholders`: only the two files, each once,
/// one block at a block boundary, with a SHA-256, firstboot.conf among
/// them, and no key this installer doesn't know.
#[test]
fn the_manifest_names_its_placeholders_within_the_rules() {
    let refused = |edit: &dyn Fn(&mut Value), why: &str| {
        let bundle = TestBundle::build(|m| edit(&mut m["root_image"]["placeholders"]), |c| c);
        let err = bundle.verify().unwrap_err();
        assert!(
            matches!(&err, Error::Manifest(s) if s.contains(why)),
            "{why}: {err}"
        );
    };
    refused(
        &|p| p[0]["path"] = json!("/root/.ssh/authorized_keys"),
        "doesn't fill",
    );
    refused(&|p| p[1]["path"] = p[0]["path"].clone(), "twice");
    refused(&|p| p[0]["length"] = json!(8192), "one 4096-byte block");
    refused(
        &|p| p[0]["offset"] = json!(300 * 4096 + 512),
        "block boundary",
    );
    refused(&|p| p[0]["sha256"] = json!("00"), "no SHA-256");
    refused(&|p| p[1]["offset"] = p[0]["offset"].clone(), "same place");
    refused(&|p| p[0]["mode"] = json!("0600"), "unknown field");
    // boot-update.conf alone: no place for the answers themselves
    refused(
        &|p| {
            let keep = p[0].clone();
            *p = json!([keep]);
        },
        "has none for /etc/omamobile/firstboot.conf",
    );
    // Absent, or empty: a release from before the placeholders
    for edit in [
        |m: &mut Value| {
            m["root_image"]
                .as_object_mut()
                .unwrap()
                .remove("placeholders");
        },
        |m: &mut Value| m["root_image"]["placeholders"] = json!([]),
    ] {
        let release = TestBundle::build(edit, |c| c).verify().unwrap();
        assert!(release.manifest().root_image.placeholders.is_empty());
    }
    // firstboot.conf alone is taken: then AUTO_WRITE is not set
    let bundle = TestBundle::build(
        |m| {
            let fb = m["root_image"]["placeholders"][1].clone();
            m["root_image"]["placeholders"] = json!([fb]);
        },
        |c| c,
    );
    let release = bundle.verify().unwrap();
    let answers = root_answers(&release, &with_wifi(), None, SALT).unwrap();
    assert_eq!(answers.paths(), [FIRSTBOOT_CONF]);
    assert_eq!(answers.auto_write(), None);
}

/// A flow that writes stops at its start for a release whose system image
/// keeps no place for the answers, before any phone is asked anything. A
/// dry run of it goes on, and says so where the answers are asked for and
/// on its last screen.
#[test]
fn a_release_without_placeholders_is_not_installed() {
    let old = TestBundle::build(
        |m| {
            m["root_image"]
                .as_object_mut()
                .unwrap()
                .remove("placeholders");
        },
        |c| c,
    );
    let flow = Flow::begin(&old.files(), old.index(), &old.trusted(), writing());
    let Step::Stopped(stop) = flow.step() else {
        panic!("{:?}", flow.step())
    };
    assert_eq!(stop.id(), "no-place-for-answers");
    assert!(stop.message[0].contains("keeps no place for your answers"));
    assert!(stop.message[1].contains("\"install\""));
    assert_eq!(stop.footer()[0], "Nothing was sent to a phone.");

    let phone = FakePhone::new("cheetah");
    let options = FlowOptions {
        bootloader_slots_match: Some("fake-1.0".into()),
        ..Default::default()
    };
    let mut flow = Flow::begin(&old.files(), old.index(), &old.trusted(), options);
    drive(&mut flow, &old, &phone, "cheetah", "answers");
    assert!(flow.screen().body[1].contains("keeps no place for your answers"));
    drive(&mut flow, &old, &phone, "cheetah", "never");
    let screen = flow.screen();
    assert_eq!(screen.state, "done");
    assert!(
        screen
            .body
            .iter()
            .any(|l| l.contains("keeps no place for your answers")),
        "{:?}",
        screen.body
    );
    assert!(phone.written().is_empty());
}

/// What goes into the placeholders: firstboot.conf in omamobile-firstboot's
/// format, padded to the block with one comment line; boot-update.conf with
/// the descriptor's BOOT_UPDATE_AUTO_WRITE.
#[test]
fn the_placeholders_hold_the_answers_padded_to_the_block() {
    let release = TestBundle::new().verify().unwrap();
    let answers = root_answers(&release, &with_wifi(), None, SALT).unwrap();
    let blocks: Vec<(&str, u64, Vec<u8>)> = answers
        .blocks()
        .map(|(p, o, b)| (p, o, b.to_vec()))
        .collect();
    // In the order of the image
    assert_eq!(blocks[0].0, BOOT_UPDATE_CONF);
    assert_eq!(blocks[1].0, FIRSTBOOT_CONF);
    for (_, _, b) in &blocks {
        assert_eq!(b.len(), 4096);
        assert_eq!(b.last(), Some(&b'\n'));
    }
    let conf = String::from_utf8(blocks[1].2.clone()).unwrap();
    let (text, padding) = conf.split_at(conf.find("\n#").unwrap() + 1);
    assert!(text.starts_with("# Written by the Omarchy Mobile installer"));
    assert!(text.ends_with("WIFI_PSK=not-a-real-passphrase\n"), "{text}");
    assert!(padding.starts_with('#') && padding[1..padding.len() - 1].bytes().all(|b| b == b' '));
    assert_eq!(answers.auto_write(), Some(true));
    assert_eq!(read_lines(&blocks[0].2), ["AUTO_WRITE=yes"]);
    // Neither the PIN nor the Wi-Fi password in the debug output
    let debug = format!("{answers:?}");
    assert!(
        !debug.contains("4812") && !debug.contains("passphrase") && !debug.contains("$6$"),
        "{debug}"
    );

    // BOOT_UPDATE_AUTO_WRITE=no in the descriptor: AUTO_WRITE=no
    let no = TestBundle::build(|_| {}, |c| c + "BOOT_UPDATE_AUTO_WRITE=no\n");
    let answers = root_answers(&no.verify().unwrap(), &with_wifi(), None, SALT).unwrap();
    let boot = answers
        .blocks()
        .find(|(p, _, _)| *p == BOOT_UPDATE_CONF)
        .unwrap()
        .2
        .to_vec();
    assert_eq!(read_lines(&boot), ["AUTO_WRITE=no"]);
    assert_eq!(answers.auto_write(), Some(false));
    // Anything else is refused, as install-phone refuses it
    let bad = TestBundle::build(|_| {}, |c| c + "BOOT_UPDATE_AUTO_WRITE=maybe\n");
    assert!(matches!(bad.verify(), Err(Error::DeviceConf(_))));
}

/// The padding at its edges: what is left after the text becomes one
/// comment line, a lone newline, or nothing; text longer than the block,
/// or without its last newline, is refused.
#[test]
fn padding_fills_the_block_exactly() {
    use omamobile_installer_core::firstboot::pad_placeholder;
    for (n, tail) in [(4094usize, &b"#\n"[..]), (4095, b"\n"), (4096, b"")] {
        let mut text = vec![b'A'; n - 1];
        text.push(b'\n');
        let out = pad_placeholder(&text, 4096).unwrap();
        assert_eq!(out.len(), 4096, "{n}");
        assert!(out.ends_with(tail) && out.starts_with(&text), "{n}");
    }
    let out = pad_placeholder(b"A=1\n", 4096).unwrap();
    assert_eq!(&out[..5], b"A=1\n#");
    assert!(out[5..4095].iter().all(|b| *b == b' ') && out[4095] == b'\n');
    let mut long = vec![b'A'; 4096];
    long.push(b'\n');
    assert!(pad_placeholder(&long, 4096).is_err());
    assert!(pad_placeholder(b"A=1", 4096).is_err());
}

/// The session checks what it sends where the answers go: a piece cut
/// from the image without them (a front end that streamed it some other
/// way) is refused before it is sent, and userdata is never finished.
#[test]
fn a_piece_without_the_answers_is_never_sent() {
    let bundle = TestBundle::new();
    let phone = FakePhone::new("cheetah");
    let (flow, mut session) = support::drive::writing_session(&bundle, &phone, writing());
    let release = flow.release().unwrap().clone();
    let mut check = release.root_image_check();
    check.update(&bundle.root_zst);
    let root = check.finish().unwrap();
    let err = pollster::block_on(async {
        for write in &release.manifest().boot_writes {
            let image = release
                .verify_boot_image(write, bundle.image(&write.image))
                .unwrap();
            session.flash_boot(&image).await.unwrap();
        }
        // The stream as it was before the answers: the placeholders pass as built
        let mut stream = omamobile_installer_core::install::UserdataStream::new(1 << 20);
        session.userdata_streamed(&bundle.root_zst);
        stream.feed(&bundle.root_zst).unwrap();
        session
            .begin_userdata(&root, stream.image_len().unwrap())
            .await
            .unwrap();
        while let Some(piece) = stream.next_segment().unwrap() {
            session.flash_userdata_segment(&piece).await?;
        }
        stream.end().unwrap();
        while let Some(piece) = stream.next_segment().unwrap() {
            session.flash_userdata_segment(&piece).await?;
        }
        session.finish_userdata().await
    })
    .unwrap_err();
    assert!(
        matches!(&err, Error::Refused(s) if s.contains("something else than the owner's answers")),
        "{err}"
    );
    // The pieces before the placeholders went; none that covers them did
    let userdata = phone.partition("userdata");
    assert!(userdata[300 * 4096..303 * 4096]
        .iter()
        .all(|b| *b == support::fake::SENTINEL));
    assert_eq!(phone.0.borrow().active_slot, "a");
}

/// A Wi-Fi network's name is at most 32 bytes, as Wi-Fi and the phone's
/// first start count it: 32 accented letters are 64 bytes and refused.
#[test]
fn a_wifi_name_is_counted_in_bytes() {
    let check = |ssid: &str| {
        OwnerAnswers::check(&AnswersInput {
            username: "alex",
            password: "4812",
            password_again: "4812",
            hostname: "",
            timezone: "UTC",
            wifi_ssid: ssid,
            wifi_psk: "",
        })
        .is_ok()
    };
    assert!(check(&"a".repeat(32)));
    assert!(check(&"é".repeat(16)));
    assert!(!check(&"é".repeat(17)));
}

/// The longest answers the checks let through and the longest key
/// `check_ssh_key` takes fill the block exactly; a byte more of key, or of
/// time zone, is refused, plainly, before anything is written.
#[test]
fn the_longest_answers_and_key_fit_the_block_and_a_byte_more_is_refused() {
    use omamobile_installer_core::firstboot::{check_ssh_key, SSH_KEY_MAX};
    use omamobile_installer_core::flow::TIMEZONE_MAX;
    let release = TestBundle::new().verify().unwrap();
    let zone = format!("A/{}", "b".repeat(TIMEZONE_MAX - 2));
    let ssid = "\u{e9}".repeat(16); // 32 bytes
    let longest = OwnerAnswers::check(&AnswersInput {
        username: &"a".repeat(32),
        password: "not digits only",
        password_again: "not digits only",
        hostname: &"h".repeat(63),
        timezone: &zone,
        wifi_ssid: &ssid,
        wifi_psk: &"0".repeat(64),
    })
    .unwrap();
    let key = format!("ssh-ed25519 AAAA {}", "c".repeat(SSH_KEY_MAX - 17));
    assert_eq!(key.len(), SSH_KEY_MAX);
    // docs/DESIGN.md §12 gives the number
    assert_eq!(SSH_KEY_MAX, 3539);
    // 16 characters of salt, the longest
    let salt = "abcdefgh12345678";
    let answers = root_answers(&release, &longest, Some(&key), salt).unwrap();
    let (_, _, block) = answers
        .blocks()
        .find(|(p, _, _)| *p == FIRSTBOOT_CONF)
        .unwrap();
    assert_eq!(block.len(), 4096);
    // No room was left for padding: the key's line is the block's last
    let text = String::from_utf8(block.to_vec()).unwrap();
    assert!(
        text.ends_with(&format!(
            "SSH_KEY={key}\nCREDENTIAL=password\nWIFI_SSID={ssid}\nWIFI_PSK={}\n",
            "0".repeat(64)
        )),
        "{text}"
    );
    // The header is the one comment line: no padding was left
    assert_eq!(text.lines().filter(|l| l.starts_with('#')).count(), 1);
    assert!(answers.has_ssh_key());

    // A byte more of key: refused, and the message says why and what to use
    let longer = format!("{key}c");
    let why = root_answers(&release, &longest, Some(&longer), salt)
        .unwrap_err()
        .to_string();
    assert!(
        why.contains(&format!("at most {SSH_KEY_MAX}")) && why.contains("Ed25519"),
        "{why}"
    );
    assert!(!why.contains(&longer), "the key is not repeated");
    assert!(check_ssh_key(&longer).is_err());
    // A time zone longer than any: a problem with the answers, not a key's
    let long_zone = format!("A/{}", "b".repeat(TIMEZONE_MAX - 1));
    assert!(OwnerAnswers::check(&AnswersInput {
        username: "alex",
        password: "4812",
        password_again: "4812",
        hostname: "",
        timezone: &long_zone,
        wifi_ssid: "",
        wifi_psk: "",
    })
    .is_err());
    // Without a key, firstboot.conf has no SSH_KEY line
    let answers = root_answers(&release, &longest, None, salt).unwrap();
    assert!(!answers.has_ssh_key());
    let (_, _, block) = answers
        .blocks()
        .find(|(p, _, _)| *p == FIRSTBOOT_CONF)
        .unwrap();
    assert!(!block.windows(8).any(|w| w == b"SSH_KEY="));
}
