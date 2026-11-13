// SPDX-License-Identifier: MIT
//! `omamobile-link-gadget` against a fake configfs tree (gadget/test): no
//! root, no gadget, no USB. Its run against the kernel is stage L3's.
use std::process::Command;

#[test]
fn the_gadget_script_against_a_fake_configfs_tree() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/gadget");
    let out = Command::new("bash")
        .arg(format!("{dir}/test"))
        .output()
        .expect("bash runs");
    let text = String::from_utf8_lossy(&out.stdout);
    print!("{text}");
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "gadget/test failed");
    assert!(!text.contains("not ok"));
}
