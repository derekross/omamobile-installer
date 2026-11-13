// SPDX-License-Identifier: MIT
//! `omamobile-install json --write` through the install link (docs/APP.md
//! §3, protocol 5), against the pretend phones: the fastboot one for the
//! write, then the one that speaks the link. A test release whose root
//! image offers the link (fixtures/selftest-link). No USB.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const PIN: &str = "481516";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest-link")
}
fn zip_file() -> PathBuf {
    std::fs::read_dir(fixture().join("factory"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("json-link-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Tool {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
    said: Vec<Value>,
}

impl Tool {
    fn start(work: &Path, env: &[(&str, &str)]) -> Tool {
        Tool::start_release(&fixture(), work, env)
    }
    fn start_release(fixture: &Path, work: &Path, env: &[(&str, &str)]) -> Tool {
        let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
            .args([
                "json",
                "--release",
                fixture.to_str().unwrap(),
                "--work-dir",
                work.to_str().unwrap(),
                "--fake-phone",
                "ready",
                "--write",
            ])
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line).expect("a JSON line");
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        let mut tool = Tool {
            child,
            stdin,
            lines,
            said: Vec::new(),
        };
        assert_eq!(tool.hear()["type"], "hello");
        tool.say(json!({"type": "hello", "protocol": 5}));
        tool
    }
    fn say(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
    }
    fn hear(&mut self) -> Value {
        let v = self
            .lines
            .recv_timeout(Duration::from_secs(60))
            .expect("the tool said something");
        self.said.push(v.clone());
        v
    }
    /// Send an action and read until a screen that asks for something.
    fn act(&mut self, v: Value) -> Value {
        self.say(v);
        loop {
            let m = self.hear();
            if m["type"] == "screen" && m["screen"]["action"] != "wait" {
                return m["screen"].clone();
            }
            assert_ne!(m["type"], "fatal", "{m}");
        }
    }
    fn shows(&self) -> String {
        self.said
            .iter()
            .rev()
            .find(|m| m["type"] == "pretend")
            .and_then(|m| m["shows"].as_str())
            .expect("the pretend phone's code")
            .to_string()
    }
    fn states(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in &self.said {
            if m["type"] == "screen" {
                let s = m["screen"]["state"].as_str().unwrap().to_string();
                if out.last() != Some(&s) {
                    out.push(s);
                }
            }
        }
        out
    }
    /// Do what each screen asks until `until` or the end.
    fn drive(&mut self, mut screen: Value, backup: &Path, until: &str) -> Value {
        for _ in 0..60 {
            let state = screen["state"].as_str().unwrap().to_string();
            if state == until {
                return screen;
            }
            let action = screen["action"].as_str().unwrap().to_string();
            let answers = json!({"type": "answers", "username": "alex", "password": PIN, "password_again": PIN,
                "hostname": "", "timezone": "Europe/Paris", "wifi_ssid": "", "wifi_psk": "",
                "full_name": "Alex Example", "email": "alex@example.org"});
            screen = match action.as_str() {
                "acknowledge" => self.act(json!({"type": "acknowledge"})),
                "read-phone" => self.act(json!({"type": "read-phone"})),
                "accept-terms" => self.act(json!({"type": "decline-terms"})),
                "download" | "take-file" => {
                    self.act(json!({"type": "take-file", "path": zip_file()}))
                }
                "answers" => self.act(answers),
                "dry-run" => self.act(json!({"type": "dry-run"})),
                "confirm" => self.act(json!({"type": "confirm", "typed": "cheetah"})),
                "write" => self.act(json!({"type": "write"})),
                "link" => self.act(json!({"type": "link"})),
                "pair" => {
                    let code = self.shows();
                    self.act(json!({"type": "pair", "code": code}))
                }
                "backup-dir" => self.act(json!({"type": "backup-dir", "path": backup})),
                "retry" => self.act(json!({"type": "retry"})),
                _ => return screen,
            };
        }
        panic!("never reached {until}: {screen}");
    }
    fn quit(mut self) -> Vec<Value> {
        self.say(json!({"type": "quit"}));
        while let Ok(v) = self.lines.recv_timeout(Duration::from_secs(30)) {
            self.said.push(v);
        }
        let _ = self.child.wait();
        self.said
    }
}

fn begin(tool: &mut Tool) -> Value {
    tool.say(json!({"type": "start", "reinstall": false}));
    loop {
        let m = tool.hear();
        if m["type"] == "screen" {
            return m["screen"].clone();
        }
    }
}

#[test]
fn the_app_goes_from_the_write_through_the_link_to_ready() {
    let dir = scratch("ready");
    let (work, backup) = (dir.join("work"), dir.join("backup"));
    let mut tool = Tool::start(&work, &[]);
    let first = begin(&mut tool);
    let end = tool.drive(first, &backup, "ready");
    assert_eq!(end["action"], "none");
    assert_eq!(end["title"], "Your Pixel 7 Pro is ready");
    let states = tool.states();
    let from = states.iter().position(|s| s == "installed").unwrap();
    assert_eq!(
        &states[from..],
        [
            "installed",
            "first-start",
            "pair",
            "setup",
            "radio-backup",
            "factory-send",
            "setup",
            "provision",
            "restart",
            "check",
            "ready"
        ]
    );
    let said = tool.quit();
    // Progress of the link's steps is said with its own words
    for what in ["link-wait", "backup", "factory-send", "provision"] {
        assert!(
            said.iter()
                .any(|m| m["type"] == "progress" && m["what"] == what),
            "{what}"
        );
    }
    // Neither the PIN nor its hash crosses to the app
    let text: String = said.iter().map(|m| m.to_string()).collect();
    assert!(!text.contains(PIN) && !text.contains("$6$"));
    assert!(backup.join("SHA256SUMS").is_file());
}

#[test]
fn an_action_the_link_screen_does_not_name_is_refused() {
    let dir = scratch("refused");
    let (work, backup) = (dir.join("work"), dir.join("backup"));
    let mut tool = Tool::start(&work, &[]);
    let first = begin(&mut tool);
    let screen = tool.drive(first, &backup, "pair");
    assert_eq!(screen["action"], "pair");
    tool.say(json!({"type": "backup-dir", "path": backup}));
    let m = tool.hear();
    assert_eq!(m["type"], "error");
    assert!(
        m["message"].as_str().unwrap().contains("asks for \"pair\""),
        "{m}"
    );
    tool.quit();
}

#[test]
fn a_closed_app_goes_on_from_the_record_in_a_new_run() {
    let dir = scratch("again");
    let (work, backup) = (dir.join("work"), dir.join("backup"));
    let mut tool = Tool::start(&work, &[]);
    let first = begin(&mut tool);
    tool.drive(first, &backup, "pair");
    let code = tool.shows();
    tool.say(json!({"type": "pair", "code": code}));
    // The app is closed while the phone works by itself
    loop {
        let m = tool.hear();
        if m["type"] == "screen" && m["screen"]["action"] == "backup-dir" {
            tool.say(json!({"type": "backup-dir", "path": backup}));
        }
        if m["type"] == "screen" && m["screen"]["state"] == "factory-send" {
            break;
        }
    }
    tool.quit();
    let record = std::fs::read_to_string(work.join("link/record.json")).unwrap();
    assert!(record.contains("\"token\": \""), "{record}");

    // A new run goes on from the record. (Its pretend phone is a new one,
    // at its first start, which holds no token: the run pairs by code again
    // and gives the answers again, as for a phone whose link started over.)
    let mut tool = Tool::start(&work, &[]);
    tool.say(json!({"type": "start", "link": true}));
    let screen = loop {
        let m = tool.hear();
        if m["type"] == "screen" && m["screen"]["action"] != "wait" {
            break m["screen"].clone();
        }
    };
    let end = tool.drive(screen, &backup, "ready");
    assert_eq!(end["state"], "ready");
    tool.quit();
}

/// The three answers only the install link carries are asked for where the
/// release offers it, and refused where it doesn't: a release that takes
/// its answers in the image's placeholders has no place for them, and
/// taking them there would drop them without a word.
#[test]
fn the_links_extra_answers_are_asked_for_only_where_the_link_carries_them() {
    let dir = scratch("extras");
    let backup = dir.join("backup");
    let mut tool = Tool::start(&dir.join("work-link"), &[]);
    let first = begin(&mut tool);
    let screen = tool.drive(first, &backup, "answers");
    assert_eq!(screen["extras"], true, "{screen}");
    tool.quit();

    let plain = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest");
    let mut tool = Tool::start_release(&plain, &dir.join("work-plain"), &[]);
    let first = begin(&mut tool);
    let screen = tool.drive(first, &backup, "answers");
    assert!(screen.get("extras").is_none(), "{screen}");
    tool.say(json!({"type": "answers", "username": "alex", "password": PIN, "password_again": PIN,
        "hostname": "", "timezone": "Europe/Paris", "wifi_ssid": "", "wifi_psk": "",
        "ssh_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleExample a@b"}));
    let m = tool.hear();
    assert_eq!(m["type"], "error", "{m}");
    assert!(
        m["message"].as_str().unwrap().contains("install link"),
        "{m}"
    );
    let m = tool.hear();
    assert_eq!(m["screen"]["state"], "answers", "{m}");
    tool.quit();
}
