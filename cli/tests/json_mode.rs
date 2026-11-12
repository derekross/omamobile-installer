// SPDX-License-Identifier: MIT
//! `omamobile-install json` (docs/APP.md §3, protocol 1), the built tool
//! driven over its standard input and output as the desktop app, an agent or
//! a script drives it. The phones are the pretend ones (feature
//! `fake-phone`) and Google's server is a socket on 127.0.0.1 in this file:
//! no USB device is opened and nothing reaches Google.
//!
//! Every test ends in [`Tool::finish`], which checks what holds for every
//! run: standard output is JSON lines of the protocol and nothing else, each
//! action was answered by exactly one screen, and the pretend phones
//! received nothing but reads.
//!
//!   cargo test -p omamobile-installer-cli --features fake-phone
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ZIP: &str = "cheetah-test.260101.001-factory-31c2322a.zip";
const TYPES: [&str; 8] = [
    "hello", "screen", "progress", "phones", "problems", "error", "fatal", "bye",
];
const PIN: &str = "481516";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest")
}
fn zip_file() -> PathBuf {
    fixture().join("factory").join(ZIP)
}
fn zip() -> Vec<u8> {
    std::fs::read(zip_file()).unwrap()
}
/// A directory of this test's own, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("json-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn answers(username: &str) -> Value {
    json!({"type": "answers", "username": username, "password": PIN, "password_again": PIN,
           "hostname": "", "timezone": "Europe/Paris", "wifi_ssid": "", "wifi_psk": ""})
}

/// What the tool said to one action: what came before the screen, and the
/// screen that ended it.
struct Reply {
    before: Vec<Value>,
    screen: Value,
}

impl Reply {
    /// The types of what came before the screen, a run of `progress` as one.
    fn kinds(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for m in &self.before {
            let kind = m["type"].as_str().unwrap();
            if out.last() != Some(&kind) || kind != "progress" {
                out.push(kind);
            }
        }
        out
    }
    fn state(&self) -> &str {
        self.screen["state"].as_str().unwrap()
    }
    fn action(&self) -> &str {
        self.screen["action"].as_str().unwrap()
    }
    fn error(&self) -> &str {
        assert_eq!(self.kinds(), ["error"], "{:?}", self.before);
        self.before[0]["message"].as_str().unwrap()
    }
    /// The first message of this type among what came before the screen.
    fn the(&self, kind: &str) -> &Value {
        self.before
            .iter()
            .find(|m| m["type"] == kind)
            .unwrap_or_else(|| panic!("no {kind} in {:?}", self.before))
    }
}

/// How a run ended.
struct Ended {
    code: i32,
    /// Every message the tool sent, in order.
    said: Vec<Value>,
    stderr: String,
}

impl Ended {
    fn kinds(&self) -> Vec<&str> {
        self.said
            .iter()
            .map(|m| m["type"].as_str().unwrap())
            .collect()
    }
    /// The states of the screens, in order.
    fn states(&self) -> Vec<&str> {
        self.said
            .iter()
            .filter(|m| m["type"] == "screen")
            .map(|m| m["screen"]["state"].as_str().unwrap())
            .collect()
    }
    /// What the pretend phones received (the tool's record on stderr).
    fn commands(&self) -> Vec<&str> {
        self.stderr
            .lines()
            .filter_map(|l| l.split_once(" <- "))
            .filter(|(who, _)| who.starts_with("pretend phone "))
            .map(|(_, command)| command)
            .collect()
    }
}

struct Tool {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    said: Vec<Value>,
    /// Lines of standard output that are not a message of the protocol.
    stray: Vec<String>,
    /// How many screens are owed: one for `start`, one for each action.
    owed: usize,
    stderr: Arc<Mutex<String>>,
}

