// SPDX-License-Identifier: MIT
//! `omamobile-install-link status|reopen` (INSTALL-LINK-PLAN §2.8, §3.3).
//!
//! `status` is read-only: the stage, the budget and, after the end, the
//! reason and what is missing. It prints no token, no boot id, no name.
//!
//! `reopen` is root's: after an `expired` end it grants one more setup
//! boot. It takes effect at the next boot: it records the boot it ran in,
//! and a daemon started in that boot exits before any gadget change.
use crate::daemon::done_reason;
use crate::durable;
use crate::found;
use crate::paths::Root;
use omamobile_link::phone::{reopen, Persistent, StateFile};
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "usage: omamobile-install-link status|reopen [--root DIR]";

pub fn main(args: Vec<String>) -> i32 {
    let mut root = PathBuf::from("/");
    let mut verb = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => match it.next() {
                Some(dir) if dir.starts_with('/') => root = PathBuf::from(dir),
                _ => return usage(),
            },
            "status" | "reopen" if verb.is_none() => verb = Some(arg),
            _ => return usage(),
        }
    }
    let root = Root::new(&root);
    match verb.as_deref() {
        Some("status") => status(&root),
        Some("reopen") => match reopen_link(&root) {
            Ok(left) => {
                println!("The install link is offered again at the next restart, for one boot ({left} setup boot left).");
                println!("Restart the phone, unlock it, tap \"Setup is not finished\" and run the installer on your computer.");
                0
            }
            Err(why) => {
                eprintln!("omamobile-install-link: {why}");
                1
            }
        },
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn state(root: &Root) -> StateFile {
    StateFile::read(fs::read(root.state()).ok().as_deref())
}

fn print_state(p: &Persistent) {
    println!("stage: {}", p.stage.as_str());
    println!("boot: {}", p.boot);
    println!("setup boots left: {}", p.boots_left());
    println!(
        "radio backup: {}",
        if p.backup_done { "done" } else { "none" }
    );
    println!(
        "factory image: {}",
        match &p.factory {
            None => "none",
            Some(f) if f.verified => "verified",
            Some(_) => "partial",
        }
    );
    println!("provisioning: {}", p.provision.as_str());
    if !p.optional_failed.is_empty() {
        println!("not set up: {}", p.optional_failed.join(" "));
    }
}

fn status(root: &Root) -> i32 {
    let done = fs::read_to_string(root.done()).ok();
    let state = state(root);
    match &done {
        Some(text) => {
            let mut words = text.split_whitespace();
            let reason = words.next().unwrap_or("unknown");
            let missing: Vec<&str> = words.collect();
            println!("link: ended");
            println!("reason: {reason}");
            if !missing.is_empty() {
                println!("missing: {}", missing.join(" "));
            }
            if let StateFile::Read(p) = &state {
                println!("stage: {}", p.stage.as_str());
            }
            if reason != "finished" {
                println!();
                println!("Setup was not finished: no Wi-Fi, sound, cameras or mobile network.");
                println!("To finish: run `sudo omamobile-install-link reopen`, restart the phone,");
                println!("then run the installer on your computer. Or install again.");
            }
        }
        None if root.marker().exists() => {
            println!(
                "link: {}",
                if root.closed().exists() {
                    "closed for this boot"
                } else if root.reopened().exists() {
                    "offered from the next restart"
                } else {
                    "offered"
                }
            );
            match &state {
                StateFile::Read(p) => print_state(p),
                StateFile::Missing => println!("stage: not started"),
                StateFile::Unreadable => println!("stage: unknown (the state file cannot be read)"),
            }
        }
        None => println!("link: none"),
    }
    0
}

/// One more setup boot after `expired`. The setup boots left then.
pub fn reopen_link(root: &Root) -> Result<u8, String> {
    if !root.scratch() && !rustix::process::geteuid().is_root() {
        return Err("reopen is root's: sudo omamobile-install-link reopen".into());
    }
    // A reopen that was cut short after it took `done` away is finished
    // here; anything else needs an `expired` end
    let begun = !root.done().exists() && !root.marker().exists() && root.reopened().exists();
    if !begun {
        match done_reason(root).as_deref() {
            Some("expired") => {}
            Some(other) => {
                return Err(format!(
                    "the link ended as `{other}`: only an `expired` end can be reopened"
                ))
            }
            None => return Err("the link has not ended: nothing to reopen".into()),
        }
    }
    let boot_id = found::boot_id(root);
    if boot_id.is_empty() {
        return Err("this boot's id cannot be read: not reopened".into());
    }
    let StateFile::Read(left) = state(root) else {
        return Err("the link's state file cannot be read: not reopened".into());
    };
    let granted = match reopen(&left) {
        Some(p) => p,
        // (cut short after the state file was written)
        None if begun && left.boots_left() == 1 => left,
        None => return Err("this phone's link cannot be reopened".into()),
    };
    let io = |e: std::io::Error| format!("not reopened: {e}");
    // The boot this ran in, first: from here on a daemon started in this
    // boot exits before any gadget change
    durable::write(&root.reopened(), format!("{boot_id}\n").as_bytes(), 0o600).map_err(io)?;
    durable::write(&root.state(), granted.to_json().as_bytes(), 0o600).map_err(io)?;
    durable::remove(&root.done()).map_err(io)?;
    // The marker last: with it the unit's condition holds again
    fs::create_dir_all(root.etc()).map_err(io)?;
    durable::write(&root.marker(), b"", 0o644).map_err(io)?;
    Ok(granted.boots_left())
}
