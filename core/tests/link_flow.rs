// SPDX-License-Identifier: MIT
//! After the write: the install link from the phone's first start to a
//! phone that is ready, against the pretend phone that speaks the link (the
//! protocol crate's stand-in for the phone's daemon) and a shell in memory.
//! Every run begins with a real write through the flow against the fake
//! fastboot phone, for a test release whose root image offers the link.
mod support;
use omamobile_installer_core::firstboot::Extras;
use omamobile_installer_core::flow::{Flow, FlowOptions};
use omamobile_installer_core::link::pretend::{parts, Clock, MemoryShell, PretendPhone};
use omamobile_installer_core::link::{LinkFlow, LinkRecord};
use omamobile_link::digest::{sha256, to_hex};
use omamobile_link::msg::Stage;
use pollster::block_on;
use support::bundle::TestBundle;
use support::drive::{answers, drive};
use support::fake::{FakePhone, SERIAL};

const SALT: &str = "saltsaltsaltsalt";

fn link_bundle() -> TestBundle {
    TestBundle::build(
        |m| m["install_link"] = serde_json::json!({"protocol": 1}),
        |c| c,
    )
}

/// A write through the flow to `installed`, and the link handed over.
fn written() -> (Flow, LinkFlow, TestBundle) {
    let bundle = link_bundle();
    let phone = FakePhone::scenario("ready").unwrap();
    let options = FlowOptions {
        write: true,
        ..Default::default()
    };
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, &bundle, &phone, "cheetah", "installed");
    let screen = flow.screen();
    assert_eq!(screen.state, "installed", "{:?}", flow.step());
    assert_eq!(screen.action, "link", "a release with the link goes on");
    let link = flow
        .link(&answers(), &Extras::default(), SALT, "0.0.1", false)
        .unwrap();
    (flow, link, bundle)
}

struct Rig {
    link: LinkFlow,
    phone: PretendPhone,
    shell: MemoryShell,
}

fn rig() -> Rig {
    let (_, link, bundle) = written();
    let clock = Clock::default();
    Rig {
        link,
        phone: PretendPhone::first_start(SERIAL, "cheetah", clock.clone(), 1),
        shell: MemoryShell::new(clock, bundle.factory_zip.clone()),
    }
}

impl Rig {
    /// Do what each screen asks, as an owner at the computer would, until
    /// the link's flow is ready, stopped, or the front end left.
    fn go(&mut self) -> &'static str {
        for _ in 0..200 {
            block_on(self.link.run(&mut self.phone, &mut self.shell)).unwrap();
            if self.shell.stuck {
                panic!(
                    "the run never ended: at {:?}, the phone at {:?}, clock {}",
                    self.link.screen(),
                    self.phone.sim.phone.stage(),
                    self.shell.clock.now()
                );
            }
            if self.shell.left {
                return "left";
            }
            let screen = self.link.screen();
            match screen.action {
                "pair" => {
                    let code = self.phone.shows().expect("the phone shows a code");
                    self.link.pair(&code).unwrap();
                }
                "backup-dir" => self.link.backup_place_chosen().unwrap(),
                "answers" => {
                    let a = omamobile_installer_core::link::link_answers(
                        &answers(),
                        &Extras::default(),
                        SALT,
                    )
                    .unwrap();
                    self.link.answers(a).unwrap();
                }
                "retry" => self.link.retry().unwrap(),
                _ => return screen.state,
            }
        }
        panic!("the link's flow never ended: {:?}", self.link.screen());
    }
}

