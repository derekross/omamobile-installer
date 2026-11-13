// SPDX-License-Identifier: MIT
//! The command-line tool driving the core's install flow against the
//! pretend phone (feature `fake-phone`), as a person would: answers typed on
//! standard input, screens read from standard output. No USB.
//!
//!   cargo test -p omamobile-installer-cli --features fake-phone
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest")
}

/// The stand-in factory zip, under the name the index gives it.
fn factory_zip() -> PathBuf {
    let mut found = std::fs::read_dir(fixture().join("factory"))
        .unwrap()
        .map(|e| e.unwrap().path());
    let zip = found
        .next()
        .expect("fixtures/selftest/factory holds the zip");
    assert!(found.next().is_none());
    zip
}

/// Run the tool; (exit code, standard output, standard error).
fn run(args: &[&str], typed: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A tool that refuses its arguments exits without reading its input,
    // and writing to it then fails: that is not this test's failure
    match child.stdin.take().unwrap().write_all(typed.as_bytes()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => panic!("{e}"),
        _ => {}
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn flow(scenario: &str, extra: &[&str], typed: &str) -> (i32, String, String) {
    let dir = fixture();
    let mut args = vec!["flow", dir.to_str().unwrap(), "--fake-phone", scenario];
    args.extend_from_slice(extra);
    run(&args, typed)
}

/// The output with every run of white space as one space: the tool wraps
/// the core's sentences to the terminal.
fn flat(out: &str) -> String {
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Each `==>` title, in order: the screens that were shown.
fn titles(out: &str) -> Vec<&str> {
    out.lines().filter_map(|l| l.strip_prefix("==> ")).collect()
}

const ANSWERS: &str = "alex\n4812\n4812\n\nEurope/Paris\n\n";
const ONLY_GETVAR: &str = "0 others; partitions written: 0";

#[test]
fn the_whole_flow_reaches_the_dry_run_and_writes_nothing() {
    let zip = factory_zip();
    let typed = format!("yes\n{ANSWERS}");
    let (code, out, err) = flow("ready", &["--factory-zip", zip.to_str().unwrap()], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        titles(&out),
        [
            "Read this once",
            "Connect the Pixel 7 Pro",
            "Google's factory image",
            "About you and the phone",
            "The dry run",
            "The dry run passed. Nothing was written"
        ]
    );
    // The fixture's images are on disk, so they were walked through
    assert!(out.contains("download 16384 bytes, flash:boot_b"), "{out}");
    assert!(out.contains("flash userdata: the root image, in "));
    assert!(out.contains("set_active:b"));
    assert!(flat(&out).contains("no step that writes to a phone"));
    assert!(out.contains(ONLY_GETVAR), "{out}");
    // Neither what was typed as the PIN nor a serial number is printed
    assert!(!out.contains("4812") && !out.contains("FAKESERIAL"));
}

#[test]
fn the_factory_zip_is_asked_for_until_it_is_the_right_file() {
    let zip = factory_zip();
    let dir = fixture();
    // Enter at Google's terms: the owner downloads the file themselves. Then
    // a file that isn't it, by name; one that doesn't exist; then the zip
    let typed = format!(
        "yes\n\n{}\n{}\n{}\n{ANSWERS}",
        dir.join("MANIFEST").display(),
        dir.join("nothing-here.zip").display(),
        zip.display()
    );
    let (code, out, err) = flow("ready", &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("! MANIFEST is not the file for this phone"));
    assert!(out.contains("There is no file"));
    // Shown again after each file that wasn't taken
    assert_eq!(
        titles(&out)
            .iter()
            .filter(|t| **t == "Google's factory image")
            .count(),
        3
    );
    assert!(out.contains(ONLY_GETVAR));
}

#[test]
fn a_locked_phone_is_shown_the_command_and_sent_nothing() {
    let zip = factory_zip();
    // yes; Enter after the unlock steps; then the answers
    let typed = format!("yes\n\n{ANSWERS}");
    let (code, out, err) = flow("locked", &["--factory-zip", zip.to_str().unwrap()], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    let shown = titles(&out);
    assert_eq!(
        shown[..3],
        [
            "Read this once",
            "Connect the Pixel 7 Pro",
            "Unlock the bootloader"
        ]
    );
    assert!(out.contains("\n           fastboot flashing unlock\n"));
    assert!(flat(&out).contains("does not send the unlock command"));
    // This version installs nothing, and the screen says so right under its
    // title, before the steps that erase the phone
    let at = |what: &str| {
        out.find(what)
            .unwrap_or_else(|| panic!("no {what:?} in {out}"))
    };
    assert!(at("==> Unlock the bootloader") < at("    !!  This version of the installer cannot"));
    assert!(at("    !!  This version of the installer cannot") < at("fastboot flashing unlock"));
    assert!(flat(&out).contains("!! This version of the installer cannot install Omarchy Mobile yet. It only checks the phone and lists what an install would write. Unlocking the bootloader erases the phone, and there is no reason to unlock it for this version: do not unlock the phone for it."));
    // The phone was asked whether it would take the command, and said yes
    assert!(flat(&out).contains("The phone says that \"OEM unlocking\" is already switched on"));
    assert!(!flat(&out).contains("Developer options"));
    assert!(out.contains("1 flashing get_unlock_ability, 0 others"));
    assert!(out.contains(ONLY_GETVAR), "{out}");

    // "OEM unlocking" still off: said, with how to switch it on. The input
    // ends at the unlock steps, as it would for an owner who goes to do that
    let (code, out, err) = flow("locked-oem-off", &[], "yes\n");
    assert_eq!(code, 1, "{out}\n{err}");
    assert_eq!(*titles(&out).last().unwrap(), "Unlock the bootloader");
    assert!(flat(&out).contains("The phone says that \"OEM unlocking\" is not switched on yet"));
    assert!(flat(&out).contains("1. Switch on \"OEM unlocking\": press Power to start Android."));
    assert!(out.contains("\n           fastboot flashing unlock\n"));
}

const SLOT_OVERRIDE: &str = "--maintainer-skip-bootloader-slot-check";
const SKIPPED: &str = "The bootloader-slot check was skipped by a maintainer override.";
const NOT_EVIDENCE: &str = "This run is not evidence that an install would be allowed.";

#[test]
fn the_maintainers_override_is_hidden_dry_run_only_and_said_on_the_screen() {
    let zip = factory_zip();
    let zip = zip.to_str().unwrap();
    let typed = format!("yes\n{ANSWERS}");
    // Without it the one-slot phone stops, and nothing mentions an override
    let (code, out, _) = flow("one-slot", &["--factory-zip", zip], &typed);
    assert_eq!(code, 2);
    assert!(!flat(&out).contains("override") && !out.contains("--"));
    // With it the dry run goes on to its end, and says what was skipped
    let (code, out, err) = flow("one-slot", &["--factory-zip", zip, SLOT_OVERRIDE], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The dry run finished with one check skipped. Nothing was written"
    );
    assert!(!titles(&out).contains(&"The dry run passed. Nothing was written"));
    // On every screen from the look at the phone on: four of them
    assert_eq!(flat(&out).matches(SKIPPED).count(), 4, "{out}");
    assert_eq!(flat(&out).matches(NOT_EVIDENCE).count(), 4, "{out}");
    // Set apart from the rest of the screen
    assert!(out.contains("\n    !!  The bootloader-slot check was skipped"));
    assert!(out.contains(ONLY_GETVAR), "{out}");
    // A phone that reports both slots: the override changes nothing
    let (code, out, _) = flow("ready", &["--factory-zip", zip, SLOT_OVERRIDE], &typed);
    assert_eq!(code, 0);
    assert!(!flat(&out).contains("override"));
    assert!(titles(&out).contains(&"The dry run passed. Nothing was written"));
    // Two versions that are known to differ still stop
    let (code, out, _) = flow("slots-differ", &[SLOT_OVERRIDE], "yes\n");
    assert_eq!(code, 2);
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The phone's two bootloader copies differ"
    );
    assert!(!flat(&out).contains("override"));

    // Not in the usage text
    let (code, out, err) = run(&[], "");
    assert_eq!(code, 1);
    assert!(err.contains("usage:") && err.contains("--dry-run"));
    for hidden in ["maintainer", "skip", "override"] {
        assert!(!err.contains(hidden) && !out.contains(hidden), "{err}");
    }
    // On a real phone it is for a dry run only: without --dry-run it is
    // refused from the arguments alone, and no USB device is listed or opened
    let dir = fixture();
    let dir = dir.to_str().unwrap();
    for args in [
        vec!["install", dir, "--serial", "<SERIAL>", SLOT_OVERRIDE],
        vec!["install", dir, SLOT_OVERRIDE, "--serial", "<SERIAL>"],
        vec!["install", dir, SLOT_OVERRIDE],
    ] {
        let (code, out, err) = run(&args, "");
        assert_eq!(code, 1, "{args:?}");
        assert!(
            err.contains("the bootloader-slot check can be skipped in a dry run only"),
            "{err}"
        );
        assert!(out.is_empty(), "{out}");
    }
}

#[test]
fn each_stop_ends_the_tool_with_the_cores_words() {
    for (scenario, title, footer) in [
        (
            "other-model",
            "This is another phone model",
            "once that is done",
        ),
        (
            "slot-b",
            "This phone runs from its second system copy",
            "will stop here again",
        ),
        (
            "one-slot",
            "The installer can't check the phone's second bootloader copy",
            "will stop here again",
        ),
        (
            "slots-differ",
            "The phone's two bootloader copies differ",
            "will stop here again",
        ),
        (
            "unknown-build",
            "This phone's Android version isn't one this release knows",
            "once that is done",
        ),
        (
            "untested-build",
            "This phone's Android build has not been tested with Omarchy Mobile yet",
            "will stop here again",
        ),
        (
            "untested-old-build",
            "This phone's Android build hasn't been tested with Omarchy Mobile",
            "once that is done",
        ),
    ] {
        let (code, out, err) = flow(scenario, &[], "yes\n");
        assert_eq!(code, 2, "{scenario}: {out}\n{err}");
        assert_eq!(*titles(&out).last().unwrap(), title, "{scenario}");
        assert!(flat(&out).contains(footer), "{scenario}: {out}");
        assert!(
            flat(&out).contains("It has written nothing to it."),
            "{scenario}"
        );
        assert!(out.contains(ONLY_GETVAR), "{scenario}: {out}");
    }
    // Not tested, and nothing newer that is: told to wait, not to update
    let (_, out, _) = flow("untested-build", &[], "yes\n");
    let out = flat(&out);
    assert!(out.contains("Nothing you can install today changes that."));
    assert!(out.contains("What you can do is wait"));
    assert!(out.contains("https://github.com/derekross/omarchy-mobile/issues"));
    assert!(!out.contains("System update") && !out.contains("install it"));
    // Not tested, and a newer build is: updating Android to it is the advice
    let (_, out, _) = flow("untested-old-build", &[], "yes\n");
    assert!(flat(&out).contains("If Android offers an update to it, install it"));
    // slot B is where a reinstall lives
    let zip = factory_zip();
    let typed = format!("yes\n{ANSWERS}");
    let (code, out, _) = flow(
        "slot-b",
        &["--reinstall", "--factory-zip", zip.to_str().unwrap()],
        &typed,
    );
    assert_eq!(code, 0, "{out}");
}

#[test]
fn the_warning_must_be_accepted_and_the_input_ending_stops() {
    // Enter instead of yes: the phone is never asked anything
    let (code, out, _) = flow("ready", &[], "\n");
    assert_eq!(code, 2);
    assert_eq!(titles(&out), ["Read this once", "Not accepted"]);
    assert!(out.contains("Nothing was sent to a phone."));
    assert!(out.contains("received 0 commands"), "{out}");
    // "y" is not yes
    let (code, out, _) = flow("ready", &[], "y\n");
    assert_eq!(code, 2);
    assert!(out.contains("received 0 commands"));
    // The input ends at a question: an error, not a guess
    let (code, out, err) = flow("ready", &[], "");
    assert_eq!(code, 1, "{out}");
    assert!(err.contains("no answer"));
    let (code, _, err) = flow("ready", &[], "yes\n");
    assert_eq!(code, 1);
    assert!(err.contains("no answer"));
}

/// The whole flow of a run that writes, typed: the warning, the answers,
/// then what is typed at the confirmation.
fn typed_install(at_the_confirmation: &str) -> String {
    format!("yes\n{ANSWERS}{at_the_confirmation}")
}

/// The commands the pretend phone says it received that were not questions.
fn others(out: &str) -> Vec<&str> {
    out.lines()
        .skip_while(|l| !l.starts_with("Those others, in the order it received them"))
        .skip(1)
        .map(str::trim)
        .collect()
}

#[test]
fn with_write_and_the_typed_codename_the_tool_writes_slot_b_and_userdata() {
    let zip = factory_zip();
    let extra = ["--write", "--factory-zip", zip.to_str().unwrap()];
    let (code, out, err) = flow("ready", &extra, &typed_install("cheetah\n"));
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        titles(&out),
        [
            "Read this once",
            "Connect the Pixel 7 Pro",
            "Google's factory image",
            "About you and the phone",
            "The dry run",
            "This is what the installer will write",
            "Writing to the phone",
            "Omarchy Mobile is written, and the phone is restarting"
        ]
    );
    // Before the confirmation: each partition with its sizes, what is
    // erased, what is not touched, and that the answers don't reach the phone
    let asked = out.find("Codename: ").unwrap();
    let before = flat(&out[..asked]);
    for line in [
        "boot_b: images/boot.img, 16384 bytes (the partition holds 16384 bytes)",
        "init_boot_b: images/init_boot.img, 8192 bytes (the partition holds 8192 bytes)",
        "vendor_boot_b: images/vendor_boot.img, 4096 bytes (the partition holds 16384 bytes)",
        "vendor_kernel_boot_b: images/vendor_kernel_boot.img, 12288 bytes (the partition holds 32768 bytes)",
        "dtbo_b: images/dtbo.img, 4096 bytes (the partition holds 8192 bytes)",
        "userdata: omamobile-rootfs-cheetah-2026.01.02-aarch64.img.zst, 2097152 bytes unpacked, sent in 6 pieces (the partition holds 8388608 bytes). ERASES userdata",
        "then: make slot B the active slot (set_active:b)",
        "then: restart the phone (reboot)",
        "userdata will be ERASED: everything Android stored on the phone is lost",
        "Slot A itself, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written.",
        "cannot put your answers on the phone",
        "To go ahead, type the phone's codename: cheetah",
    ] {
        assert!(before.contains(line), "not said before the confirmation: {line}\n{out}");
    }
    // Nothing was sent that changes a phone before the codename was typed
    assert!(!out[..asked].contains("flash:") && !out[..asked].contains("download:"));
    // What the phone received, all of it and in order: slot B's boot
    // images, userdata in pieces, slot B made active, the restart
    let sent = others(&out);
    assert_eq!(
        sent[..10],
        [
            "download:00004000",
            "flash:boot_b",
            "download:00002000",
            "flash:init_boot_b",
            "download:00001000",
            "flash:vendor_boot_b",
            "download:00003000",
            "flash:vendor_kernel_boot_b",
            "download:00001000",
            "flash:dtbo_b"
        ]
    );
    assert_eq!(sent.len(), 10 + 2 * 6 + 2, "{sent:?}");
    for pair in sent[10..22].chunks(2) {
        assert!(pair[0].starts_with("download:"));
        assert_eq!(pair[1], "flash:userdata");
    }
    assert_eq!(sent[22..], ["set_active:b", "reboot"]);
    assert!(out.contains("(partitions written: boot_b, dtbo_b, init_boot_b, userdata, vendor_boot_b, vendor_kernel_boot_b)"));
    // Each piece of userdata as the run's log keeps it: the blocks its
    // flash covered on the phone, and how long the phone took to answer
    let pieces: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("flash:userdata (piece "))
        .collect();
    assert_eq!(pieces.len(), 6, "{out}");
    let mut blocks = 0;
    for (n, line) in pieces.iter().enumerate() {
        let rest = line
            .strip_prefix(&format!("flash:userdata (piece {}: ", n + 1))
            .unwrap_or_else(|| panic!("{line}"));
        let (covered, rest) = rest.split_once(" blocks, ").unwrap();
        blocks += covered.parse::<u32>().unwrap();
        assert!(rest.contains(" KiB written on the phone, "), "{line}");
        assert!(rest.contains(" bytes sent, answered in "), "{line}");
        assert!(rest.ends_with(" s)"), "{line}");
    }
    // 2 MiB of 4096-byte blocks, each covered once
    assert_eq!(blocks, 512);
    assert!(out.contains("userdata, the piece that covered the most blocks: piece "));
    assert!(out.contains("userdata, the flash the phone took longest over: piece "));
    assert!(!out.contains("sent and not confirmed"));
    // After the last write: what the owner is told
    let after = flat(&out[out.find("==> Omarchy Mobile is written").unwrap()..]);
    assert!(after.contains("It now starts Omarchy Mobile from slot B."));
    assert!(after.contains("no user account"));
    assert!(!out.contains("4812") && !out.contains("FAKESERIAL"));
}

/// A copy of the test release in a directory of this test's own, so that a
/// test can change one of its files.
fn release_copy(name: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("flow-fake-phone-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&fixture(), &dir);
    dir
}

/// The dry run checks every file of the release, and the owner confirms
/// what it found. If a file is another one by the time the writes begin,
/// the tool must find that out before it writes anything: not after slot
/// B's boot partitions are already written, which is where the root image
/// used to be hashed, and not from a second opening of the file's name.
#[test]
fn a_release_file_that_changed_after_the_confirmation_is_found_before_the_first_write() {
    use std::io::Read;
    let zip = factory_zip();
    // The root image, and the last of the boot images
    for (n, victim) in ["root.img.zst", "images/dtbo.img"].iter().enumerate() {
        let dir = release_copy(&format!("changed-{n}"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
            .args(["flow", dir.to_str().unwrap(), "--fake-phone", "ready"])
            .args(["--write", "--factory-zip", zip.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        // Everything up to the confirmation: the dry run reads the files
        // as the release signed them, and passes
        stdin.write_all(typed_install("").as_bytes()).unwrap();
        stdin.flush().unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&out).contains("Codename: ") {
            let n = stdout.read(&mut buf).unwrap();
            assert!(
                n > 0,
                "the tool ended before the confirmation:\n{}",
                String::from_utf8_lossy(&out)
            );
            out.extend_from_slice(&buf[..n]);
        }
        // While the tool waits for the codename, one bit of the file
        // changes in place: the same name, the same size
        let path = dir.join(victim);
        let mut bytes = std::fs::read(&path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        stdin.write_all(b"cheetah\n").unwrap();
        drop(stdin);
        stdout.read_to_end(&mut out).unwrap();
        let code = child.wait().unwrap().code().unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_ne!(code, 0, "{victim}:\n{out}");
        let shown = titles(&out);
        assert_eq!(
            shown[shown.len() - 3..],
            [
                "This is what the installer will write",
                "Writing to the phone",
                "The install stopped before anything was written"
            ],
            "{victim}:\n{out}"
        );
        assert!(
            flat(&out).contains("doesn't match the signed manifest"),
            "{victim}:\n{out}"
        );
        // Not one command that changes a phone, or stages a change, left
        assert!(out.contains(ONLY_GETVAR), "{victim}:\n{out}");
        let after = &out[out.find("Codename: ").unwrap()..];
        assert!(
            !after
                .lines()
                .map(str::trim)
                .any(|l| l.starts_with("flash:") || l.starts_with("download:")),
            "{victim}:\n{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

const SLOTS_MATCH: &str = "--bootloader-slots-match";
const ON_YOUR_WORD: &str = "!! This phone tells the version of only one of its two bootloader copies. The installer has not read the other copy's version and cannot: it goes on because this run was started with the statement that someone read it from the phone and that it is the same version. If that is not so, the phone may not start from slot B.";

#[test]
fn a_phone_that_tells_one_bootloader_slot_is_written_on_the_owners_statement_only() {
    let zip = factory_zip();
    let zip = zip.to_str().unwrap();
    // Without the statement the one-slot phone stops, as it always did, and
    // the stop hands out no option
    let (code, out, _) = flow(
        "one-slot",
        &["--write", "--factory-zip", zip],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 2);
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The installer can't check the phone's second bootloader copy"
    );
    assert!(!out.contains("--") && out.contains(ONLY_GETVAR), "{out}");

    // With it, and the version the pretend phone reports: the install runs.
    // The confirmation says whose word it rests on, before the codename
    let stated = ["--write", "--factory-zip", zip, SLOTS_MATCH, "fake-1.0"];
    let (code, out, err) = flow("one-slot", &stated, &typed_install("cheetah\n"));
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        *titles(&out).last().unwrap(),
        "Omarchy Mobile is written, and the phone is restarting"
    );
    let asked = out.find("Codename: ").unwrap();
    let confirmation = &out[out
        .find("==> This is what the installer will write")
        .unwrap()..asked];
    assert!(flat(confirmation).contains(ON_YOUR_WORD), "{out}");
    assert!(!out[..asked].contains("flash:") && !out[..asked].contains("download:"));
    assert_eq!(others(&out).len(), 10 + 2 * 6 + 2);
    assert!(out.contains("(partitions written: boot_b, dtbo_b, init_boot_b, userdata, vendor_boot_b, vendor_kernel_boot_b)"));
    // Without the codename, the statement writes nothing either
    let (code, out, _) = flow("one-slot", &stated, &typed_install("yes\n"));
    assert_eq!(code, 2);
    assert!(out.contains(ONLY_GETVAR), "{out}");

    // Another version than the phone reports: a stop that names both
    let (code, out, err) = flow(
        "one-slot",
        &["--write", "--factory-zip", zip, SLOTS_MATCH, "fake-0.9"],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 2, "{out}\n{err}");
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The bootloader version that was stated is not this phone's"
    );
    assert!(flat(&out).contains("is fake-0.9, the same as the copy in use. The copy in use has another version: the phone reports fake-1.0."));
    assert!(out.contains(ONLY_GETVAR), "{out}");

    // A dry run takes it too, so the rehearsal of a write passes the same
    // stops as the write
    let (code, out, err) = flow(
        "one-slot",
        &["--factory-zip", zip, SLOTS_MATCH, "fake-1.0"],
        &format!("yes\n{ANSWERS}"),
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The dry run passed. Nothing was written"
    );
    assert!(flat(&out).contains(ON_YOUR_WORD));
    assert!(out.contains(ONLY_GETVAR), "{out}");

    // It is said once, with a version: refused from the arguments alone,
    // before any USB device is listed or opened
    let dir = fixture();
    let dir = dir.to_str().unwrap();
    let install = ["install", dir, "--serial", "<SERIAL>", "--write"];
    for (more, says) in [
        (
            vec![SLOTS_MATCH],
            "takes the bootloader version that was read from the phone's other slot",
        ),
        (
            vec![SLOTS_MATCH, ""],
            "takes the bootloader version that was read from the phone's other slot",
        ),
        (
            vec![SLOTS_MATCH, "fake-1.0", SLOTS_MATCH, "fake-1.0"],
            "--bootloader-slots-match is given once",
        ),
        // With the maintainer's override, a write is still refused
        (
            vec![SLOTS_MATCH, "fake-1.0", SLOT_OVERRIDE],
            "in a dry run only",
        ),
    ] {
        let args: Vec<&str> = install.iter().copied().chain(more).collect();
        let (code, out, err) = run(&args, "");
        assert_eq!(code, 1, "{args:?}");
        assert!(err.contains(says) && out.is_empty(), "{args:?}: {err}");
    }

    // And help says what it is: a statement the installer cannot check
    let (_, out, _) = run(&["help"], "");
    let help = flat(&out);
    for said in [
        "install ... --bootloader-slots-match VERSION",
        "your statement that the other copy's version was read from the phone and is VERSION",
        "It cannot check your statement and never switches the phone's slots to try",
        "Without it, such a phone stops before anything is written.",
    ] {
        assert!(help.contains(said), "help does not say: {said}\n{out}");
    }
}

#[test]
fn without_the_codename_or_without_write_nothing_is_written() {
    let zip = factory_zip();
    let zip = zip.to_str().unwrap();
    // Anything but the codename at the confirmation stops the install
    for wrong in ["yes\n", "\n", "Cheetah\n", "cheetah \n", "panther\n"] {
        let (code, out, err) = flow(
            "ready",
            &["--write", "--factory-zip", zip],
            &typed_install(wrong),
        );
        assert_eq!(code, 2, "{wrong:?}: {out}\n{err}");
        assert_eq!(*titles(&out).last().unwrap(), "Not confirmed");
        assert!(flat(&out).contains("Nothing was written."));
        assert!(out.contains(ONLY_GETVAR), "{wrong:?}: {out}");
    }
    // The input ends at the confirmation: no answer is not a yes
    let (code, out, err) = flow(
        "ready",
        &["--write", "--factory-zip", zip],
        &typed_install(""),
    );
    assert_eq!(code, 1, "{out}");
    assert!(err.contains("no answer"));
    assert!(out.contains(ONLY_GETVAR), "{out}");
    // Without --write the same typing ends at the dry run: the codename is
    // never asked for, and typing it changes nothing
    let (code, out, err) = flow(
        "ready",
        &["--factory-zip", zip],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The dry run passed. Nothing was written"
    );
    assert!(!out.contains("Codename"));
    assert!(out.contains(ONLY_GETVAR), "{out}");
    // A phone the flow stops for is not written with --write either
    for scenario in [
        "slot-b",
        "one-slot",
        "slots-differ",
        "untested-build",
        "other-model",
    ] {
        let (code, out, _) = flow(
            scenario,
            &["--write", "--factory-zip", zip],
            &typed_install("cheetah\n"),
        );
        assert_eq!(code, 2, "{scenario}: {out}");
        assert!(out.contains(ONLY_GETVAR), "{scenario}: {out}");
        assert!(!out.contains("This is what the installer will write"));
    }
    // The maintainer's override and --write: stopped before the warning,
    // with nothing asked of the phone
    let (code, out, _) = flow(
        "one-slot",
        &["--write", SLOT_OVERRIDE, "--factory-zip", zip],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 2, "{out}");
    assert_eq!(
        titles(&out),
        ["The bootloader-slot check can't be skipped in an install"]
    );
    assert!(out.contains("received 0 commands"), "{out}");
}

#[test]
fn help_says_what_write_erases_and_install_has_no_default() {
    let (code, out, _) = run(&["help"], "");
    assert_eq!(code, 0);
    let help = flat(&out);
    for said in [
        "install DIR --serial S --write",
        "slot B's boot partitions (the release's boot images, each to its partition with the _b suffix) and userdata",
        "userdata is ERASED: everything Android stored on the phone is lost",
        "Slot A itself, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are never written.",
        "waits for you to type the phone's codename; nothing is written before that",
        "starts with no user account",
        "install needs --dry-run or --write, one of them: there is no default.",
    ] {
        assert!(help.contains(said), "help does not say: {said}\n{out}");
    }
    // The maintainer's override is in no usage or help text
    assert!(!out.contains("maintainer") && !out.contains("skip"));
}

#[test]
fn install_needs_dry_run_or_write_and_the_test_key_is_for_the_pretend_phone() {
    let dir = fixture();
    let dir = dir.to_str().unwrap();
    // Each of these is refused from the arguments alone: no USB device is
    // listed or opened. (`install --serial S --write` is never run here: it
    // would look for a phone.)
    let (code, _, err) = run(&["install", dir, "--serial", "<SERIAL>"], "");
    assert_eq!(code, 1);
    assert!(
        err.contains("install needs --dry-run or --write, and has no default"),
        "{err}"
    );
    assert!(err.contains("--write erases the phone's userdata and writes slot B"));
    let (code, _, err) = run(&["install", dir, "--write"], "");
    assert_eq!(code, 1);
    assert!(err.contains("--serial SERIAL is required"));
    let (code, _, err) = run(
        &[
            "install",
            dir,
            "--serial",
            "<SERIAL>",
            "--write",
            "--dry-run",
        ],
        "",
    );
    assert_eq!(code, 1);
    assert!(
        err.contains("--dry-run and --write are two different runs"),
        "{err}"
    );
    let (code, _, err) = run(
        &[
            "install",
            dir,
            "--serial",
            "<SERIAL>",
            "--write",
            SLOT_OVERRIDE,
        ],
        "",
    );
    assert_eq!(code, 1);
    assert!(err.contains("can be skipped in a dry run only"), "{err}");
    let (code, _, err) = run(&["install", dir, "--fake-phone", "ready", "--write"], "");
    assert_eq!(code, 1);
    assert!(err.contains("install is for a real phone"));
    let (code, _, err) = run(&["install", dir, "--dry-run"], "");
    assert_eq!(code, 1);
    assert!(err.contains("--serial SERIAL is required"));
    let (code, _, err) = run(&["install", dir, "--fake-phone", "ready", "--dry-run"], "");
    assert_eq!(code, 1);
    assert!(err.contains("install is for a real phone"));
    // (`--bootloader-slots-match` was an unknown argument here until the
    // write path: what it is now has its own test above.)
    let (code, _, err) = run(
        &[
            "install",
            dir,
            "--serial",
            "<SERIAL>",
            "--dry-run",
            "--slots-match",
            "x",
        ],
        "",
    );
    assert_eq!(code, 1);
    assert!(err.contains("unknown argument --slots-match"));
    let (code, _, err) = run(&["flow", dir, "--serial", "<SERIAL>"], "");
    assert_eq!(code, 1);
    assert!(err.contains("pretend phone only"));
    let (code, _, err) = flow("no-such-phone", &[], "");
    assert_eq!(code, 1);
    assert!(err.contains("no pretend phone"));
    // A bundle signed by the test key is nothing to the commands that trust
    // the release keys: with the test key in the release key's place, it is
    // refused for its signer
    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join("as-a-release");
    let _ = std::fs::remove_dir_all(&copy);
    std::fs::create_dir_all(&copy).unwrap();
    for name in [
        "SHA256SUMS",
        "SHA256SUMS.sig",
        "manifest.json",
        "MANIFEST",
        "device.conf",
    ] {
        std::fs::copy(fixture().join(name), copy.join(name)).unwrap();
    }
    std::fs::copy(fixture().join("test-key.gpg"), copy.join("omamobile.gpg")).unwrap();
    let (code, _, err) = run(&["verify", copy.to_str().unwrap()], "");
    assert_eq!(code, 1);
    assert!(err.contains("not signed by an Omarchy Mobile key"), "{err}");
}

#[test]
fn a_reinstall_from_slot_b_is_written_with_the_version_slot_b_reports() {
    // The Pixel 7 Pro's case on a reinstall: it runs from slot B and tells
    // one bootloader copy's version, which is slot B's. --reinstall alone
    // takes it to a write; no statement about slot A is asked for
    let zip = factory_zip();
    let zip = zip.to_str().unwrap();
    let (code, out, err) = flow(
        "slot-b-one-slot",
        &["--write", "--reinstall", "--factory-zip", zip],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        *titles(&out).last().unwrap(),
        "Omarchy Mobile is written, and the phone is restarting"
    );
    let asked = out.find("Codename: ").unwrap();
    let confirmation = flat(
        &out[out
            .find("==> This is what the installer will write")
            .unwrap()..asked],
    );
    assert!(
        confirmation.contains("Bootloader fake-1.0: slot B's bootloader, which the phone already runs; the install switches no slot."),
        "{out}"
    );
    assert!(!out.contains("!!"), "no notice: {out}");
    assert!(!out[..asked].contains("flash:") && !out[..asked].contains("download:"));
    assert!(out.contains("(partitions written: boot_b, dtbo_b, init_boot_b, userdata, vendor_boot_b, vendor_kernel_boot_b)"));
    assert!(!out.contains("set_active:a"));

    // Without --reinstall: the slot B stop, before the bootloader is looked at
    let (code, out, _) = flow(
        "slot-b-one-slot",
        &["--write", "--factory-zip", zip],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 2);
    assert_eq!(
        *titles(&out).last().unwrap(),
        "This phone runs from its second system copy"
    );
    assert!(out.contains(ONLY_GETVAR), "{out}");

    // From slot A, --reinstall changes nothing: the one-slot stop
    let (code, out, _) = flow(
        "one-slot",
        &["--write", "--reinstall", "--factory-zip", zip],
        &typed_install("cheetah\n"),
    );
    assert_eq!(code, 2);
    assert_eq!(
        *titles(&out).last().unwrap(),
        "The installer can't check the phone's second bootloader copy"
    );
    assert!(out.contains(ONLY_GETVAR), "{out}");

    // And help says what --reinstall asserts, and what a wrong one costs
    let (_, out, _) = run(&["help"], "");
    let help = flat(&out);
    for said in [
        "install ... --reinstall",
        "the copy it tells is slot B's, the bootloader the phone already runs",
        "if Android is, the install overwrites the Android the phone is using",
        "A reinstall from slot B does not need it (--reinstall).",
    ] {
        assert!(help.contains(said), "help does not say: {said}\n{out}");
    }
}
