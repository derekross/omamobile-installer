// SPDX-License-Identifier: MIT
//! `tools/l3-run-as-root` without root: its argument checks, its refusal to
//! run without sudo, and its dry run, which prints every root command and
//! runs none.
use std::process::Command;

fn script(args: &[&str]) -> (i32, String) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/l3-run-as-root");
    let out = Command::new("bash")
        .arg(path)
        .args(args)
        .env_remove("SUDO_USER")
        .output()
        .expect("bash runs");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn root() -> bool {
    rustix::process::geteuid().is_root()
}

#[test]
fn wrong_arguments_are_refused() {
    for args in [
        &["--bogus"][..],
        &["--speeds", "warp"],
        &["--speeds"],
        &["--big-bytes", "lots"],
        &["--only", "a;rm"],
    ] {
        let (rc, text) = script(args);
        assert_eq!(rc, 2, "{args:?}: {text}");
        assert!(text.contains("usage:"), "{text}");
    }
}

#[test]
fn it_needs_sudo() {
    if root() {
        return eprintln!("run as root: the refusal is not what this test sees");
    }
    let (rc, text) = script(&["--speeds", "high"]);
    assert_eq!(rc, 1, "{text}");
    assert!(text.contains("needs root"), "{text}");
}

#[test]
fn the_dry_run_prints_every_root_command_and_changes_nothing() {
    if root() {
        return eprintln!("run as root: the dry run needs the account the tests would run as");
    }
    let state = std::path::Path::new("/run/omamobile-l3.state");
    let had_state = state.exists();
    let (rc, text) = script(&["--dry-run", "--speeds", "high super", "--big-bytes", "0"]);
    if text.contains("another run (pid") {
        return eprintln!("a real run is going on: the dry run rightly refuses ({text})");
    }
    assert_eq!(rc, 0, "{text}");
    for want in [
        "+ modprobe dummy_hcd is_high_speed=1 is_super_speed=0",
        "+ modprobe dummy_hcd is_high_speed=1 is_super_speed=1",
        "+ modprobe usb_f_fs",
        "+ mkdir /sys/kernel/config/usb_gadget/omamobile-l3-",
        "/functions/SourceSink.base",
        "+ echo dummy_udc.0 > /sys/kernel/config/usb_gadget/omamobile-l3-",
        "+ udevadm control --reload",
        "LINKD_L3=usb L3_SPEED=high",
        "LINKD_L3=usb L3_SPEED=super",
        "cleanup: putting the computer back",
        "+ rmmod dummy_hcd",
        "+ rmmod usb_f_fs",
        "+ rm -f /run/udev/rules.d/99-omamobile-l3-",
    ] {
        assert!(text.contains(want), "the dry run lacks {want:?}:\n{text}");
    }
    // Its own gadget only, and never another UDC
    for line in text.lines().filter(|l| l.starts_with("+ ")) {
        if line.contains("/usb_gadget/") {
            assert!(line.contains("/usb_gadget/omamobile-l3-"), "{line}");
        }
        if line.contains("/UDC") && line.starts_with("+ echo ") && !line.starts_with("+ echo ''") {
            assert!(line.contains("echo dummy_udc."), "{line}");
        }
    }
    assert_eq!(state.exists(), had_state, "the dry run wrote no state");
}