impl Tool {
    /// `omamobile-install json ARGS`, with nothing sent yet.
    fn with_args(args: &[&str]) -> Tool {
        let mut child = Command::new(env!("CARGO_BIN_EXE_omamobile-install"))
            .arg("json")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A download with no --work-dir must not land in a real cache
            .env(
                "XDG_CACHE_HOME",
                Path::new(env!("CARGO_TARGET_TMPDIR")).join("json-cache"),
            )
            .spawn()
            .unwrap();
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match stdout.read_until(b'\n', &mut line) {
                    Ok(n) if n > 0 => {
                        if tx
                            .send(String::from_utf8_lossy(&line).into_owned())
                            .is_err()
                        {
                            return;
                        }
                    }
                    _ => return,
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
        Tool {
            stdin: child.stdin.take(),
            child,
            lines,
            said: Vec::new(),
            stray: Vec::new(),
            owed: 0,
            stderr,
        }
    }

    /// A run against pretend phones in these states, the test bundle as the
    /// release, and `extra` arguments. Reads the tool's `hello`.
    fn pretend(states: &str, extra: &[&str]) -> Tool {
        let dir = fixture();
        let mut args = vec!["--release", dir.to_str().unwrap(), "--fake-phone", states];
        args.extend_from_slice(extra);
        let mut tool = Tool::with_args(&args);
        let hello = tool.hear();
        assert_eq!(
            hello,
            json!({"type": "hello", "protocol": 1, "tool": "omamobile-install", "version": env!("CARGO_PKG_VERSION")})
        );
        tool
    }

    /// As [`Tool::pretend`], through the handshake to the first screen.
    fn started(states: &str, extra: &[&str]) -> (Tool, Reply) {
        let mut tool = Tool::pretend(states, extra);
        tool.say(json!({"type": "hello", "protocol": 1, "app": "the tests", "version": "0"}));
        let first = tool.act(json!({"type": "start", "reinstall": false}));
        (tool, first)
    }

    fn write(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    /// Send a message that is owed no screen.
    fn say(&mut self, message: Value) {
        self.write(format!("{message}\n").as_bytes());
    }

    /// The next message, or `None` when the tool says nothing for `wait`
    /// or has closed its output.
    fn hear_within(&mut self, wait: Duration) -> Option<Value> {
        let line = self.lines.recv_timeout(wait).ok()?;
        // One JSON object, ended by one line feed, of a type the protocol has
        let parsed = serde_json::from_str::<Value>(&line).ok().filter(|m| {
            line.ends_with('\n')
                && line.matches('\n').count() == 1
                && m.as_object()
                    .and_then(|m| m.get("type"))
                    .and_then(Value::as_str)
                    .is_some_and(|t| TYPES.contains(&t))
        });
        match parsed {
            Some(message) => {
                self.said.push(message.clone());
                Some(message)
            }
            None => {
                self.stray.push(line);
                Some(Value::Null)
            }
        }
    }

    fn hear(&mut self) -> Value {
        self.hear_within(Duration::from_secs(30))
            .unwrap_or_else(|| panic!("the tool said nothing; so far: {:?}", self.said))
    }

    /// Everything up to the next screen. The reply rule: any number of
    /// `progress`, at most one of `phones`, `problems` and `error`, then
    /// the screen.
    fn reply(&mut self) -> Reply {
        let mut before = Vec::new();
        loop {
            let message = self.hear();
            match message["type"].as_str() {
                Some("screen") => {
                    let others = before.iter().filter(|m: &&Value| m["type"] != "progress");
                    assert!(others.count() <= 1, "{before:?}");
                    return Reply {
                        before,
                        screen: message["screen"].clone(),
                    };
                }
                Some("progress" | "phones" | "problems" | "error") => before.push(message),
                _ => panic!("{message} where a screen was owed; before it: {before:?}"),
            }
        }
    }

    /// Send one action (or anything else that is owed a screen) and read
    /// what answers it.
    fn act(&mut self, message: Value) -> Reply {
        self.owed += 1;
        self.say(message);
        self.reply()
    }

    /// A line as it is, owed a screen like an action.
    fn act_raw(&mut self, line: &[u8]) -> Reply {
        self.owed += 1;
        self.write(line);
        self.reply()
    }

    /// The straight path from the first screen to the factory-image step,
    /// with Google's terms declined: the owner hands in the file.
    fn go_to_take_file(&mut self) {
        assert_eq!(self.act(json!({"type": "acknowledge"})).state(), "phone");
        let terms = self.act(json!({"type": "read-phone"}));
        assert_eq!(terms.state(), "google-terms");
        let image = self.act(json!({"type": "decline-terms"}));
        assert_eq!(
            (image.state(), image.action()),
            ("factory-image", "take-file")
        );
    }

    /// `quit`, the `bye` that answers it, and the end of the run.
    fn quit(mut self) -> Ended {
        self.say(json!({"type": "quit"}));
        let ended = self.finish();
        assert_eq!(ended.code, 0, "{}", ended.stderr);
        assert_eq!(ended.said.last().unwrap(), &json!({"type": "bye"}));
        ended
    }

    /// Close the tool's input, wait for it to end, and check what holds for
    /// every run of this mode.
    fn finish(mut self) -> Ended {
        drop(self.stdin.take());
        while self.hear_within(Duration::from_secs(30)).is_some() {}
        let code = self.child.wait().unwrap().code().unwrap();
        // The reader of stderr ends with the process
        std::thread::sleep(Duration::from_millis(50));
        let stderr = self.stderr.lock().unwrap().clone();
        let ended = Ended {
            code,
            said: self.said,
            stderr,
        };
        // Standard output carried the protocol and nothing else
        assert!(self.stray.is_empty(), "not the protocol: {:?}", self.stray);
        // The tool speaks first, and a run that got as far as hello began with it
        if let Some(first) = ended.kinds().first() {
            assert!(["hello", "fatal"].contains(first), "{first}");
        }
        let hellos = ended.kinds().iter().filter(|k| **k == "hello").count();
        assert!(hellos <= 1 && (hellos == 0 || ended.kinds()[0] == "hello"));
        // Every action ended with exactly one screen
        let screens = ended.kinds().iter().filter(|k| **k == "screen").count();
        assert_eq!(screens, self.owed, "{:?}", ended.kinds());
        // Nothing but reads reached a pretend phone: no flash, no erase, no
        // set_active, no reboot, no download, no unlock
        for command in ended.commands() {
            assert!(
                command.starts_with("getvar:") || command == "flashing get_unlock_ability",
                "the pretend phone received {command}"
            );
        }
        for line in ended.stderr.lines().filter(|l| l.contains("received ")) {
            assert!(line.ends_with(" 0 others; partitions written: 0"), "{line}");
        }
        // No serial number and nothing of the owner's secrets, anywhere
        for text in [
            &ended.stderr,
            &ended.said.iter().map(Value::to_string).collect::<String>(),
        ] {
            assert!(
                !text.contains("FAKESERIAL") && !text.contains(PIN),
                "{text}"
            );
        }
        ended
    }
}

// ---- A stand-in for Google's server ---------------------------------------------

/// One request the stand-in got.
#[derive(Clone)]
struct Asked {
    path: String,
    /// The first byte asked for, from a `Range: bytes=N-` header.
    from: Option<usize>,
}

/// A stand-in for Google's server on 127.0.0.1: `answer` writes the reply to
/// each request. The log says what was asked.
fn stand_in(
    answer: impl Fn(&Asked, &mut TcpStream) + Send + 'static,
) -> (SocketAddr, Arc<Mutex<Vec<Asked>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = listener.local_addr().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let head = String::from_utf8_lossy(&head).to_string();
            let mut lines = head.lines();
            let path = lines
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .unwrap_or_default()
                .to_string();
            let from = lines
                .filter_map(|l| l.split_once(": "))
                .find(|(name, _)| name.eq_ignore_ascii_case("range"))
                .and_then(|(_, v)| v.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok());
            let request = Asked { path, from };
            log.lock().unwrap().push(request.clone());
            answer(&request, &mut stream);
        }
    });
    (at, asked)
}

/// As a server that knows `Range` answers: the file, or the rest of it.
fn send_file(file: &[u8], asked: &Asked, stream: &mut TcpStream) {
    let from = asked.from.unwrap_or(0);
    let head = match asked.from {
        Some(from) => format!(
            "HTTP/1.1 206 Stand-in\r\nContent-Range: bytes {from}-{}/{}\r\n",
            file.len() - 1,
            file.len()
        ),
        None => "HTTP/1.1 200 Stand-in\r\n".to_string(),
    };
    let _ = stream.write_all(
        format!(
            "{head}Connection: close\r\nContent-Length: {}\r\n\r\n",
            file.len() - from
        )
        .as_bytes(),
    );
    let _ = stream.write_all(&file[from..]);
}