/// A release whose root image offers the install link takes the answers
/// there, after the write, whether or not its image also keeps the answers'
/// placeholders: its image is written exactly as signed, the confirmation
/// says where the answers go, and the session refuses answers for its image
/// (docs/DESIGN.md §2, M1).
#[test]
fn a_release_with_the_link_is_written_as_signed_and_takes_its_answers_over_the_link() {
    let without_placeholders = TestBundle::build(
        |m| {
            m["install_link"] = serde_json::json!({"protocol": 1});
            m["root_image"]
                .as_object_mut()
                .unwrap()
                .remove("placeholders");
        },
        |c| c,
    );
    for (bundle, has_places) in [(link_bundle(), true), (without_placeholders, false)] {
        let phone = FakePhone::scenario("ready").unwrap();
        let options = FlowOptions {
            write: true,
            ..Default::default()
        };
        let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
        drive(&mut flow, &bundle, &phone, "cheetah", "confirm");
        let confirm = flow.screen();
        assert_eq!(confirm.state, "confirm", "{has_places}: {:?}", flow.step());
        let words = confirm.footer.join("\n");
        assert!(
            words.contains("Your answers are not written into the images"),
            "{words}"
        );
        assert!(!words.contains("puts them into the place"), "{words}");
        drive(&mut flow, &bundle, &phone, "cheetah", "installed");
        assert_eq!(flow.screen().action, "link", "{has_places}");
        // userdata is the release's image, byte for byte: no answers in it
        let raw = support::bundle::root_image();
        assert_eq!(
            &phone.partition("userdata")[..raw.len()],
            &raw[..],
            "{has_places}"
        );
        assert!(flow
            .link(&answers(), &Extras::default(), SALT, "0.0.1", false)
            .is_ok());
    }
}

#[test]
fn a_release_without_the_link_ends_at_installed() {
    let bundle = TestBundle::new();
    let phone = FakePhone::scenario("ready").unwrap();
    let options = FlowOptions {
        write: true,
        ..Default::default()
    };
    let mut flow = Flow::begin(&bundle.files(), bundle.index(), &bundle.trusted(), options);
    drive(&mut flow, &bundle, &phone, "cheetah", "installed");
    assert_eq!(flow.screen().action, "none");
    assert!(!flow.has_install_link());
    assert!(flow
        .link(&answers(), &Extras::default(), SALT, "0.0.1", false)
        .is_err());
}

#[test]
fn a_link_this_installer_cannot_speak_is_refused_before_anything_is_done() {
    let bundle = TestBundle::build(
        |m| m["install_link"] = serde_json::json!({"protocol": 2}),
        |c| c,
    );
    let flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions::default(),
    );
    assert_eq!(flow.screen().stop, Some("release-not-verified"));
    let extra = TestBundle::build(
        |m| m["install_link"] = serde_json::json!({"protocol": 1, "more": true}),
        |c| c,
    );
    let flow = Flow::begin(
        &extra.files(),
        extra.index(),
        &extra.trusted(),
        FlowOptions::default(),
    );
    assert_eq!(flow.screen().stop, Some("release-not-verified"));
}

#[test]
fn the_link_is_handed_only_the_answers_this_install_was_given() {
    let (mut flow, _, _) = written();
    let mut other = answers();
    other.username = "sam".into();
    assert!(flow
        .link(&other, &Extras::default(), SALT, "0.0.1", false)
        .is_err());
}

#[test]
fn from_the_write_to_a_phone_that_is_ready() {
    let mut r = rig();
    assert_eq!(r.go(), "ready");
    assert_eq!(
        r.shell.states(),
        [
            "first-start",
            "pair",
            "setup",
            "radio-backup",
            "factory-send",
            "setup",
            "provision",
            "restart",
            "check"
        ],
        "the screens while the phone worked"
    );
    // The answers reached first boot, the password as its hash only
    let conf = r.phone.sim.started_with.clone().expect("first boot ran");
    assert!(conf.contains("USERNAME=alex\n"), "{conf}");
    assert!(conf.contains("CREDENTIAL=pin\n"), "{conf}");
    assert!(
        conf.contains("PASSWORD_HASH=$6$saltsaltsaltsalt$"),
        "{conf}"
    );
    assert!(!conf.contains("4812"));
    // The backup is the phone's partitions, byte for byte, and its files
    // are those tools/nv-backup writes
    for (found, bytes) in parts() {
        assert_eq!(r.shell.backup[&found.name], bytes, "{}", found.name);
    }
    let files = r.shell.finished.clone().expect("the backup's files");
    for (found, bytes) in parts() {
        let line = format!("{}  {}.img\n", to_hex(&sha256(&bytes)), found.name);
        assert!(files.sha256sums.contains(&line), "{}", found.name);
        assert!(files
            .partitions_txt
            .contains(&format!("{} - ro=1 mounted=0 {}\n", found.name, found.size)));
    }
    assert!(files.nv_backup.contains("device: cheetah\n"));
    assert!(files
        .nv_backup
        .contains("checked: efs efs_backup modem_userdata persist modem_a\n"));
    assert!(files.nv_backup.contains("copied, may change: devinfo\n"));
    // The phone kept NV_PARTS' checksums only after the computer had all
    // three files: the commit came after the finish
    assert!(r.phone.sim.sums.is_some());
    // The image was provisioned from and is gone; the phone restarted once
    assert_eq!(r.phone.sim.provision_starts, 1);
    assert_eq!(r.phone.sim.restarts, 1);
    assert!(r.phone.sim.factory_file.is_empty());
    assert_eq!(r.phone.sim.done.as_deref(), Some("finished"));
    assert_eq!(r.phone.sim.phone.wrong_tries(), 0);
    // Read a piece at a time, never whole
    assert!(r.shell.largest_read <= 4096, "{}", r.shell.largest_read);
    let record = r.shell.record.clone().unwrap();
    assert!(record.finished && record.token.is_none());
    let screen = r.link.screen();
    assert_eq!(screen.title, "Your Pixel 7 Pro is ready");
    assert!(!screen.body.iter().any(|l| l.contains("Cameras")));
    // Progress was told for each long step
    for what in ["link-wait", "backup", "factory-send", "provision"] {
        assert!(
            r.shell.progress.iter().any(|p| p.0 == what),
            "no progress of {what}"
        );
    }
}

