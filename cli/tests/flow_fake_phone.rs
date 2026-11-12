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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(typed.as_bytes())
        .unwrap();
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
    // First a file that isn't it, by name; then one that doesn't exist; then the zip
    let typed = format!(
        "yes\n{}\n{}\n{}\n{ANSWERS}",
        dir.join("MANIFEST").display(),
        dir.join("nothing-here.zip").display(),
        zip.display()
    );
    let (code, out, err) = flow("ready", &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Not taken: MANIFEST is not the file for this phone"));
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

#[test]
fn install_is_a_dry_run_or_nothing_and_the_test_key_is_for_the_pretend_phone() {
    let dir = fixture();
    let dir = dir.to_str().unwrap();
    // Refused from the arguments alone: no USB device is listed or opened
    let (code, _, err) = run(&["install", dir, "--serial", "<SERIAL>"], "");
    assert_eq!(code, 1);
    assert!(err.contains("only with --dry-run"), "{err}");
    let (code, _, err) = run(&["install", dir, "--dry-run"], "");
    assert_eq!(code, 1);
    assert!(err.contains("--serial SERIAL is required"));
    let (code, _, err) = run(&["install", dir, "--fake-phone", "ready", "--dry-run"], "");
    assert_eq!(code, 1);
    assert!(err.contains("install is for a real phone"));
    let (code, _, err) = run(
        &[
            "install",
            dir,
            "--serial",
            "<SERIAL>",
            "--dry-run",
            "--bootloader-slots-match",
            "x",
        ],
        "",
    );
    assert_eq!(code, 1);
    assert!(err.contains("unknown argument --bootloader-slots-match"));
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
