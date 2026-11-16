// SPDX-License-Identifier: MIT
//! The whole flow against a real daemon process on a scratch root, its
//! link a socket pair, its partitions regular files (INSTALL-LINK-PLAN
//! §11.1, L2).
#![cfg(target_os = "linux")]
mod common;

use common::*;
use omamobile_link::digest::{sha256, to_hex};
use omamobile_link::host::{Command, Event};
use omamobile_link::msg::Stage;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

fn mode(p: &Phone, abs: &str) -> u32 {
    fs::metadata(p.path(abs)).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_whole_install_from_the_code_to_finished() {
    let mut p = Phone::new();
    let mut d = p.daemon();
    assert!(d.settled());
    // The state file exists from the first start, before the gadget change
    assert_eq!(p.state().unwrap().stage, Stage::Waiting);
    assert_eq!(p.state().unwrap().boot_id, p.boot_id());
    assert_eq!(p.gadget(), ["add"]);
    let mut i = Installer::new();
    i.pair_at_screen(&mut d);

    // The answers: checked by first boot's own program, placed, the unit started
    i.step(&mut d, Command::Answers(answers()));
    assert!(
        i.saw(|e| *e == Event::AnswersAccepted),
        "{:?}",
        i.host.events
    );
    assert_eq!(p.state().unwrap().stage, Stage::Firstboot);
    assert_eq!(
        p.read("/etc/omamobile/firstboot.conf").unwrap(),
        answers().firstboot_conf()
    );
    assert_eq!(mode(&p, "/etc/omamobile/firstboot.conf"), 0o600);
    assert_eq!(
        p.read("/fake-systemd/candidate-modes").unwrap().trim(),
        "600"
    );
    assert!(
        !p.exists("/run/omamobile-install-link/firstboot.conf"),
        "the candidate is gone"
    );
    assert!(p
        .read("/etc/omamobile/boot-update.conf")
        .unwrap()
        .ends_with("AUTO_WRITE=yes\n"));
    assert_eq!(p.unit("omamobile-firstboot.service"), "activating");

    // First boot ends, with no session open or with one: `status` says so
    p.finish_firstboot(&[]);
    i.step(&mut d, Command::AwaitFirstboot);
    assert!(i.saw(|e| *e == Event::FirstbootDone), "{:?}", i.host.events);
    assert_eq!(p.state().unwrap().stage, Stage::FirstbootDone);
    assert_eq!(d.screen().wait(|l| l == "off").as_deref(), Some("off"));

    // The radio backup: the computer's copies are the partitions' bytes
    i.step(&mut d, Command::Backup);
    assert!(i.saw(|e| *e == Event::BackupDone), "{:?}", i.host.events);
    let mut sums = String::new();
    for (n, (name, _, checked)) in PARTS.iter().enumerate() {
        let on_phone = fs::read(p.path(&format!("/dev/sda{}", n + 1))).unwrap();
        assert_eq!(i.host.backup[*name], on_phone, "{name}");
        if *checked {
            sums.push_str(&format!("{}  {name}.img\n", to_hex(&sha256(&on_phone))));
        }
    }
    // The pending checksums, in sha256sum's format; not yet in /etc
    assert_eq!(
        p.read("/var/lib/omamobile/install-link/modem-nv.sha256")
            .unwrap(),
        sums
    );
    assert!(!p.exists("/etc/omamobile/modem-nv.sha256"));
    assert!(p.state().unwrap().backup_done);

    // The factory image
    i.step(&mut d, Command::Factory(factory_begin(&image())));
    assert!(
        i.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        i.host.events
    );
    assert_eq!(
        fs::read(p.path("/var/lib/omamobile/install-link/factory.zip")).unwrap(),
        image()
    );
    assert_eq!(
        mode(&p, "/var/lib/omamobile/install-link/factory.zip"),
        0o600
    );
    let f = p.state().unwrap().factory.unwrap();
    assert!(f.verified && f.synced == IMAGE_LEN as u64);

    // Provisioning: the worker as a transient unit, with the fixed paths
    i.host.take_events();
    i.command(&mut d, Command::Provision);
    i.until(&mut d, "the worker is started", |_| {
        p.provision_starts() == 1
    });
    let run = p.systemd_log().pop().unwrap();
    assert!(run.contains("--unit=omamobile-provision-run"), "{run}");
    assert!(
        run.ends_with(&format!(
            "/usr/bin/omamobile-provision first-install --result {0}/provision.result {0}/factory.zip {0}/modem-nv.sha256",
            p.path("/var/lib/omamobile/install-link").display()
        )),
        "{run}"
    );
    p.finish_provision(None);
    i.until(&mut d, "provisioning ends", |i| !i.host.host.busy());
    assert!(
        i.saw(|e| matches!(e, Event::ProvisionDone { optional_failed, .. } if optional_failed.is_empty())),
        "{:?}",
        i.host.events
    );
    assert_eq!(p.state().unwrap().stage, Stage::Provisioned);
    assert_eq!(p.image_len(), None, "the image is deleted");

    // The planned restart: asked of systemd once, by its name
    i.host.take_events();
    i.command(&mut d, Command::Restart);
    i.until(&mut d, "the restart is asked for", |_| p.reboots() == 1);
    d.kill();
    p.new_boot();
    let mut d = p.daemon();
    assert!(d.settled());
    assert_eq!(p.state().unwrap().boot, 2);
    assert_eq!(p.state().unwrap().setup_boots, 1);
    i.pump(&mut d, Duration::from_millis(5));
    i.connect(&mut d);
    i.until(&mut d, "back after the restart", |i| !i.host.host.busy());
    assert!(i.saw(|e| *e == Event::Restarted), "{:?}", i.host.events);

    // The smoke report waits for the session; the agent's connection is it
    d.agent();
    i.step(&mut d, Command::Smoke);
    assert!(
        i.saw(|e| matches!(e, Event::SmokeReport(_))),
        "{:?}",
        i.host.events
    );

    // The end
    i.step(&mut d, Command::Finish);
    assert!(i.saw(|e| *e == Event::Finished), "{:?}", i.host.events);
    assert_eq!(d.wait_exit(), 0);
    assert_eq!(p.done().as_deref(), Some("finished"));
    assert!(!p.marker());
    assert!(p.state().is_none(), "the state and the token are deleted");
    assert!(!p.exists("/var/lib/omamobile/install-link/modem-nv.sha256"));
    assert_eq!(
        p.gadget(),
        ["add", "boot", "add", "remove"],
        "the gadget is put back"
    );
    assert_eq!(p.firstboot_runs, 1);
    assert_eq!(p.firstboot_ran_with, answers().firstboot_conf().as_bytes());
    assert_eq!(p.reboots(), 1);

    // The journal names types and lengths, and nothing of §9's list
    let journal = p.journal();
    for secret in ["saltsalt", "a passphrase", "<SSID>", "owner@", SERIAL] {
        assert!(!journal.contains(secret), "the journal holds {secret}");
    }
    let (status, out) = p.command("status");
    assert_eq!(status, 0);
    assert_eq!(out, "link: ended\nreason: finished\n");
}