#[test]
fn nothing_secret_or_identifying_is_in_the_record_but_the_serial_and_token() {
    let mut r = rig();
    r.go();
    let record = r.shell.record.clone().unwrap();
    let text = String::from_utf8(record.to_json()).unwrap();
    assert!(!text.contains("4812") && !text.contains("alex"), "{text}");
    assert!(!format!("{record:?}").contains(SERIAL));
    assert!(!format!("{:?}", r.shell.finished).contains(&to_hex(&sha256(&parts()[0].1))));
}

/// How many frames a straight run sends to the phone.
fn frames_of_a_run() -> u64 {
    let mut r = rig();
    assert_eq!(r.go(), "ready");
    r.phone.frames_in
}

#[test]
fn a_cable_pulled_before_any_frame_is_taken_up_again_with_no_new_code() {
    let total = frames_of_a_run();
    assert!(total > 40, "{total}");
    for n in 0..total {
        let mut r = rig();
        r.phone.unplug_after = Some(n);
        assert_eq!(
            r.go(),
            "ready",
            "pulled after {n} frames: {:?}",
            r.link.screen()
        );
        assert_eq!(r.phone.pulls, 1, "after {n}");
        assert_eq!(r.phone.sim.phone.wrong_tries(), 0, "after {n}");
        assert_eq!(r.phone.sim.firstboot_starts, 1, "after {n}: answers once");
        assert_eq!(r.phone.sim.restarts, 1, "after {n}: one restart");
        for (found, bytes) in parts() {
            assert_eq!(
                r.shell.backup[&found.name], bytes,
                "after {n}: {}",
                found.name
            );
        }
        assert_eq!(r.phone.sim.done.as_deref(), Some("finished"), "after {n}");
    }
}

#[test]
fn an_installer_closed_at_any_screen_goes_on_from_its_record() {
    let states = [
        "first-start",
        "pair",
        "setup",
        "radio-backup",
        "factory-send",
        "provision",
        "restart",
        "check",
    ];
    for at in states {
        let mut r = rig();
        r.shell.leave_at = Some(at);
        assert_eq!(r.go(), "left", "{at}");
        // A new run of the installer: a new flow from the record, no
        // answers, the same phone and cable
        let record = r.shell.record.clone().expect("a record was kept");
        let record = LinkRecord::parse(&record.to_json()).expect("the record reads back");
        let start = record.start("0.0.1", true).unwrap();
        r.link = LinkFlow::new(start);
        r.shell.left = false;
        r.shell.leave_at = None;
        assert_eq!(r.go(), "ready", "left at {at}: {:?}", r.link.screen());
        assert_eq!(r.phone.sim.phone.wrong_tries(), 0, "{at}");
        assert_eq!(r.phone.sim.firstboot_starts, 1, "{at}");
        assert_eq!(r.phone.sim.restarts, 1, "{at}");
        assert_eq!(r.phone.sim.done.as_deref(), Some("finished"), "{at}");
    }
}

