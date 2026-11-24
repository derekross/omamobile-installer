// SPDX-License-Identifier: MIT
//! The command-line tool's first install of a phone that tells one
//! bootloader copy's version, against the pretend phone: the other slot
//! measured with the owner's answer, the restarts watched, the record kept
//! in the work directory and used by the next run. No USB.
//!
//!   cargo test -p omamobile-installer-cli --features fake-phone
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest-probe")
}

fn factory_zip() -> PathBuf {
    std::fs::read_dir(fixture().join("factory"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .next()
        .unwrap()
}

fn work_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("slot-probe-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `flow` on the probe fixture; (exit code, standard output, standard error).
fn flow(scenario: &str, work: &Path, extra: &[&str], typed: &str) -> (i32, String, String) {
    let dir = fixture();
    let zip = factory_zip();
    let mut args = vec![
        "flow",
        dir.to_str().unwrap(),
        "--fake-phone",
        scenario,
        "--factory-zip",
        zip.to_str().unwrap(),
        "--work-dir",
        work.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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

fn titles(out: &str) -> Vec<&str> {
    out.lines().filter_map(|l| l.strip_prefix("==> ")).collect()
}

fn flat(out: &str) -> String {
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The commands the pretend phone received that are not questions, as the
/// tool lists them at the end of a pretend run.
fn others(out: &str) -> Vec<&str> {
    let Some(at) = out.find("Those others, in the order it received them") else {
        return Vec::new();
    };
    out[at..]
        .lines()
        .skip(1)
        .filter_map(|l| l.strip_prefix("    "))
        .collect()
}

const ANSWERS: &str = "alex\n4812\n4812\n\nEurope/Paris\n\n";

#[test]
fn a_first_install_measures_the_other_slot_and_writes() {
    let work = work_dir("measure");
    let typed = format!("yes\nmeasure\n{ANSWERS}cheetah\n");
    let (code, out, err) = flow("first-install", &work, &["--write"], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    let shown = titles(&out);
    assert_eq!(shown[2], "Measure the phone's second bootloader copy?");
    assert!(shown.contains(&"Omamobile is written, and the phone is restarting"));
    let text = flat(&out);
    assert!(text
        .contains("Measure slot B's bootloader (two restarts into the bootloader) (type measure)"));
    assert!(text.contains("the phone left the bus and came back in Fastboot Mode"));
    assert!(text.contains(
        "Bootloader fake-1.0 on both copies: the installer measured slot B's bootloader"
    ));
    let received = others(&out);
    assert_eq!(
        received[..4],
        [
            "set_active:b",
            "reboot-bootloader",
            "set_active:a",
            "reboot-bootloader"
        ]
    );
    assert_eq!(received[received.len() - 2..], ["set_active:b", "reboot"]);
    // Nothing kept: the phone came back on slot A
    assert!(!work.join("slot-probe").exists());
}

#[test]
fn a_dry_run_never_measures() {
    let work = work_dir("dry");
    let (code, out, err) = flow("first-install", &work, &[], "yes\n");
    assert_eq!(code, 2, "{out}\n{err}");
    assert!(flat(&out).contains("so a dry run does not do it"), "{out}");
    assert!(out.contains("0 others; partitions written: 0"), "{out}");
}

#[test]
fn not_measuring_stops_with_nothing_sent() {
    let work = work_dir("stop");
    let (code, out, err) = flow("first-install", &work, &["--write"], "yes\nno thanks\n");
    assert_eq!(code, 2, "{out}\n{err}");
    assert_eq!(titles(&out).last(), Some(&"Not measured"));
    assert!(out.contains("0 others; partitions written: 0"), "{out}");
}

#[test]
fn a_phone_left_on_slot_b_is_put_back_by_the_next_run() {
    let work = work_dir("os");
    // It starts an operating system from slot B and does not come back
    let (code, out, err) = flow("first-install-os", &work, &["--write"], "yes\nmeasure\n");
    assert_eq!(code, 2, "{out}\n{err}");
    assert_eq!(
        titles(&out).last(),
        Some(&"The phone did not come back from slot B's bootloader")
    );
    let text = flat(&out);
    assert!(text.contains("the phone left the bus and did not come back in time"));
    assert!(
        text.contains("slot B may still be the active slot"),
        "{text}"
    );
    assert_eq!(others(&out), ["set_active:b", "reboot-bootloader"]);
    let record = std::fs::read_to_string(work.join("slot-probe")).unwrap();
    assert!(record.starts_with("omamobile-slot-probe 1\n"));
    // A new run with the same work directory offers to put slot A back
    // first, and does; then the record is gone
    let (code, out, err) = flow(
        "first-install",
        &work,
        &["--write"],
        "yes\nput-back\n\nstop\n",
    );
    assert_eq!(code, 2, "{out}\n{err}");
    let shown = titles(&out);
    assert_eq!(shown[2], "Put slot A back as the active slot");
    assert_eq!(shown[3], "Connect the Pixel 7 Pro");
    assert_eq!(shown[4], "Measure the phone's second bootloader copy?");
    // This pretend phone runs slot A's bootloader: the flag is set, no restart
    assert_eq!(others(&out), ["set_active:a"]);
    assert!(!work.join("slot-probe").exists());
}

#[test]
fn copies_that_differ_stop_after_slot_a_is_back() {
    let work = work_dir("differ");
    let (code, out, err) = flow(
        "first-install-differ",
        &work,
        &["--write"],
        "yes\nmeasure\n",
    );
    assert_eq!(code, 2, "{out}\n{err}");
    assert_eq!(
        titles(&out).last(),
        Some(&"The phone's two bootloader copies differ")
    );
    assert!(flat(&out).contains("slot B's is fake-0.9"));
    assert_eq!(
        others(&out),
        [
            "set_active:b",
            "reboot-bootloader",
            "set_active:a",
            "reboot-bootloader"
        ]
    );
    assert!(out.contains("partitions written: 0"));
}