/// The file's head and its first `some` bytes, and then nothing more until
/// `go` is set: a transfer that stands still.
fn send_some_and_wait(file: &[u8], some: usize, go: &AtomicBool, stream: &mut TcpStream) {
    let head = format!(
        "HTTP/1.1 200 Stand-in\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        file.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&file[..some]);
    let _ = stream.flush();
    while !go.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---- The handshake ---------------------------------------------------------------

#[test]
fn the_tool_says_hello_first_and_asks_nothing_of_a_phone_before_start() {
    let mut tool = Tool::pretend("ready", &[]);
    // Nothing follows the hello by itself: no screen before start
    assert!(tool.hear_within(Duration::from_millis(300)).is_none());
    // The app's hello is not answered
    tool.say(json!({"type": "hello", "protocol": 1, "app": "the tests", "version": "0"}));
    assert!(tool.hear_within(Duration::from_millis(300)).is_none());
    let ended = tool.quit();
    assert_eq!(ended.kinds(), ["hello", "bye"]);
    assert!(
        ended.stderr.contains("received 0 commands"),
        "{}",
        ended.stderr
    );

    // What `omainstall check` does: read the hello, send quit. It needs no
    // release, and none is read
    let mut tool = Tool::with_args(&["--fake-phone", "ready"]);
    assert_eq!(tool.hear()["protocol"], 1);
    assert_eq!(tool.quit().kinds(), ["hello", "bye"]);

    // The first screen comes with start, and is the warning
    let (tool, first) = Tool::started("ready", &[]);
    assert_eq!((first.state(), first.action()), ("warning", "acknowledge"));
    assert!(first.before.is_empty());
    let ended = tool.quit();
    assert_eq!(ended.kinds(), ["hello", "screen", "bye"]);
    // Still nothing was asked of the phone: the warning was not accepted
    assert!(ended.commands().is_empty());

    // The end of the input means quit
    let (tool, _) = Tool::started("ready", &[]);
    let ended = tool.finish();
    assert_eq!(
        (ended.code, ended.kinds()),
        (0, vec!["hello", "screen", "bye"])
    );
}

#[test]
fn another_protocol_number_is_fatal() {
    for hello in [
        json!({"type": "hello", "protocol": 2, "app": "the tests", "version": "0"}),
        json!({"type": "hello", "protocol": 0}),
        json!({"type": "hello", "protocol": "1"}),
        json!({"type": "hello"}),
    ] {
        let mut tool = Tool::pretend("ready", &[]);
        tool.say(hello.clone());
        let ended = tool.finish();
        assert_eq!(ended.code, 1, "{hello}");
        assert_eq!(ended.kinds(), ["hello", "fatal"], "{hello}");
        let said = ended.said[1]["message"].as_str().unwrap();
        assert!(said.contains("this tool speaks protocol 1"), "{said}");
        assert!(ended.stderr.contains("received 0 commands"));
    }
}

#[test]
fn what_the_tool_cannot_start_with_is_fatal_and_no_argument_writes() {
    // An argument the mode does not have: refused before hello, and so is
    // anything that would ask for a write, name a phone or skip a check
    let dir = fixture();
    let dir = dir.to_str().unwrap();
    for refused in [
        "--write",
        "--flash",
        "--yes",
        "--no-dry-run",
        "--dry-run",
        "--reinstall",
        "--maintainer-skip-bootloader-slot-check",
    ] {
        for args in [
            vec!["--release", dir, "--fake-phone", "ready", refused],
            vec![refused, "--release", dir, "--fake-phone", "ready"],
        ] {
            let ended = Tool::with_args(&args).finish();
            assert_eq!(ended.code, 1, "{args:?}");
            assert_eq!(ended.kinds(), ["fatal"], "{args:?}");
            let said = ended.said[0]["message"].as_str().unwrap();
            assert!(
                said.contains("no argument that writes to a phone"),
                "{said}"
            );
            // No pretend phone was even made
            assert!(
                !ended.stderr.contains("pretend phone 1"),
                "{}",
                ended.stderr
            );
        }
    }
    let ended = Tool::with_args(&["--release", dir, "--serial", "<SERIAL>"]).finish();
    assert_eq!((ended.code, ended.kinds()), (1, vec!["fatal"]));
    let ended = Tool::with_args(&["--release", dir, "--fake-phone", "no-such-phone"]).finish();
    assert_eq!((ended.code, ended.kinds()), (1, vec!["fatal"]));
    assert!(ended.said[0]["message"]
        .as_str()
        .unwrap()
        .contains("no pretend phone"));
    let ended = Tool::with_args(&[
        "--release",
        dir,
        "--fake-phone",
        "ready",
        "--fake-google",
        "192.0.2.1:80",
    ])
    .finish();
    assert_eq!((ended.code, ended.kinds()), (1, vec!["fatal"]));
    assert!(ended.said[0]["message"]
        .as_str()
        .unwrap()
        .contains("on this computer only"));

    // No release, or one that can't be read: said at start, as the tool's
    // failure. One that fails its checks is the flow's stop instead (below)
    for args in [
        vec!["--fake-phone", "ready"],
        vec!["--fake-phone", "ready", "--release", "/nowhere/at/all"],
    ] {
        let mut tool = Tool::with_args(&args);
        tool.hear();
        tool.say(json!({"type": "hello", "protocol": 1}));
        tool.say(json!({"type": "start", "reinstall": false}));
        let ended = tool.finish();
        assert_eq!((ended.code, ended.kinds()), (1, vec!["hello", "fatal"]));
    }

    // A panic in the tool is its failure too: said on the wire, status 1
    let (mut tool, _) = Tool::started("ready", &[]);
    tool.say(json!({"type": "pretend-panic"}));
    let ended = tool.finish();
    assert_eq!(
        (ended.code, ended.kinds()),
        (1, vec!["hello", "screen", "fatal"])
    );
    let said = ended.said[2]["message"].as_str().unwrap();
    assert!(said.contains("failed unexpectedly"), "{said}");
    assert!(ended.stderr.contains("the tests asked for a panic"));
}

// ---- The flow ----------------------------------------------------------------------

#[test]
fn the_whole_flow_to_the_dry_run_as_a_transcript() {
    let file = zip();
    let (google, asked) = stand_in(move |asked, stream| send_file(&file, asked, stream));
    let work = scratch("transcript");
    let (mut tool, first) = Tool::started(
        "ready",
        &[
            "--fake-google",
            &google.to_string(),
            "--work-dir",
            work.to_str().unwrap(),
        ],
    );
    assert_eq!(first.screen["title"], "Read this once");
    // The core's Screen, key for key
    let mut keys: Vec<&str> = first
        .screen
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "action", "body", "file", "footer", "link", "notice", "plan", "problem", "quote",
            "retry", "state", "steps", "stop", "title"
        ]
    );

    let phone = tool.act(json!({"type": "acknowledge"}));
    assert_eq!((phone.state(), phone.action()), ("phone", "read-phone"));
    let terms = tool.act(json!({"type": "read-phone"}));
    assert_eq!(
        (terms.state(), terms.action()),
        ("google-terms", "accept-terms")
    );
    assert!(terms.screen["quote"]["lines"][0]
        .as_str()
        .unwrap()
        .starts_with("Downloading of the system image"));
    // Nothing was fetched before the terms were accepted
    assert!(asked.lock().unwrap().is_empty());
    let image = tool.act(json!({"type": "accept-terms"}));
    assert_eq!(
        (image.state(), image.action()),
        ("factory-image", "download")
    );
    assert_eq!(
        image.screen["file"],
        json!({"name": ZIP, "size": 49152, "build": "TEST.260101.001"})
    );
    assert!(asked.lock().unwrap().is_empty());

    let got = tool.act(json!({"type": "download", "do": "start"}));
    assert_eq!(got.kinds(), ["progress"]);
    for p in &got.before {
        assert_eq!(
            (&p["what"], &p["total"]),
            (&json!("download"), &json!(49152))
        );
    }
    assert_eq!(got.before.last().unwrap()["done"], 49152);
    assert_eq!((got.state(), got.action()), ("answers", "answers"));
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert!(asked.lock().unwrap()[0].path.ends_with(ZIP));
    assert_eq!(
        std::fs::read(work.join("factory").join(ZIP)).unwrap(),
        zip()
    );

    // Answers the core objects to: its words, per field, and the step stays
    let mut bad = answers("Root");
    bad["password_again"] = json!("another");
    let refused = tool.act(bad);
    assert_eq!(refused.kinds(), ["problems"]);
    assert_eq!(
        refused.the("problems")["problems"],
        json!([
            {"field": "username", "message": "Use lower-case letters, digits, - and _, starting with a letter (at most 32)."},
            {"field": "password", "message": "The two don't match."},
        ])
    );
    assert_eq!(refused.state(), "answers");
    // A time zone this computer does not know is the tool's to refuse
    let zones = Path::new("/usr/share/zoneinfo/Europe/Paris").is_file();
    if zones {
        let mut nowhere = answers("alex");
        nowhere["timezone"] = json!("Europe/Atlantis");
        let refused = tool.act(nowhere);
        assert_eq!(refused.the("problems")["problems"][0]["field"], "timezone");
        assert_eq!(refused.state(), "answers");
    }
    let dry = tool.act(answers("alex"));
    assert!(dry.before.is_empty());
    assert_eq!((dry.state(), dry.action()), ("dry-run", "dry-run"));

    let done = tool.act(json!({"type": "dry-run"}));
    assert_eq!(done.kinds(), ["progress"]);
    assert!(done.before.iter().all(|p| p["what"] == "dry-run"));
    let last = done.before.last().unwrap();
    assert!(
        last["total"].as_u64().unwrap() > 0 && last["done"] == last["total"],
        "{last}"
    );
    assert_eq!((done.state(), done.action()), ("done", "none"));
    assert_eq!(
        done.screen["title"],
        "The dry run passed. Nothing was written"
    );
    assert_eq!(done.screen["retry"], Value::Null);
    // The release's images are on disk, so they were walked through
    let plan: Vec<&str> = done.screen["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(
        plan.contains(&"download 16384 bytes, flash:boot_b"),
        "{plan:?}"
    );
    assert!(plan
        .iter()
        .any(|l| l.starts_with("flash userdata: the root image, in ")));
    assert!(plan.contains(&"set_active:b"));

    // After the last screen nothing more is taken
    let over = tool.act(json!({"type": "dry-run"}));
    assert!(
        over.error().contains("The install has ended"),
        "{}",
        over.error()
    );
    assert_eq!(over.state(), "done");

    let ended = tool.quit();
    // One screen for start and one for each action, in the flow's order
    let mut states = vec![
        "warning",
        "phone",
        "google-terms",
        "factory-image",
        "answers",
    ];
    states.extend(vec!["answers"; if zones { 2 } else { 1 }]);
    states.extend(["dry-run", "done", "done"]);
    assert_eq!(ended.states(), states);
    assert_eq!(ended.kinds().first(), Some(&"hello"));
    assert_eq!(ended.kinds().last(), Some(&"bye"));
    // The plan lists what an install would write; the phone received reads
    // only (checked for every run), and this run asked a good many
    assert!(ended.commands().len() > 10, "{:?}", ended.commands());
}