#[test]
fn a_run_that_lost_the_answers_before_the_phone_had_them_asks_again() {
    let mut r = rig();
    // Closed once paired, before the answers went
    r.shell.leave_at = Some("pair");
    r.go();
    let start = r
        .shell
        .record
        .clone()
        .unwrap()
        .start("0.0.1", true)
        .unwrap();
    r.link = LinkFlow::new(start);
    r.shell.left = false;
    r.shell.leave_at = None;
    // The run asks for them on its own screen, once paired again by code
    let screen = loop {
        block_on(r.link.run(&mut r.phone, &mut r.shell)).unwrap();
        let screen = r.link.screen();
        if screen.action != "pair" {
            break screen;
        }
        r.link.pair(&r.phone.shows().unwrap()).unwrap();
    };
    if r.phone.sim.phone.stage() == Stage::Waiting {
        assert_eq!(
            (screen.state, screen.action),
            ("setup", "answers"),
            "{screen:?}"
        );
    }
    assert_eq!(r.go(), "ready");
}

#[test]
fn a_lost_token_pairs_again_through_the_phone_at_every_stage() {
    for at in ["radio-backup", "factory-send", "provision", "check"] {
        let mut r = rig();
        r.shell.leave_at = Some(at);
        r.go();
        let mut record = r.shell.record.clone().unwrap();
        record.token = None;
        r.link = LinkFlow::new(record.start("0.0.1", true).unwrap());
        r.shell.left = false;
        r.shell.leave_at = None;
        assert_eq!(r.go(), "ready", "token lost at {at}: {:?}", r.link.screen());
    }
}

#[test]
fn a_wrong_code_says_how_many_tries_are_left_and_a_right_one_pairs() {
    let mut r = rig();
    block_on(r.link.run(&mut r.phone, &mut r.shell)).unwrap();
    assert_eq!(r.link.screen().action, "pair");
    let right = r.phone.shows().unwrap();
    let wrong = if right == "000000" {
        "111111"
    } else {
        "000000"
    };
    r.link.pair(wrong).unwrap();
    block_on(r.link.run(&mut r.phone, &mut r.shell)).unwrap();
    let screen = r.link.screen();
    assert_eq!(screen.action, "pair");
    assert!(
        screen.problem.as_deref().unwrap().contains("4 more tries"),
        "{screen:?}"
    );
    r.link.pair("12 34").unwrap();
    r.link.pair("not a code").ok();
    let screen = r.link.screen();
    assert!(screen.problem.is_some());
    r.link.pair(&right).unwrap();
    assert_eq!(r.go(), "ready");
}

#[test]
fn another_phone_on_the_cable_is_told_nothing_and_the_install_stops() {
    let (_, link, bundle) = written();
    let clock = Clock::default();
    let mut r = Rig {
        link,
        phone: PretendPhone::first_start("ANOTHERSERIAL", "cheetah", clock.clone(), 1),
        shell: MemoryShell::new(clock, bundle.factory_zip.clone()),
    };
    assert_eq!(r.go(), "stopped");
    let screen = r.link.screen();
    assert_eq!(screen.stop, Some("link-wrong-phone"));
    assert_eq!(
        screen.title,
        "This is not the phone that was just installed"
    );
    assert_eq!(r.phone.sim.firstboot_starts, 0, "no answers reached it");
    assert!(r.shell.backup.is_empty());
}

#[test]
fn a_damaged_transfer_is_sent_again_once_and_a_second_one_stops() {
    let mut r = rig();
    r.phone.sim.damage_factory = Some(100);
    assert_eq!(r.go(), "ready", "one damaged transfer, then the right one");

    // The file on this computer is not the listed one: twice
    let mut r = rig();
    r.shell.factory[7] ^= 1;
    assert_eq!(r.go(), "stopped");
    assert_eq!(r.link.screen().stop, Some("factory-mismatch"));
    assert_eq!(r.phone.sim.provision_starts, 0);
}

