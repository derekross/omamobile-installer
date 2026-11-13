// SPDX-License-Identifier: MIT
//! The steps after the write in `omamobile-install json --write`
//! (docs/DESIGN.md §13, docs/APP.md §3), driven as the desktop app drives
//! them. Nothing here reaches a phone, a network or this computer's
//! settings:
//!
//! - the phone in Fastboot Mode is the pretend one (`--fake-phone`);
//! - the release is `fixtures/after`, the test bundle with stand-ins for
//!   the tools a release ships (`fixtures/after-stand-ins/tools`: phone-ssh,
//!   nv-backup, phone-provision, phone-smoke), signed with the test key;
//! - `nmcli` and `ssh-keygen` are stand-ins too (`--fake-after`,
//!   `fixtures/after-stand-ins/bin`);
//! - the phone that answers over "USB SSH" is a folder in this test's
//!   scratch directory, which the stand-ins read and write
//!   (`OMAINSTALL_PRETEND_PHONE`), and `HOME` and the XDG folders are
//!   scratch folders.
//!
//!   cargo test -p omamobile-installer-cli --features fake-phone --test after_write
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ZIP: &str = "cheetah-test.260101.001-factory-31c2322a.zip";
const PIN: &str = "481516";
/// The digest of the pretend phone's serial number (`FAKESERIAL0001`, the
/// json mode's first pretend phone): what the phone that was written says.
fn written_phone() -> String {
    let out = Command::new("sh")
        .args(["-c", "printf %s FAKESERIAL0001 | sha256sum | cut -d' ' -f1"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn release() -> PathBuf {
    repo().join("fixtures/after")
}
fn stand_ins() -> PathBuf {
    repo().join("fixtures/after-stand-ins/bin")
}

/// One test's folders: the owner's home, the XDG folders, and the phone
/// that answers over SSH.
struct World {
    root: PathBuf,
}

impl World {
    fn new(name: &str) -> World {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("after-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["home", "state", "data", "cache", "phone", "bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        // The stand-ins, and the waits short enough for a test
        for program in ["nmcli", "ssh-keygen"] {
            std::fs::copy(stand_ins().join(program), root.join("bin").join(program)).unwrap();
        }
        std::fs::write(
            root.join("bin/waits"),
            "first_start=4 restart=4 settle=1 poll_ms=20\n",
        )
        .unwrap();
        let w = World { root };
        // The phone after the write: restarting, not on the link yet, and
        // its first start finishes once it is
        w.put("id", &written_phone());
        w.put("boot_id", "boot-1");
        w.put("up_after", "3");
        w.put("firstboot_done", "");
        w
    }
    fn phone(&self) -> PathBuf {
        self.root.join("phone")
    }
    fn put(&self, name: &str, text: &str) {
        std::fs::write(self.phone().join(name), text).unwrap();
    }
    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.phone().join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    fn ran(&self, start: &str) -> Vec<String> {
        self.log()
            .into_iter()
            .filter(|l| l.starts_with(start))
            .collect()
    }
    fn record(&self) -> Option<Value> {
        let text = std::fs::read(
            self.root
                .join("state/omamobile-installer/cheetah/after.json"),
        )
        .ok()?;
        serde_json::from_slice(&text).ok()
    }
}

struct Reply {
    before: Vec<Value>,
    screen: Value,
}

impl Reply {
    fn state(&self) -> &str {
        self.screen["state"].as_str().unwrap()
    }
    fn action(&self) -> &str {
        self.screen["action"].as_str().unwrap()
    }
    fn answers(&self) -> Vec<&str> {
        self.screen["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["answer"].as_str().unwrap())
            .collect()
    }
    fn text(&self) -> String {
        self.screen.to_string()
    }
    fn error(&self) -> Option<&str> {
        self.before
            .iter()
            .find(|m| m["type"] == "error")
            .and_then(|m| m["message"].as_str())
    }
}

struct Tool {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Tool {
    fn start(world: &World, release: &Path, write: bool) -> Tool {
        let mut args: Vec<String> = vec![
            "json".into(),
            "--release".into(),
            release.display().to_string(),
            "--fake-phone".into(),
            "ready".into(),
            "--fake-after".into(),
            world.root.join("bin").display().to_string(),
        ];
        if write {
            args.push("--write".into());
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
            .args(&args)
            .env("HOME", world.root.join("home"))
            .env("XDG_STATE_HOME", world.root.join("state"))
            .env("XDG_DATA_HOME", world.root.join("data"))
            .env("XDG_CACHE_HOME", world.root.join("cache"))
            .env("OMAINSTALL_PRETEND_PHONE", world.phone())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let (mut from, into) = (child.stderr.take().unwrap(), stderr.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = from.read(&mut buf) {
                if n == 0 {
                    break;
                }
                into.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let stdin = child.stdin.take();
        let mut tool = Tool {
            child,
            stdin,
            lines,
            stderr,
        };
        let hello = tool.hear();
        assert_eq!(
            (hello["type"].as_str(), hello["protocol"].as_u64()),
            (Some("hello"), Some(4))
        );
        assert_eq!(hello["writes"], write);
        tool.say(json!({"type": "hello", "protocol": 4, "app": "the tests", "version": "0"}));
        tool
    }

    fn say(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(format!("{message}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn hear(&mut self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| {
                panic!(
                    "the tool said nothing; stderr: {}",
                    self.stderr.lock().unwrap()
                )
            });
        // Standard output is the protocol and nothing else
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("not a message: {line}"))
    }

    fn act(&mut self, message: Value) -> Reply {
        self.say(message);
        let mut before = Vec::new();
        loop {
            let m = self.hear();
            match m["type"].as_str() {
                Some("screen") => {
                    return Reply {
                        before,
                        screen: m["screen"].clone(),
                    }
                }
                Some(
                    "progress" | "error" | "problems" | "phones" | "sent" | "piece" | "account",
                ) => before.push(m),
                _ => panic!("{m} where a screen was owed"),
            }
        }
    }

    fn after(&mut self, answer: &str) -> Reply {
        self.act(json!({"type": "after", "answer": answer}))
    }

    /// The flow from its first screen through the write: the factory image
    /// handed in, made-up answers, the codename typed.
    fn install(&mut self, release: &Path) -> Reply {
        let first = self.act(json!({"type": "start", "reinstall": false}));
        assert_eq!(first.state(), "warning", "{}", first.text());
        assert_eq!(self.act(json!({"type": "acknowledge"})).state(), "phone");
        assert_eq!(
            self.act(json!({"type": "read-phone"})).state(),
            "google-terms"
        );
        assert_eq!(
            self.act(json!({"type": "decline-terms"})).state(),
            "factory-image"
        );
        let zip = release.join("factory").join(ZIP);
        assert_eq!(
            self.act(json!({"type": "take-file", "path": zip})).state(),
            "answers"
        );
        let answers = json!({"type": "answers", "username": "alex", "password": PIN, "password_again": PIN,
                             "hostname": "", "timezone": "Europe/Paris", "wifi_ssid": "", "wifi_psk": ""});
        assert_eq!(self.act(answers).state(), "dry-run");
        let confirm = self.act(json!({"type": "dry-run"}));
        assert_eq!(confirm.state(), "confirm", "{}", confirm.text());
        assert!(confirm
            .text()
            .contains("vendor_kernel_boot_b is written with this computer's SSH public key"));
        assert!(confirm
            .text()
            .contains("This computer's SSH public key goes with your answers"));
        assert!(confirm
            .text()
            .contains("Wi-Fi not set, and this computer's SSH public key."));
        assert_eq!(
            self.act(json!({"type": "confirm", "typed": "cheetah"}))
                .state(),
            "write"
        );
        self.act(json!({"type": "write"}))
    }

    /// Quit, and the run's standard error.
    fn finish(mut self) -> String {
        self.say(json!({"type": "quit"}));
        assert_eq!(self.hear()["type"], "bye");
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success());
        let said = self.stderr.lock().unwrap().clone();
        said
    }
}

#[test]
fn after_the_write_the_phone_is_set_up_with_the_releases_tools_step_by_step() {
    let world = World::new("whole");
    let mut tool = Tool::start(&world, &release(), true);
    let installed = tool.install(&release());
    // The core's last screen, with the way on
    assert_eq!(
        (installed.state(), installed.action()),
        ("installed", "after")
    );
    assert_eq!(installed.answers(), ["begin"]);
    let text = installed.text();
    assert!(
        text.contains("They come next, with the release's own tools"),
        "{text}"
    );
    assert!(
        text.contains("so the installer made one for itself"),
        "{text}"
    );
    // The key was made for the installer, in its data folder: this home
    // has no ~/.ssh
    assert_eq!(world.ran("ssh-keygen").len(), 1);
    assert!(world
        .root
        .join("data/omamobile-installer/ssh/id_ed25519.pub")
        .is_file());
    let record = world.record().expect("the record is begun at the write");
    assert_eq!(record["phone"], written_phone());
    assert!(record["factory_zip"].as_str().unwrap().ends_with(ZIP));

    // The USB connection: asked, explained, and the command shown
    let network = tool.after("begin");
    assert_eq!(
        (network.state(), network.answers()),
        ("network", vec!["add", "myself"])
    );
    assert!(network.text().contains("nmcli connection add type ethernet con-name omamobile-phone match.driver cdc_ncm ipv4.method manual ipv4.addresses 172.16.42.2/24 ipv4.never-default yes ipv6.method disabled connection.autoconnect-priority -100"));
    assert!(network
        .text()
        .contains("nmcli connection delete omamobile-phone"));
    assert!(
        world.ran("nmcli connection add").is_empty(),
        "nothing is added before the owner says so"
    );

    // Added on the click; then the first start is waited for
    let backup = tool.after("add");
    assert_eq!(world.ran("nmcli connection add").len(), 1);
    assert!(backup.before.iter().any(|m| m["what"] == "first-start"));
    assert_eq!(
        (backup.state(), backup.answers()),
        ("backup", vec!["backup"])
    );
    assert!(backup.screen["notice"].as_str().unwrap().contains("IMEI"));
    let folder = backup.screen["body"][1].as_str().unwrap().to_string();
    assert!(
        folder.starts_with(
            &world
                .root
                .join("home/omamobile-backup/cheetah-radio-backup-")
                .display()
                .to_string()
        ),
        "{folder}"
    );
    // Nothing of the release's tools has run yet but the phone's questions
    assert!(world.ran("nv-backup").is_empty());

    let firmware = tool.after("backup");
    assert_eq!(firmware.state(), "firmware", "{}", firmware.text());
    assert_eq!(world.ran("nv-backup"), [format!("nv-backup {folder}")]);
    assert!(Path::new(&folder).join("SHA256SUMS").is_file());

    let restart = tool.after("firmware");
    assert_eq!(restart.state(), "restart", "{}", restart.text());
    let zip = release().join("factory").join(ZIP);
    assert_eq!(
        world.ran("phone-provision"),
        [format!(
            "phone-provision --first-install --nv-backup {folder} {}",
            zip.display()
        )]
    );
    assert!(
        world.ran("ssh systemctl stop omamobile-modem").is_empty(),
        "no restart before the click"
    );

    let ready = tool.after("restart");
    assert_eq!(
        (ready.state(), ready.action()),
        ("ready", "none"),
        "{}",
        ready.text()
    );
    let text = ready.text();
    assert!(
        text.contains("Works: Starts its own system (slot B), Display."),
        "{text}"
    );
    assert!(text.contains("Not working: Sound (no card)."), "{text}");
    assert!(
        text.contains("Not checked: Cameras (not provisioned)."),
        "{text}"
    );
    assert!(text.contains(&folder));
    assert_eq!(
        world.ran("ssh systemctl stop omamobile-modem").len(),
        1,
        "one restart"
    );
    assert_eq!(world.ran("phone-smoke"), ["phone-smoke --json"]);
    let record = world.record().unwrap();
    assert_eq!(record["check"], "passed");
    assert!(record["finished"].is_string());

    // Every step that talked to the phone asked it first which phone it is
    let log = world.log();
    let mut since = 0;
    for step in [
        "nv-backup",
        "phone-provision",
        "ssh systemctl stop omamobile-modem",
        "phone-smoke",
    ] {
        let at = log.iter().position(|l| l.starts_with(step)).unwrap();
        assert!(
            log[since..at]
                .iter()
                .any(|l| l.starts_with("ssh ") && l.contains("sha256sum")),
            "the phone was asked which it is before {step}: {:?}",
            &log[since..at]
        );
        since = at + 1;
    }
    let stderr = tool.finish();
    // The pretend phone in Fastboot Mode received the write and nothing after
    assert!(stderr.contains("partitions written: 6"), "{stderr}");
}

#[test]
fn a_setup_that_was_cut_off_goes_on_from_the_step_that_did_not_finish() {
    let world = World::new("resume");
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.after("begin");
    let firmware = {
        let backup = tool.after("add");
        assert_eq!(backup.state(), "backup");
        tool.after("backup")
    };
    assert_eq!(firmware.state(), "firmware");
    // The window is closed here
    tool.finish();

    let mut tool = Tool::start(&world, &release(), true);
    let resume = tool.act(json!({"type": "start", "reinstall": false}));
    assert_eq!(
        (resume.state(), resume.answers()),
        ("after-resume", vec!["go-on", "forget"])
    );
    assert!(
        resume
            .text()
            .contains("the firmware, the restart and the check"),
        "{}",
        resume.text()
    );
    // Nothing that is not offered is taken
    let refused = tool.act(json!({"type": "acknowledge"}));
    assert!(refused.error().is_some());
    assert_eq!(refused.state(), "after-resume");
    let firmware = tool.after("go-on");
    assert_eq!(firmware.state(), "firmware");
    assert_eq!(
        world.ran("nv-backup").len(),
        1,
        "the backup is not taken again"
    );
    assert_eq!(tool.after("firmware").state(), "restart");
    assert_eq!(tool.after("restart").state(), "ready");
    tool.finish();

    // Finished: a new start is a new install
    let mut tool = Tool::start(&world, &release(), true);
    assert_eq!(
        tool.act(json!({"type": "start", "reinstall": false}))
            .state(),
        "warning"
    );
    tool.finish();
}

#[test]
fn forgetting_an_unfinished_setup_starts_a_new_install() {
    let world = World::new("forget");
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.finish();
    let mut tool = Tool::start(&world, &release(), true);
    assert_eq!(
        tool.act(json!({"type": "start", "reinstall": false}))
            .state(),
        "after-resume"
    );
    let warning = tool.after("forget");
    assert_eq!(warning.state(), "warning");
    assert!(world.record().is_none());
    tool.finish();
    // A run that can't write never offers it, and makes no key
    let world = World::new("forget-dry");
    let mut tool = Tool::start(&world, &release(), false);
    assert_eq!(
        tool.act(json!({"type": "start", "reinstall": false}))
            .state(),
        "warning"
    );
    tool.finish();
    assert!(world.ran("ssh-keygen").is_empty());
}

#[test]
fn another_phone_that_answers_is_asked_which_it_is_and_nothing_else() {
    let world = World::new("other-phone");
    world.put(
        "id",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.after("begin");
    let stopped = tool.after("add");
    assert_eq!(
        (stopped.state(), stopped.screen["stop"].as_str()),
        ("stopped", Some("after-other-phone"))
    );
    assert_eq!(stopped.screen["retry"], "after-fix");
    assert!(stopped
        .text()
        .contains("nothing was copied from it, put on it or restarted"));
    assert!(world.ran("nv-backup").is_empty() && world.ran("phone-provision").is_empty());
    assert!(world.ran("ssh systemctl stop").is_empty());
    tool.finish();
}

#[test]
fn a_connection_networkmanager_refuses_is_shown_for_the_owner_to_add() {
    let world = World::new("nm-refused");
    world.put("nm_refuse", "");
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.after("begin");
    let refused = tool.after("add");
    assert_eq!(refused.state(), "network");
    let problem = refused.screen["problem"].as_str().unwrap();
    assert!(problem.contains("Insufficient privileges"), "{problem}");
    assert!(refused.text().contains("Run this in a terminal"));
    assert_eq!(refused.answers(), ["add", "myself"]);
    // The owner added it: the first start is waited for
    let backup = tool.after("myself");
    assert_eq!(backup.state(), "backup");
    assert_eq!(world.record().unwrap()["network"], "set up by the owner");
    tool.finish();
}

#[test]
fn a_phone_that_refuses_this_computers_key_is_said_plainly() {
    let world = World::new("key-refused");
    world.put("refuse_key", "");
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.after("begin");
    let waiting = tool.after("add");
    assert_eq!(
        (waiting.state(), waiting.answers()),
        ("first-start", vec!["wait"])
    );
    let problem = waiting.screen["problem"].as_str().unwrap();
    assert!(
        problem.contains("does not take this computer's SSH key"),
        "{problem}"
    );
    tool.finish();
}

#[test]
fn a_first_start_that_does_not_finish_is_waited_for_within_its_bound() {
    let world = World::new("no-first-start");
    world.put("firstboot_hangs", "");
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.after("begin");
    let waiting = tool.after("add");
    assert_eq!(
        (waiting.state(), waiting.answers()),
        ("first-start", vec!["wait"])
    );
    assert!(waiting.screen["problem"]
        .as_str()
        .unwrap()
        .contains("hasn't finished its first start after 1 minute."));
    world.put("firstboot_done", "");
    assert_eq!(tool.after("wait").state(), "backup");
    tool.finish();
}

/// A made-up SSH key pair for the owner, new for each test run: random
/// characters in an Ed25519 public key's form, and a private file that is
/// no key. Never a real key.
fn made_up_key(home: &Path, comment: &str) -> String {
    let mut random = [0u8; 33];
    std::fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut random)
        .unwrap();
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let body: String = random
        .chunks(3)
        .flat_map(|c| {
            let w = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
            (0..4)
                .rev()
                .map(move |n| B64[((w >> (6 * n)) & 63) as usize] as char)
        })
        .collect();
    let line = format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI{body} {comment}");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_ed25519"), "a stand-in, not a key\n").unwrap();
    std::fs::write(home.join(".ssh/id_ed25519.pub"), format!("{line}\n")).unwrap();
    line
}

/// What the pretend run wrote to the phone's firstboot.conf: the block the
/// signed manifest names, in the userdata the pretend phone received.
fn firstboot_conf(world: &World) -> String {
    let fs = std::fs::read_to_string(world.phone().join("fs")).unwrap();
    let line = fs
        .lines()
        .find(|l| l.starts_with("/etc/omamobile/firstboot.conf "))
        .unwrap();
    let mut words = line.split(' ').skip(1).map(|w| w.parse::<usize>().unwrap());
    let (offset, length) = (words.next().unwrap(), words.next().unwrap());
    let userdata = std::fs::read(world.phone().join("userdata.img")).unwrap();
    String::from_utf8(userdata[offset..offset + length].to_vec()).unwrap()
}

/// The whole run, as the desktop app runs it: the write with the answers
/// and this computer's key in the release image's placeholder, the phone's
/// first start reading them (the stand-in for omamobile-firstboot, which
/// gives root the answers' SSH_KEY and only then takes the key), then the
/// radio backup, the firmware, the restart and the check.
#[test]
fn one_run_from_the_write_to_a_working_phone_with_the_key_from_the_answers() {
    let world = World::new("end-to-end");
    let key = made_up_key(&world.root.join("home"), "owner@laptop");
    let mut tool = Tool::start(&world, &release(), true);
    let installed = tool.install(&release());
    assert_eq!(installed.state(), "installed", "{}", installed.text());
    // The owner's own key, not one made for the installer
    assert!(world.ran("ssh-keygen").is_empty());
    assert!(
        installed
            .text()
            .contains("went to the phone with your answers"),
        "{}",
        installed.text()
    );
    // The answers' block holds the key, once, as firstboot reads it
    let conf = firstboot_conf(&world);
    assert_eq!(conf.len(), 4096);
    let lines: Vec<&str> = conf.lines().filter(|l| !l.starts_with('#')).collect();
    assert!(lines.contains(&"USERNAME=alex"), "{lines:?}");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("SSH_KEY="))
            .collect::<Vec<_>>(),
        [&format!("SSH_KEY={key}").as_str()]
    );
    assert!(!conf.contains(PIN), "no PIN in the image");

    tool.after("begin");
    let backup = tool.after("add");
    assert_eq!(backup.state(), "backup", "{}", backup.text());
    // The phone refused the key while its first start ran, then took it,
    // because the first start gave root the key from the answers
    let log = world.log();
    let started = log
        .iter()
        .position(|l| l == "first start: account alex, 1 SSH key(s) for root")
        .expect("the first start ran");
    assert!(log[..started].iter().any(|l| l == "ssh refused the key"));
    assert!(!log[started..].iter().any(|l| l == "ssh refused the key"));
    assert_eq!(
        std::fs::read_to_string(world.phone().join("authorized_keys")).unwrap(),
        format!("{key}\n")
    );
    assert!(backup.screen["problem"].is_null(), "{}", backup.text());

    assert_eq!(tool.after("backup").state(), "firmware");
    assert_eq!(tool.after("firmware").state(), "restart");
    let ready = tool.after("restart");
    assert_eq!(ready.state(), "ready", "{}", ready.text());
    assert_eq!(world.record().unwrap()["check"], "passed");
    assert_eq!(world.ran("nv-backup").len(), 1);
    assert_eq!(world.ran("phone-provision").len(), 1);
    assert_eq!(world.ran("phone-smoke"), ["phone-smoke --json"]);
    // The key is in no message the tool sent, and nowhere on its standard
    // error
    let stderr = tool.finish();
    let body = key.split(' ').nth(1).unwrap();
    assert!(!stderr.contains(body), "{stderr}");
    assert!(!ready.text().contains(body));
}

/// The phone takes only the key that went with the answers: with another
/// in this computer's key file, the first start finishes and the key is
/// refused to the end of the wait, said plainly; with the key file back,
/// waiting again goes on.
#[test]
fn a_key_that_is_not_the_one_in_the_answers_is_refused() {
    let world = World::new("other-key");
    let home = world.root.join("home");
    let key = made_up_key(&home, "owner@laptop");
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    // After the write, this computer's key file changes
    let other = made_up_key(&home, "owner@laptop");
    assert_ne!(key, other);
    tool.after("begin");
    let waiting = tool.after("add");
    assert_eq!(
        (waiting.state(), waiting.answers()),
        ("first-start", vec!["wait"])
    );
    let problem = waiting.screen["problem"].as_str().unwrap();
    assert!(
        problem.contains("still does not take this computer's SSH key"),
        "{problem}"
    );
    assert_eq!(
        std::fs::read_to_string(world.phone().join("authorized_keys")).unwrap(),
        format!("{key}\n")
    );
    assert!(world.ran("nv-backup").is_empty());
    // The key that went with the answers, back in its file
    std::fs::write(home.join(".ssh/id_ed25519.pub"), format!("{key}\n")).unwrap();
    assert_eq!(tool.after("wait").state(), "backup");
    tool.finish();
}

#[test]
fn a_tool_that_is_not_the_signed_one_never_runs() {
    let world = World::new("changed-tool");
    // A copy of the release with one byte of a tool changed after signing
    let copy = world.root.join("release");
    let status = Command::new("cp")
        .arg("-a")
        .arg(release())
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    let tool_file = copy.join("tools/nv-backup");
    let mut bytes = std::fs::read(&tool_file).unwrap();
    bytes.extend_from_slice(b"\ntouch \"$OMAINSTALL_PRETEND_PHONE/changed-tool-ran\"\n");
    std::fs::write(&tool_file, bytes).unwrap();
    let mut tool = Tool::start(&world, &copy, true);
    tool.install(&copy);
    let refused = tool.after("begin");
    let error = refused.error().expect("an error");
    assert!(
        error.contains("tools/nv-backup is not the file the release signed"),
        "{error}"
    );
    assert_eq!(refused.state(), "network");
    // Not even phone-ssh ran: the tools are checked before any of them
    let ran: Vec<String> = world
        .log()
        .into_iter()
        .filter(|l| !l.starts_with("ssh-keygen"))
        .collect();
    assert!(ran.is_empty(), "{ran:?}");
    assert!(!world.phone().join("changed-tool-ran").exists());
    tool.finish();
}

#[test]
fn a_failed_backup_stays_open_with_the_tools_last_words() {
    let world = World::new("backup-fails");
    world.put(
        "fail_backup",
        "error: efs reads ro=0: the storage guard does not hold\n",
    );
    let mut tool = Tool::start(&world, &release(), true);
    tool.install(&release());
    tool.after("begin");
    tool.after("add");
    let failed = tool.after("backup");
    assert_eq!(
        (failed.state(), failed.answers()),
        ("backup", vec!["backup"])
    );
    assert!(failed.screen["problem"]
        .as_str()
        .unwrap()
        .contains("don't work around it"));
    assert!(failed.text().contains("the storage guard does not hold"));
    assert!(world.record().unwrap().get("backup").is_none());
    tool.finish();
}