#[test]
fn a_script_can_write_every_line_at_once() {
    // Lines are taken in the order they were written, each after the one
    // before it was answered: a quit, or the end of the input, behind other
    // lines waits its turn
    let zip = zip_file();
    let script: String = [
        json!({"type": "hello", "protocol": 1, "app": "a script", "version": "0"}),
        json!({"type": "start", "reinstall": false}),
        json!({"type": "acknowledge"}),
        json!({"type": "read-phone"}),
        json!({"type": "decline-terms"}),
        json!({"type": "take-file", "path": zip}),
        answers("alex"),
        json!({"type": "dry-run"}),
    ]
    .iter()
    .map(|m| format!("{m}\n"))
    .collect();
    let mut tool = Tool::pretend("ready", &[]);
    tool.owed = 7;
    tool.write(script.as_bytes());
    let ended = tool.finish();
    assert_eq!(ended.code, 0, "{}", ended.stderr);
    assert_eq!(
        ended.states(),
        [
            "warning",
            "phone",
            "google-terms",
            "factory-image",
            "answers",
            "dry-run",
            "done"
        ]
    );
    assert_eq!(ended.said.last().unwrap(), &json!({"type": "bye"}));
}

#[test]
fn a_file_the_owner_hands_in_is_checked_and_a_missing_one_is_an_error() {
    let (mut tool, _) = Tool::started("ready", &[]);
    tool.go_to_take_file();
    // Not there: the tool's words, and the step stays open
    let missing = fixture().join("nothing-here.zip");
    let reply = tool.act(json!({"type": "take-file", "path": missing}));
    assert_eq!(
        reply.error(),
        format!("There is no file {}", missing.display())
    );
    assert_eq!(
        (reply.state(), reply.action()),
        ("factory-image", "take-file")
    );
    assert_eq!(reply.screen["problem"], Value::Null);
    // A path that is not absolute, and none at all
    let reply = tool.act(json!({"type": "take-file", "path": "saved.zip"}));
    assert!(reply.error().contains("absolute path"));
    let reply = tool.act(json!({"type": "take-file"}));
    assert!(reply.error().contains("\"path\""));
    // Not the file: no error, the core's problem on the same step
    let reply = tool.act(json!({"type": "take-file", "path": fixture().join("MANIFEST")}));
    assert!(reply.before.is_empty());
    assert_eq!(reply.state(), "factory-image");
    assert!(reply.screen["problem"]
        .as_str()
        .unwrap()
        .starts_with("MANIFEST is not the file for this phone"));
    // The file: hashed in pieces, with progress
    let reply = tool.act(json!({"type": "take-file", "path": zip_file()}));
    assert_eq!(reply.kinds(), ["progress"]);
    assert_eq!(
        reply.before.last().unwrap(),
        &json!({"type": "progress", "what": "check", "done": 49152, "total": 49152})
    );
    assert_eq!(reply.state(), "answers");
    tool.quit();
}