#[test]
fn a_camera_only_failure_is_said_on_the_last_screen_even_by_a_run_that_missed_it() {
    let mut r = rig();
    r.phone.camera_fails = true;
    assert_eq!(r.go(), "ready");
    let screen = r.link.screen();
    assert!(
        screen
            .body
            .iter()
            .any(|l| l.starts_with("Cameras: not set up")),
        "{screen:?}"
    );

    // Closed while the phone provisioned: the next run learns it from status
    let mut r = rig();
    r.phone.camera_fails = true;
    r.shell.leave_at = Some("provision");
    r.go();
    r.phone.clock().pass(60_000);
    r.link = LinkFlow::new(
        r.shell
            .record
            .clone()
            .unwrap()
            .start("0.0.1", true)
            .unwrap(),
    );
    r.shell.left = false;
    r.shell.leave_at = None;
    assert_eq!(r.go(), "ready");
    assert!(r
        .link
        .screen()
        .body
        .iter()
        .any(|l| l.starts_with("Cameras: not set up")));
}

#[test]
fn first_boot_that_failed_is_tried_again_on_the_owners_word() {
    let mut r = rig();
    r.phone.firstboot_fails = Some("wifi".into());
    let mut asked = false;
    for _ in 0..100 {
        block_on(r.link.run(&mut r.phone, &mut r.shell)).unwrap();
        let screen = r.link.screen();
        match screen.action {
            "pair" => r.link.pair(&r.phone.shows().unwrap()).unwrap(),
            "backup-dir" => r.link.backup_place_chosen().unwrap(),
            "retry" => {
                assert_eq!(screen.state, "setup");
                assert!(screen.body[0].contains("stopped at: wifi"), "{screen:?}");
                asked = true;
                r.link.retry().unwrap();
            }
            _ => break,
        }
    }
    assert!(asked);
    assert_eq!(r.link.screen().state, "ready");
    assert_eq!(r.phone.sim.firstboot_starts, 2);
}

#[test]
fn the_backup_must_be_whole_on_the_computer_before_the_phone_counts_it() {
    let mut r = rig();
    r.shell.finish_fails = true;
    assert_eq!(r.go(), "stopped");
    let screen = r.link.screen();
    assert_eq!(screen.stop, Some("link-computer"));
    assert!(
        screen.body[0].contains("the backup directory is full"),
        "{screen:?}"
    );
    // The phone never recorded the backup as done
    assert!(r.phone.sim.sums.is_none());
    assert!(!r.phone.sim.state_file.backup_done);
}

#[test]
fn every_screen_of_the_link_is_plain_and_names_its_action() {
    let mut r = rig();
    r.go();
    for s in &r.shell.screens {
        assert_eq!(s.action, "wait", "{s:?}");
        assert!(!s.title.is_empty());
    }
    let s = r.link.screen();
    assert_eq!((s.state, s.action), ("ready", "none"));
}

/// Writes a signed test bundle whose root image offers the install link to
/// fixtures/selftest-link/, for the tool's pretend runs, the desktop app's
/// tests and recorded screens, which can't make keys. Its images, root
/// image and factory zip are the same bytes as fixtures/selftest's, and are
/// links to those files. Run by hand:
/// `cargo test -p omamobile-installer-core --test link_flow -- --ignored`
#[test]
#[ignore = "regenerates fixtures/selftest-link"]
fn write_the_link_fixture() {
    let b = link_bundle();
    b.verify().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let (dir, plain) = (root.join("selftest-link"), root.join("selftest"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("images")).unwrap();
    std::fs::create_dir_all(dir.join("factory")).unwrap();
    let put = |name: &str, data: &[u8]| std::fs::write(dir.join(name), data).unwrap();
    let same = |name: &str, data: &[u8], up: &str| {
        assert_eq!(std::fs::read(plain.join(name)).unwrap(), data, "{name}");
        std::os::unix::fs::symlink(format!("{up}selftest/{name}"), dir.join(name)).unwrap();
    };
    put("SHA256SUMS", &b.sums);
    put("SHA256SUMS.sig", &b.sig);
    put("test-key.gpg", &b.keyring);
    put("test-key.fingerprint", b.fingerprint.as_bytes());
    put("manifest.json", &b.manifest_json);
    put("MANIFEST", &b.manifest_txt);
    put("device.conf", &b.device_conf);
    put("factory-builds.json", &b.index_json);
    same("root.img.zst", &b.root_zst, "../");
    for (name, data) in &b.images {
        same(&format!("images/{name}.img"), data, "../../");
    }
    same(
        &format!("factory/{}", b.factory_zip_name),
        &b.factory_zip,
        "../../",
    );
}
