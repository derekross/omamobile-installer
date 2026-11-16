// SPDX-License-Identifier: MIT
//! The command-line tool through the install link after the write, against
//! the pretend phone (feature `fake-phone`): a test release whose root image
//! offers the link (fixtures/selftest-link), a write to the pretend fastboot
//! phone, then the pretend phone speaking the link (the protocol crate's
//! stand-in for the phone's daemon) to a phone that is ready. No USB.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest-link")
}

fn factory_zip() -> PathBuf {
    let mut found = std::fs::read_dir(fixture().join("factory"))
        .unwrap()
        .map(|e| e.unwrap().path());
    found.next().unwrap()
}

/// A scratch directory of this test's own, under the build's target.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("link-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str], typed: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(typed.as_bytes());
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// The warning, the answers (with no SSH key, name or email), the codename,
/// and at the link the code as the pretend phone shows it.
const TYPED: &str = "yes\nalex\n4812\n4812\n\nEurope/Paris\n\n\n\n\ncheetah\npretend\n";

fn install(name: &str, typed: &str, env: &[(&str, &str)]) -> (i32, String, PathBuf, PathBuf) {
    let dir = scratch(name);
    let (work, backup) = (dir.join("work"), dir.join("backup"));
    let zip = factory_zip();
    let fixture = fixture();
    let (code, out, err) = run(
        &[
            "flow",
            fixture.to_str().unwrap(),
            "--fake-phone",
            "ready",
            "--write",
            "--factory-zip",
            zip.to_str().unwrap(),
            "--work-dir",
            work.to_str().unwrap(),
            "--backup-dir",
            backup.to_str().unwrap(),
        ],
        typed,
        env,
    );
    (code, format!("{out}\n{err}"), work, backup)
}

fn titles(out: &str) -> Vec<&str> {
    let mut t: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("==> ")).collect();
    t.dedup();
    t
}

#[test]
fn a_write_goes_on_through_the_link_to_a_phone_that_is_ready() {
    let (code, out, work, backup) = install("ready", TYPED, &[]);
    assert_eq!(code, 0, "{out}");
    let t = titles(&out);
    let from = t
        .iter()
        .position(|t| *t == "Omarchy Mobile is written, and the phone is restarting")
        .expect("the write ran through");
    assert_eq!(
        &t[from + 1..],
        [
            "Waiting for the Pixel 7 Pro's first start",
            "Type the code the phone shows",
            "Connecting to the phone",
            "Giving the phone your answers",
            "Copying the phone's radio partitions to this computer",
            "Sending Google's factory image to the phone",
            "The phone is setting itself up",
            "The phone is installing its firmware",
            "The phone is restarting",
            "Checking the phone",
            "Your Pixel 7 Pro is ready",
        ],
        "{out}"
    );
    assert!(
        out.contains("first boot run 1 times, provisioning 1, restarts 1, ended as finished"),
        "{out}"
    );
    // The backup as tools/nv-backup writes it, private
    let sums = std::fs::read_to_string(backup.join("SHA256SUMS")).unwrap();
    assert_eq!(sums.lines().count(), 6, "{sums}");
    for line in sums.lines() {
        let (digest, file) = line.split_once("  ").unwrap();
        let bytes = std::fs::read(backup.join(file)).unwrap();
        let mut h = omamobile_link::digest::Hasher::new();
        h.update(&bytes);
        assert_eq!(
            digest,
            omamobile_link::digest::to_hex(&h.finish()),
            "{file}"
        );
    }
    assert!(std::fs::read_to_string(backup.join("NV-BACKUP"))
        .unwrap()
        .contains("device: cheetah\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&backup), 0o700);
        for e in std::fs::read_dir(&backup).unwrap() {
            assert_eq!(mode(&e.unwrap().path()), 0o600);
        }
        assert_eq!(mode(&work.join("link/record.json")), 0o600);
    }
    // The record says it is over, and holds no token any more
    let record = std::fs::read_to_string(work.join("link/record.json")).unwrap();
    assert!(
        record.contains("\"finished\": true") && record.contains("\"token\": null"),
        "{record}"
    );
    assert!(!record.contains("4812"));
}

#[test]
fn a_cable_pulled_in_the_middle_is_taken_up_again() {
    for at in ["3", "12", "30", "45"] {
        let (code, out, _, _) = install(
            &format!("pull-{at}"),
            TYPED,
            &[("OMAMOBILE_PRETEND_LINK", &format!("unplug-at={at}"))],
        );
        assert_eq!(code, 0, "pulled at {at}: {out}");
        assert!(out.contains("cable pulled 1 times"), "{at}: {out}");
        assert!(out.contains("ended as finished"), "{at}");
    }
}

#[test]
fn a_camera_only_failure_is_said_at_the_end() {
    let (code, out, _, _) = install(
        "camera",
        TYPED,
        &[("OMAMOBILE_PRETEND_LINK", "camera-fails")],
    );
    assert_eq!(code, 0, "{out}");
    let flat: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("Cameras: not set up."), "{out}");
}

#[test]
fn first_boot_that_failed_is_tried_again_after_enter() {
    let typed = format!("{TYPED}\n");
    let (code, out, _, _) = install(
        "firstboot",
        &typed,
        &[("OMAMOBILE_PRETEND_LINK", "firstboot-fails")],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("==> Setup on the phone stopped"), "{out}");
    assert!(out.contains("first boot run 2 times"), "{out}");
}

#[test]
fn without_a_backup_folder_the_tool_asks_for_a_new_empty_one() {
    let dir = scratch("ask");
    let (work, backup, full) = (dir.join("work"), dir.join("backup"), dir.join("full"));
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("x"), b"x").unwrap();
    let typed = format!("{TYPED}{}\n{}\n", full.display(), backup.display());
    let zip = factory_zip();
    let fixture = fixture();
    let (code, out, err) = run(
        &[
            "flow",
            fixture.to_str().unwrap(),
            "--fake-phone",
            "ready",
            "--write",
            "--factory-zip",
            zip.to_str().unwrap(),
            "--work-dir",
            work.to_str().unwrap(),
        ],
        &typed,
        &[],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("==> Choose where the radio backup goes"),
        "{out}"
    );
    assert!(out.contains("is not empty"), "{out}");
    assert!(backup.join("SHA256SUMS").is_file());
}

#[test]
fn a_dry_run_of_a_release_with_the_link_never_reaches_it() {
    let zip = factory_zip();
    let fixture = fixture();
    let (code, out, _) = run(
        &[
            "flow",
            fixture.to_str().unwrap(),
            "--fake-phone",
            "ready",
            "--factory-zip",
            zip.to_str().unwrap(),
        ],
        "yes\nalex\n4812\n4812\n\nEurope/Paris\n\n",
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("install link, held in memory"), "{out}");
    assert!(!out.contains("first start"), "{out}");
}