#[test]
fn a_locked_phone_gets_the_unlock_steps_and_is_looked_at_again() {
    let (mut tool, _) = Tool::started("locked", &[]);
    tool.act(json!({"type": "acknowledge"}));
    let unlock = tool.act(json!({"type": "read-phone"}));
    assert_eq!((unlock.state(), unlock.action()), ("unlock", "read-phone"));
    let commands: Vec<&Value> = unlock.screen["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["command"])
        .collect();
    assert!(commands.contains(&&json!("fastboot flashing unlock")));
    // The pretend owner unlocked it; the same phone is looked at again
    let terms = tool.act(json!({"type": "read-phone"}));
    assert_eq!(terms.state(), "google-terms");
    let ended = tool.quit();
    // The command was shown and never sent: the one question was, once
    let asked = ended
        .commands()
        .iter()
        .filter(|c| **c == "flashing get_unlock_ability")
        .count();
    assert_eq!(asked, 1);
    assert!(!ended.commands().contains(&"flashing unlock"));
}

#[test]
fn no_phone_and_more_than_one_phone() {
    // None in Fastboot Mode: the flow's own words, on the same step
    let (mut tool, _) = Tool::started("none", &[]);
    tool.act(json!({"type": "acknowledge"}));
    let nobody = tool.act(json!({"type": "read-phone"}));
    assert!(nobody.before.is_empty());
    assert_eq!(nobody.state(), "phone");
    assert_eq!(
        nobody.screen["problem"],
        "No phone in Fastboot Mode answered."
    );
    tool.quit();

    // Two: the tool asks which, and the flow is not touched
    let (mut tool, _) = Tool::started("ready,slot-b", &[]);
    tool.act(json!({"type": "acknowledge"}));
    let which = tool.act(json!({"type": "read-phone"}));
    assert_eq!(which.kinds(), ["phones"]);
    assert_eq!(which.state(), "phone");
    assert_eq!(which.screen["problem"], Value::Null);
    let phones = which.the("phones");
    assert!(phones["title"]
        .as_str()
        .unwrap()
        .ends_with("Which one is this install for?"));
    assert_eq!(
        phones["phones"],
        json!([
            {"id": "1", "label": "Pretend phone 1 (ready)"},
            {"id": "2", "label": "Pretend phone 2 (slot-b)"},
        ])
    );
    // An id that was never listed, and one that is not text
    let reply = tool.act(json!({"type": "read-phone", "phone": "3"}));
    assert!(
        reply.error().contains("no phone \"3\""),
        "{}",
        reply.error()
    );
    let reply = tool.act(json!({"type": "read-phone", "phone": 1}));
    assert!(reply.error().contains("as text"));
    assert_eq!(reply.state(), "phone");
    // The one chosen is the one the flow looks at, from then on
    let terms = tool.act(json!({"type": "read-phone", "phone": "1"}));
    assert_eq!(terms.state(), "google-terms");
    tool.act(json!({"type": "decline-terms"}));
    tool.act(json!({"type": "take-file", "path": zip_file()}));
    tool.act(answers("alex"));
    // The dry run goes to that phone without asking again
    let done = tool.act(json!({"type": "dry-run"}));
    assert_eq!(done.state(), "done");
    let ended = tool.quit();
    // The other phone was never asked anything
    assert!(
        ended
            .stderr
            .contains("pretend phone 2 (slot-b): received 0 commands"),
        "{}",
        ended.stderr
    );

    // The other one, chosen: the flow's stop for it
    let (mut tool, _) = Tool::started("ready,slot-b", &[]);
    tool.act(json!({"type": "acknowledge"}));
    tool.act(json!({"type": "read-phone"}));
    let stopped = tool.act(json!({"type": "read-phone", "phone": "2"}));
    assert_eq!(stopped.screen["stop"], "runs-from-slot-b");
    tool.quit();
}

/// Run to the first look at a phone in `state` and return the stop.
fn stop_at_the_first_look(state: &str, reinstall: bool) -> (Tool, Reply) {
    let mut tool = Tool::pretend(state, &[]);
    tool.say(json!({"type": "hello", "protocol": 1}));
    tool.act(json!({"type": "start", "reinstall": reinstall}));
    tool.act(json!({"type": "acknowledge"}));
    let reply = tool.act(json!({"type": "read-phone"}));
    (tool, reply)
}

#[test]
fn each_kind_of_stop_is_the_cores_screen_and_says_whether_starting_again_helps() {
    for (state, stop, retry, footer) in [
        (
            "other-model",
            "wrong-model",
            "after-fix",
            "once that is done",
        ),
        (
            "slot-b",
            "runs-from-slot-b",
            "stops-again",
            "will stop here again",
        ),
        (
            "one-slot",
            "bootloader-slots-unknown",
            "stops-again",
            "will stop here again",
        ),
        (
            "slots-differ",
            "bootloader-slots-differ",
            "stops-again",
            "will stop here again",
        ),
        (
            "unknown-build",
            "build-not-listed",
            "after-fix",
            "once that is done",
        ),
        (
            "untested-build",
            "build-not-tested",
            "stops-again",
            "will stop here again",
        ),
        (
            "untested-old-build",
            "build-not-tested",
            "after-fix",
            "once that is done",
        ),
    ] {
        let (mut tool, stopped) = stop_at_the_first_look(state, false);
        assert!(stopped.before.is_empty(), "{state}");
        assert_eq!(
            (stopped.state(), stopped.action()),
            ("stopped", "none"),
            "{state}"
        );
        assert_eq!(stopped.screen["stop"], stop, "{state}");
        // The typed field and the closing line say the same
        assert_eq!(stopped.screen["retry"], retry, "{state}");
        let closing = stopped.screen["footer"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap();
        assert!(closing.contains(footer), "{state}: {closing}");
        // A stop takes nothing more, and stays what it is
        for action in [
            json!({"type": "read-phone"}),
            json!({"type": "acknowledge"}),
            json!({"type": "dry-run"}),
        ] {
            let reply = tool.act(action);
            assert!(
                reply.error().contains("The install has ended"),
                "{}",
                reply.error()
            );
            assert_eq!(reply.screen, stopped.screen);
        }
        tool.quit();
    }
    // Slot B is where a reinstall lives: said at start, it is no stop
    let (tool, reply) = stop_at_the_first_look("slot-b", true);
    assert_eq!(reply.state(), "google-terms");
    tool.quit();

    // The warning declined: nothing was asked of a phone
    let (mut tool, _) = Tool::started("ready", &[]);
    let stopped = tool.act(json!({"type": "decline"}));
    assert_eq!(
        (&stopped.screen["stop"], &stopped.screen["retry"]),
        (&json!("warning-declined"), &json!("whenever"))
    );
    assert_eq!(stopped.screen["footer"][0], "Nothing was sent to a phone.");
    assert!(tool.quit().commands().is_empty());

    // A release that fails its checks is the first screen: a stop, before
    // the warning. A changed manifest, and a build index that is missing
    let changed = scratch("stop-release");
    let missing = scratch("stop-index");
    for dir in [&changed, &missing] {
        for entry in std::fs::read_dir(fixture()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                std::fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
            }
        }
    }
    let mut manifest = std::fs::read(changed.join("manifest.json")).unwrap();
    manifest[100] ^= 1;
    std::fs::write(changed.join("manifest.json"), manifest).unwrap();
    std::fs::remove_file(missing.join("factory-builds.json")).unwrap();
    for (dir, stop) in [
        (&changed, "release-not-verified"),
        (&missing, "build-index-refused"),
    ] {
        let mut tool =
            Tool::with_args(&["--release", dir.to_str().unwrap(), "--fake-phone", "ready"]);
        tool.hear();
        tool.say(json!({"type": "hello", "protocol": 1}));
        let first = tool.act(json!({"type": "start", "reinstall": false}));
        assert_eq!((first.state(), first.action()), ("stopped", "none"));
        assert_eq!(
            (&first.screen["stop"], &first.screen["retry"]),
            (&json!(stop), &json!("after-fix"))
        );
        let reply = tool.act(json!({"type": "acknowledge"}));
        assert!(reply.error().contains("The install has ended"));
        assert!(tool.quit().commands().is_empty());
    }

    // Another phone than the first one the install looked at
    let (mut tool, _) = Tool::started("locked,ready", &[]);
    tool.act(json!({"type": "acknowledge"}));
    tool.act(json!({"type": "read-phone"}));
    let unlock = tool.act(json!({"type": "read-phone", "phone": "1"}));
    assert_eq!(unlock.state(), "unlock");
    let stopped = tool.act(json!({"type": "read-phone", "phone": "2"}));
    assert_eq!(
        (&stopped.screen["stop"], &stopped.screen["retry"]),
        (&json!("phone-changed"), &json!("after-fix"))
    );
    assert!(stopped.screen["footer"][0]
        .as_str()
        .unwrap()
        .contains("each of the two phones"));
    tool.quit();

    // A download that is not the file the release lists
    let mut wrong = zip();
    let middle = wrong.len() / 2;
    wrong[middle] ^= 1;
    let (google, _) = stand_in(move |asked, stream| send_file(&wrong, asked, stream));
    let work = scratch("stop-mismatch");
    let (mut tool, _) = Tool::started(
        "ready",
        &[
            "--fake-google",
            &google.to_string(),
            "--work-dir",
            work.to_str().unwrap(),
        ],
    );
    tool.act(json!({"type": "acknowledge"}));
    tool.act(json!({"type": "read-phone"}));
    tool.act(json!({"type": "accept-terms"}));
    let stopped = tool.act(json!({"type": "download", "do": "start"}));
    assert_eq!(stopped.kinds(), ["progress"]);
    assert_eq!(
        (&stopped.screen["stop"], &stopped.screen["retry"]),
        (&json!("factory-download-mismatch"), &json!("whenever"))
    );
    // What was downloaded of it is deleted, as the stop says
    assert!(std::fs::read_dir(work.join("factory"))
        .unwrap()
        .next()
        .is_none());
    tool.quit();
}

// ---- What the app may get wrong ------------------------------------------------------

#[test]
fn malformed_lines_get_an_error_and_never_move_the_flow() {
    // Before start there is no screen to answer with: an error, alone
    let mut tool = Tool::pretend("ready", &[]);
    for line in [
        "this is not JSON\n",
        "[1, 2]\n",
        "\"quit\"\n",
        "{\"no\": \"type\"}\n",
        "{\"type\": 7}\n",
        "{\"type\": \"acknowledge\"}\n",
        "{\"type\": \"surprise\"}\n",
        // start before hello
        "{\"type\": \"start\", \"reinstall\": false}\n",
    ] {
        tool.write(line.as_bytes());
        assert_eq!(tool.hear()["type"], "error", "{line}");
    }
    // An empty line is skipped
    tool.write(b"\n   \n");
    assert!(tool.hear_within(Duration::from_millis(200)).is_none());
    tool.say(json!({"type": "hello", "protocol": 1}));
    // start says whether this is a reinstall, as true or false
    for start in [
        json!({"type": "start"}),
        json!({"type": "start", "reinstall": "no"}),
    ] {
        tool.say(start);
        assert!(tool.hear()["message"]
            .as_str()
            .unwrap()
            .contains("\"reinstall\""));
    }
    let first = tool.act(json!({"type": "start", "reinstall": false}));
    assert_eq!(first.state(), "warning");

    // After start: an error, then the screen, which has not changed
    let long = format!(
        "{{\"type\": \"acknowledge\", \"padding\": \"{}\"}}\n",
        "x".repeat(1 << 20)
    );
    for line in [
        b"this is not JSON\n".to_vec(),
        b"{\"type\": \"acknowledge\"\n".to_vec(),
        b"[]\n".to_vec(),
        b"{\"no\": \"type\"}\n".to_vec(),
        b"{\"type\": \"surprise\"}\n".to_vec(),
        b"{\"type\": \"write\"}\n".to_vec(),
        b"{\"type\": \"acknowledge\xff\"}\n".to_vec(),
        b"\xff\xfe\x00\n".to_vec(),
        long.into_bytes(),
        // said once, before the install
        b"{\"type\": \"hello\", \"protocol\": 1}\n".to_vec(),
        b"{\"type\": \"start\", \"reinstall\": true}\n".to_vec(),
    ] {
        let reply = tool.act_raw(&line);
        let shown = String::from_utf8_lossy(&line[..line.len().min(60)]).into_owned();
        assert!(!reply.error().is_empty(), "{shown}");
        assert_eq!(reply.screen, first.screen, "{shown}");
    }
    // The flow is where it was, and goes on
    tool.go_to_take_file();
    tool.act(json!({"type": "take-file", "path": zip_file()}));

    // Answers that are not seven texts: an error, and nothing of them is
    // repeated (the PIN is among them; `finish` looks for it everywhere)
    let mut short = answers("alex");
    short.as_object_mut().unwrap().remove("wifi_psk");
    let reply = tool.act(short);
    assert!(reply.error().contains("\"wifi_psk\""), "{}", reply.error());
    let mut odd = answers("alex");
    odd["hostname"] = json!(7);
    assert!(tool.act(odd).error().contains("\"hostname\""));
    let cut = format!("{{\"type\": \"answers\", \"password\": \"{PIN}\", \"username\": \n");
    let reply = tool.act_raw(cut.as_bytes());
    assert!(reply.error().contains("not a JSON object"));
    assert_eq!(reply.state(), "answers");
    let dry = tool.act(answers("alex"));
    assert_eq!(dry.state(), "dry-run");
    tool.quit();

    // A last line without its line feed still counts
    let mut tool = Tool::pretend("ready", &[]);
    tool.write(
        b"{\"type\": \"hello\", \"protocol\": 1}\n{\"type\": \"start\", \"reinstall\": false}",
    );
    tool.owed = 1;
    assert_eq!(tool.finish().kinds(), ["hello", "screen", "bye"]);
}

#[test]
fn an_action_out_of_turn_is_refused_and_reaches_no_phone() {
    let all = [
        ("acknowledge", json!({"type": "acknowledge"})),
        ("acknowledge", json!({"type": "decline"})),
        ("read-phone", json!({"type": "read-phone"})),
        ("accept-terms", json!({"type": "accept-terms"})),
        ("accept-terms", json!({"type": "decline-terms"})),
        ("download", json!({"type": "download", "do": "start"})),
        (
            "take-file",
            json!({"type": "take-file", "path": zip_file()}),
        ),
        ("answers", answers("alex")),
        ("dry-run", json!({"type": "dry-run"})),
    ];
    let (mut tool, first) = Tool::started("ready", &[]);
    let mut at = first.screen;
    // At each step, every action the screen does not name: an error, and
    // the same screen
    let path = [
        json!({"type": "acknowledge"}),
        json!({"type": "read-phone"}),
        json!({"type": "decline-terms"}),
        json!({"type": "take-file", "path": zip_file()}),
        answers("alex"),
    ];
    let mut steps = path.into_iter();
    loop {
        let asks = at["action"].as_str().unwrap().to_string();
        for (action, message) in &all {
            if *action == asks || (asks == "download" && *action == "take-file") {
                continue;
            }
            let reply = tool.act(message.clone());
            let said = reply.error();
            assert!(
                said.contains(&format!("the screen asks for \"{asks}\"")),
                "{message} at {asks}: {said}"
            );
            assert_eq!(reply.screen, at, "{message} at {asks}");
        }
        // A cancel with no download running is ignored: no answer at all
        tool.say(json!({"type": "download", "do": "cancel"}));
        let Some(next) = steps.next() else { break };
        at = tool.act(next).screen;
    }
    assert_eq!(at["state"], "dry-run");
    let ended = tool.quit();
    // Before the warning was accepted, and at every refusal since, nothing
    // went to the phone: the one look is all it saw
    let looks = ended
        .commands()
        .iter()
        .filter(|c| **c == "getvar:product")
        .count();
    assert_eq!(looks, 1, "{:?}", ended.commands());

    // The warning's screen, alone: a refused look at the phone asks it nothing
    let (mut tool, _) = Tool::started("ready", &[]);
    for message in [
        json!({"type": "read-phone"}),
        json!({"type": "dry-run"}),
        json!({"type": "read-phone", "phone": "1"}),
    ] {
        tool.act(message);
    }
    assert!(tool.quit().commands().is_empty());

    // download says what to do, and only start and cancel exist
    let (mut tool, _) = Tool::started("ready", &[]);
    tool.act(json!({"type": "acknowledge"}));
    tool.act(json!({"type": "read-phone"}));
    tool.act(json!({"type": "accept-terms"}));
    for odd in [
        json!({"type": "download"}),
        json!({"type": "download", "do": "sideways"}),
    ] {
        let reply = tool.act(odd);
        assert!(reply.error().contains("\"start\" or \"cancel\""));
        assert_eq!(reply.action(), "download");
    }
    // A pretend run with no stand-in fetches nothing from anywhere: the
    // core's fallback to the owner's own download
    let reply = tool.act(json!({"type": "download", "do": "start"}));
    assert_eq!(reply.action(), "download");
    assert!(reply.screen["problem"]
        .as_str()
        .unwrap()
        .contains("a pretend run fetches nothing from Google"));
    tool.quit();
}

// ---- Ending a download, and the run ----------------------------------------------------

/// A run at the factory-image step, terms accepted, against a server that
/// sends 20000 bytes and then stands still until `go`.
fn at_a_download_that_stands_still(name: &str) -> (Tool, Arc<AtomicBool>, PathBuf) {
    let file = zip();
    let go = Arc::new(AtomicBool::new(false));
    let wait = go.clone();
    let (google, _) = stand_in(move |asked, stream| {
        if wait.load(Ordering::SeqCst) {
            send_file(&file, asked, stream)
        } else {
            send_some_and_wait(&file, 20000, &wait, stream)
        }
    });
    let work = scratch(name);
    let (mut tool, _) = Tool::started(
        "ready",
        &[
            "--fake-google",
            &google.to_string(),
            "--work-dir",
            work.to_str().unwrap(),
        ],
    );
    tool.act(json!({"type": "acknowledge"}));
    tool.act(json!({"type": "read-phone"}));
    assert_eq!(
        tool.act(json!({"type": "accept-terms"})).action(),
        "download"
    );
    (tool, go, work)
}

/// Start the download and read progress until the 20000 bytes are in.
fn start_and_stall(tool: &mut Tool) {
    tool.owed += 1;
    tool.say(json!({"type": "download", "do": "start"}));
    loop {
        let progress = tool.hear();
        assert_eq!(progress["type"], "progress", "{progress}");
        if progress["done"] == 20000 {
            break;
        }
    }
}

#[test]
fn a_running_download_is_cancelled_and_goes_on_from_what_arrived() {
    let (mut tool, go, work) = at_a_download_that_stands_still("cancel");
    start_and_stall(&mut tool);
    // Nothing more comes while the transfer stands still
    assert!(tool.hear_within(Duration::from_millis(400)).is_none());
    tool.say(json!({"type": "download", "do": "cancel"}));
    // The download's one screen: the core's, for a transfer that broke off
    let reply = tool.reply();
    assert!(reply.before.iter().all(|m| m["type"] == "progress"));
    assert_eq!(
        (reply.state(), reply.action()),
        ("factory-image", "download")
    );
    let problem = reply.screen["problem"].as_str().unwrap();
    assert!(
        problem.contains("stopped before it was complete: 20000 of 49152 bytes had arrived"),
        "{problem}"
    );
    // What arrived is kept
    let part = work.join("factory").join(format!("{ZIP}.part"));
    assert_eq!(std::fs::metadata(&part).unwrap().len(), 20000);
    // A cancel now finds nothing running, and is ignored
    tool.say(json!({"type": "download", "do": "cancel"}));
    assert!(tool.hear_within(Duration::from_millis(300)).is_none());
    // The next try goes on from the kept bytes, to the whole file
    go.store(true, Ordering::SeqCst);
    let reply = tool.act(json!({"type": "download", "do": "start"}));
    assert_eq!(reply.state(), "answers");
    assert_eq!(
        std::fs::read(work.join("factory").join(ZIP)).unwrap(),
        zip()
    );
    assert!(!part.exists());
    tool.quit();
}

#[test]
fn quit_and_the_end_of_the_input_end_a_running_download() {
    for close in [false, true] {
        let (mut tool, _go, work) =
            at_a_download_that_stands_still(if close { "close" } else { "quit" });
        start_and_stall(&mut tool);
        if !close {
            tool.say(json!({"type": "quit"}));
        }
        // The input ends (in `finish`), or quit was said: the download is
        // ended as one that broke off, its screen is sent, then bye
        let ended = tool.finish();
        assert_eq!(ended.code, 0, "{}", ended.stderr);
        let kinds = ended.kinds();
        assert_eq!(kinds[kinds.len() - 2..], ["screen", "bye"], "{kinds:?}");
        let last = &ended.said[ended.said.len() - 2]["screen"];
        assert!(last["problem"]
            .as_str()
            .unwrap()
            .contains("20000 of 49152 bytes had arrived"));
        assert_eq!(
            std::fs::metadata(work.join("factory").join(format!("{ZIP}.part")))
                .unwrap()
                .len(),
            20000
        );
    }
}

#[test]
fn the_input_ending_mid_flow_is_a_quit() {
    // At each step of the way: bye, status 0, and nothing but reads
    let steps = [
        json!({"type": "acknowledge"}),
        json!({"type": "read-phone"}),
        json!({"type": "decline-terms"}),
        json!({"type": "take-file", "path": zip_file()}),
        answers("alex"),
        json!({"type": "dry-run"}),
    ];
    for upto in 0..=steps.len() {
        let (mut tool, _) = Tool::started("ready", &[]);
        for step in &steps[..upto] {
            tool.act(step.clone());
        }
        let ended = tool.finish();
        assert_eq!(ended.code, 0, "after {upto} steps: {}", ended.stderr);
        assert_eq!(ended.kinds().last(), Some(&"bye"), "after {upto} steps");
        assert_eq!(ended.states().len(), upto + 1);
    }
}

#[test]
fn nothing_but_reads_reaches_a_phone_whatever_is_sent() {
    // Every message a write might be asked with, at every step up to and
    // past the dry run. `finish` reads the pretend phone's own record of
    // what it received: getvar and nothing else, no partition changed
    let asks = [
        json!({"type": "write"}),
        json!({"type": "flash"}),
        json!({"type": "install"}),
        json!({"type": "erase", "partition": "userdata"}),
        json!({"type": "set-active", "slot": "b"}),
        json!({"type": "reboot"}),
        json!({"type": "unlock"}),
        json!({"type": "command", "command": "flash:boot_b"}),
    ];
    let steps = [
        json!({"type": "acknowledge"}),
        json!({"type": "read-phone"}),
        json!({"type": "decline-terms"}),
        json!({"type": "take-file", "path": zip_file()}),
        answers("alex"),
        // A field the protocol does not have is ignored: this is a dry run
        json!({"type": "dry-run", "write": true, "dry_run": false, "really": "yes"}),
    ];
    let (mut tool, _) = Tool::started("ready", &[]);
    let mut state = String::from("warning");
    for step in steps {
        for ask in &asks {
            let reply = tool.act(ask.clone());
            assert!(!reply.error().is_empty());
            assert_eq!(reply.state(), state, "{ask}");
        }
        state = tool.act(step).state().to_string();
    }
    assert_eq!(state, "done");
    for ask in &asks {
        assert_eq!(tool.act(ask.clone()).state(), "done");
    }
    let ended = tool.quit();
    // The dry run walked the release's images: the plan names the writes an
    // install would send, and not one of them was sent
    let done = ended
        .said
        .iter()
        .rev()
        .find(|m| m["type"] == "screen")
        .unwrap();
    assert!(done["screen"]["plan"].as_array().unwrap().len() >= 7);
    let commands = ended.commands();
    assert!(commands.len() > 10);
    for write in [
        "flash",
        "erase",
        "set_active",
        "reboot",
        "download",
        "oem",
        "flashing unlock",
    ] {
        assert!(
            !commands.iter().any(|c| c.starts_with(write)),
            "{write}: {commands:?}"
        );
    }
    assert!(ended.stderr.contains("0 others; partitions written: 0"));
}
