// SPDX-License-Identifier: MIT
//! `omamobile-install json`: the install flow as JSON lines, for the desktop
//! app, agents and scripts. The protocol is docs/APP.md §3, number 2.
//!
//! One JSON object per line on standard input and on standard output.
//! Standard output carries the protocol and nothing else: this file takes it
//! away from the rest of the process before it says anything ([`claim`]), a
//! panic is reported there as `fatal`, and what a person might want to read
//! besides goes to standard error.
//!
//! The tool says `hello` first. Before `start` it reads no release, opens no
//! USB device and fetches nothing. From `start` on it holds one
//! [`Flow`], sends the flow's [`Screen`](omamobile_installer_core::flow::Screen)
//! as serde serialises it, and hands the flow what each message says the
//! person did. It decides nothing about the install: an action the current
//! screen does not name is refused here, before the flow or a phone is
//! touched, and everything else is the flow's answer.
//!
//! Without `--write` this mode ends at the dry run, and nothing that is
//! sent to it can change that: whether the flow may write is said once,
//! when the tool is started, and never by a message. With `--write` the
//! flow goes on as `install --write` does: the dry run, the list of what
//! will be written, the phone's codename as the owner typed it (`confirm`),
//! and then the writes (`write`), through the one session the flow opens
//! (`Flow::begin_write`). This file opens no session of its own.
//!
//! The arguments are a closed list ([`Args::parse`]), and anything else is
//! refused before `hello`: there is none that names a phone, none for the
//! owner's statement about a bootloader slot the phone does not report
//! (that is `install`'s, typed by the owner on a command line), and none
//! for the maintainer's override.
use crate::{
    download_factory_zip, is_fastboot, parse_proxy, piece_line, take_factory_zip, walk_images,
    Bundle, Fetcher, Res, Watch,
};
use omamobile_installer_core::fastboot::{PhoneId, PieceReport, Transport};
use omamobile_installer_core::flow::{
    AnswerField, AnswerProblem, AnswersInput, Flow, FlowOptions, OwnerAnswers,
};
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_core::Error;
use omamobile_installer_usb::UsbTransport;
use pollster::block_on;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The protocol this tool speaks (docs/APP.md §3).
pub const PROTOCOL: u64 = 2;
/// A line longer than this is not a message of this protocol.
const LONGEST_LINE: usize = 1 << 20;

// ---- Standard output: the protocol, and nothing else -----------------------

/// Where protocol messages go. Set once, by [`claim`].
static WIRE: OnceLock<Mutex<Box<dyn Write + Send>>> = OnceLock::new();

/// Take standard output for the protocol. On Unix the descriptor is moved
/// aside and file descriptor 1 is pointed at standard error, so whatever
/// else in this process prints to standard output (this crate's other
/// commands, a dependency) cannot reach the reader of the protocol.
fn claim() -> io::Result<()> {
    #[cfg(unix)]
    let wire: Box<dyn Write + Send> = {
        use std::os::fd::AsFd;
        let ours = io::stdout().as_fd().try_clone_to_owned()?;
        rustix::stdio::dup2_stdout(io::stderr().as_fd())?;
        Box::new(std::fs::File::from(ours))
    };
    #[cfg(not(unix))]
    let wire: Box<dyn Write + Send> = Box::new(io::stdout());
    let _ = WIRE.set(Mutex::new(wire));
    Ok(())
}

/// Nobody reads the protocol any more: a message could not be written.
static DEAF: AtomicBool = AtomicBool::new(false);

/// Send one message: one line. If the reader is gone there is nobody left to
/// tell. The tool does not end here for that: a message is also sent in the
/// middle of a write to a phone, and a write that was begun is finished
/// whoever still listens. It ends where the lines are taken in turn
/// ([`Driver::run`]), between two actions.
fn say(message: Value) {
    if DEAF.load(Ordering::SeqCst) {
        return;
    }
    // serde_json escapes every control character: no line feed in a message
    let mut line = message.to_string();
    line.push('\n');
    let Some(wire) = WIRE.get() else { return };
    let mut wire = wire.lock().unwrap_or_else(|e| e.into_inner());
    if wire
        .write_all(line.as_bytes())
        .and_then(|()| wire.flush())
        .is_err()
    {
        DEAF.store(true, Ordering::SeqCst);
        eprintln!("omamobile-install: nobody reads the protocol any more; leaving once the step in hand is over");
    }
}

/// The tool itself failed: said as `fatal`, and the status is not 0.
fn fatal(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    say(json!({"type": "fatal", "message": message}));
    ExitCode::FAILURE
}

// ---- Arguments ---------------------------------------------------------------

/// What `json` was started with. A closed list: there is no argument for a
/// serial number, for the owner's statement about a bootloader slot or for
/// the maintainer's override, and an argument that is not listed here stops
/// the mode before it says `hello`.
#[derive(Debug, Default)]
pub struct Args {
    /// The flow may write: `--write`, said when the tool is started and by
    /// nothing else. Without it the flow ends at the dry run.
    write: bool,
    /// The proxy the owner named for the download of Google's file.
    proxy: Option<ureq::Proxy>,
    /// The release's directory. Without one the mode still says `hello`
    /// (so that a front end can ask its version), and `start` fails.
    release: Option<PathBuf>,
    /// Where a download of Google's factory image is kept.
    work_dir: Option<PathBuf>,
    /// The pretend phones, by state, in place of USB.
    #[cfg(feature = "fake-phone")]
    fake_phone: Option<String>,
    /// The stand-in for Google's server, for a pretend run.
    #[cfg(feature = "fake-phone")]
    fake_google: Option<String>,
    /// The pretend phone's cable is pulled as the Nth command that starts
    /// with a text is sent: `flash:userdata:2`.
    #[cfg(feature = "fake-phone")]
    fake_unplug: Option<(String, usize)>,
}

impl Args {
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        let mut out = Args::default();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let mut value = || {
                it.next()
                    .map(|v| v.to_string())
                    .ok_or_else(|| format!("{arg} needs a value"))
            };
            match *arg {
                "--release" => out.release = Some(PathBuf::from(value()?)),
                "--work-dir" => out.work_dir = Some(PathBuf::from(value()?)),
                // The one way this mode's flow may write. It is never implied
                "--write" => out.write = true,
                // The one way a proxy is used: named here, by the owner
                "--proxy" => {
                    out.proxy =
                        Some(parse_proxy(Some(&value()?)).map_err(|e| e.to_string())?)
                }
                #[cfg(feature = "fake-phone")]
                "--fake-phone" => out.fake_phone = Some(value()?),
                #[cfg(feature = "fake-phone")]
                "--fake-google" => out.fake_google = Some(value()?),
                #[cfg(feature = "fake-phone")]
                "--fake-unplug" => {
                    let value = value()?;
                    let parsed = value
                        .rsplit_once(':')
                        .and_then(|(command, nth)| Some((command.to_string(), nth.parse().ok()?)));
                    out.fake_unplug = Some(parsed.ok_or(
                        "--fake-unplug takes the start of a command and a count, like flash:userdata:2",
                    )?);
                }
                other => {
                    return Err(format!(
                        "json takes --release DIR, --work-dir DIR, --proxy http://HOST:PORT and --write, and nothing else: not {other}. This mode has no argument that names a phone, states anything about one, or skips a check"
                    ))
                }
            }
        }
        #[cfg(feature = "fake-phone")]
        if (out.fake_google.is_some() || out.fake_unplug.is_some()) && out.fake_phone.is_none() {
            return Err(
                "a stand-in for Google's server and a pulled cable are for the pretend phone only: --fake-phone STATE"
                    .into(),
            );
        }
        Ok(out)
    }
}

// ---- Standard input ----------------------------------------------------------

enum Input {
    Line(Vec<u8>),
    TooLong,
    /// Standard input ended. It stays ended.
    Closed,
}

/// What the next line is, as far as a running step needs to know.
enum Front {
    /// `download` with `"do": "cancel"`.
    Cancel,
    /// `quit`, or the end of the input.
    Quit,
    /// Another message, or none yet.
    Other,
}

/// The app's lines, in the order they were written. They are read on a
/// thread of their own so that a step that runs can see a cancel or a quit;
/// nothing is ever taken out of turn.
struct Lines {
    arriving: Receiver<Input>,
    ahead: VecDeque<Input>,
    closed: bool,
}

fn parse(line: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice(line) {
        Ok(Value::Object(message)) => Some(message),
        _ => None,
    }
}

fn kind(message: &Map<String, Value>) -> Option<&str> {
    message.get("type").and_then(Value::as_str)
}

fn is_cancel(message: &Map<String, Value>) -> bool {
    kind(message) == Some("download") && message.get("do").and_then(Value::as_str) == Some("cancel")
}

impl Lines {
    fn read() -> Self {
        let (tx, arriving) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let (mut line, mut too_long) = (Vec::new(), false);
            loop {
                let (take, ended) = match stdin.fill_buf() {
                    Ok(buf) if !buf.is_empty() => {
                        let end = buf.iter().position(|b| *b == b'\n');
                        let take = end.map_or(buf.len(), |at| at + 1);
                        if line.len() + take > LONGEST_LINE {
                            too_long = true;
                            line.clear();
                        }
                        if !too_long {
                            line.extend_from_slice(&buf[..take]);
                        }
                        (take, end.is_some())
                    }
                    _ => break,
                };
                stdin.consume(take);
                if ended {
                    let input = if too_long {
                        Input::TooLong
                    } else {
                        Input::Line(std::mem::take(&mut line))
                    };
                    too_long = false;
                    if tx.send(input).is_err() {
                        return;
                    }
                }
            }
            // A last line that lacks its line feed is still a line
            if too_long {
                let _ = tx.send(Input::TooLong);
            } else if !line.is_empty() {
                let _ = tx.send(Input::Line(line));
            }
            let _ = tx.send(Input::Closed);
        });
        Lines {
            arriving,
            ahead: VecDeque::new(),
            closed: false,
        }
    }

    /// The next line, waited for. An empty line is skipped.
    fn next(&mut self) -> Input {
        loop {
            let input = match self.ahead.pop_front() {
                Some(input) => input,
                None if self.closed => Input::Closed,
                None => self.arriving.recv().unwrap_or(Input::Closed),
            };
            match input {
                Input::Closed => {
                    self.closed = true;
                    return Input::Closed;
                }
                Input::Line(line) if line.trim_ascii().is_empty() => {}
                other => return other,
            }
        }
    }

    /// A look at the next line without waiting for one or taking it.
    fn front(&mut self) -> Front {
        loop {
            if self.ahead.is_empty() && !self.closed {
                match self.arriving.try_recv() {
                    Ok(input) => self.ahead.push_back(input),
                    Err(TryRecvError::Empty) => return Front::Other,
                    Err(TryRecvError::Disconnected) => self.closed = true,
                }
            }
            match self.ahead.front() {
                None => {
                    return if self.closed {
                        Front::Quit
                    } else {
                        Front::Other
                    }
                }
                Some(Input::Closed) => return Front::Quit,
                Some(Input::Line(line)) if line.trim_ascii().is_empty() => {
                    self.ahead.pop_front();
                }
                Some(Input::Line(line)) => {
                    return match parse(line) {
                        Some(m) if kind(&m) == Some("quit") => Front::Quit,
                        Some(m) if is_cancel(&m) => Front::Cancel,
                        _ => Front::Other,
                    }
                }
                Some(Input::TooLong) => return Front::Other,
            }
        }
    }
}

/// A step that runs, as the protocol sees it: `progress` a few times a
/// second, a cancel that ends a download, and a quit (or the end of the
/// input) that ends a download or the check of a file. The line that ended
/// it stays where it is, for the loop that takes the lines in turn.
struct Wire<'a> {
    lines: &'a mut Lines,
    /// When the last `progress` went out, what it was of, and its `done`.
    told: Option<(Instant, &'static str, u64)>,
}

impl<'a> Wire<'a> {
    fn new(lines: &'a mut Lines) -> Self {
        Wire { lines, told: None }
    }
}

impl Watch for Wire<'_> {
    fn note(&mut self, text: &str) {
        eprintln!("{text}");
    }

    fn progress(&mut self, what: &'static str, done: u64, total: u64) {
        let due = match self.told {
            None => true,
            Some((_, of, _)) if of != what => true,
            Some((_, _, last)) if last == done => false,
            Some((at, _, _)) => done == total || at.elapsed() >= Duration::from_millis(250),
        };
        if due {
            self.told = Some((Instant::now(), what, done));
            say(json!({"type": "progress", "what": what, "done": done, "total": total}));
        }
    }

    fn sent(&mut self, command: &str) {
        say(json!({"type": "sent", "command": command}));
    }

    fn piece(&mut self, number: u32, piece: &PieceReport) {
        say(json!({
            "type": "piece",
            "number": number,
            "blocks": piece.blocks,
            "bytes": piece.bytes,
            "seconds": piece.flash.map(|took| took.as_secs_f64()),
            // The same words the terminal prints for it
            "line": format!("flash:userdata ({})", piece_line(number, piece)),
        }));
    }

    fn account(&mut self, lines: &[String]) {
        if !lines.is_empty() {
            say(json!({"type": "account", "lines": lines}));
        }
    }

    fn go_on(&mut self, what: &'static str) -> bool {
        // A dry run only reads and is soon over: it is finished, not cut
        // off. A write is never cut off from here: what was begun is sent
        if what != "download" && what != "check" {
            return true;
        }
        // Nobody is left to download or check a file for
        if DEAF.load(Ordering::SeqCst) {
            return false;
        }
        loop {
            match self.lines.front() {
                Front::Other => return true,
                Front::Quit => return false,
                Front::Cancel => {
                    self.lines.ahead.pop_front();
                    // Only a download is cancelled; elsewhere it is ignored
                    if what == "download" {
                        return false;
                    }
                }
            }
        }
    }
}

// ---- The phones --------------------------------------------------------------

/// A phone in Fastboot Mode, as much as is said of one. `label` is what a
/// person tells it from another by; it is never a serial number.
struct Connected {
    /// Tells this phone from the others for this run. Its serial number
    /// where it has one: kept here, compared, and printed nowhere.
    identity: String,
    label: String,
}

/// Where the phones are: USB, or a pretend build's memory.
trait Phones {
    type Link: Transport;
    /// The phones in Fastboot Mode now.
    fn look(&mut self) -> Res<Vec<Connected>>;
    /// Open the `nth` phone of the last look.
    fn open(&mut self, nth: usize) -> Res<(Self::Link, PhoneId)>;
}

#[derive(Default)]
struct Usb {
    seen: Vec<nusb::DeviceInfo>,
}

impl Phones for Usb {
    type Link = UsbTransport;

    fn look(&mut self) -> Res<Vec<Connected>> {
        self.seen = block_on(nusb::list_devices())?
            .filter(is_fastboot)
            .collect();
        Ok(self
            .seen
            .iter()
            .map(|info| {
                let name = info.product_string().unwrap_or("A phone in Fastboot Mode");
                #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
                let place = {
                    let ports: Vec<String> =
                        info.port_chain().iter().map(|p| p.to_string()).collect();
                    format!("USB bus {}, port {}", info.bus_id(), ports.join("."))
                };
                #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
                let place = String::from("USB");
                Connected {
                    identity: match info.serial_number() {
                        Some(serial) if !serial.is_empty() => format!("serial {serial}"),
                        _ => format!("at {place}"),
                    },
                    label: format!("{name} ({place})"),
                }
            })
            .collect())
    }

    fn open(&mut self, nth: usize) -> Res<(UsbTransport, PhoneId)> {
        let info = self.seen.get(nth).ok_or("that phone is not connected")?;
        let id = info
            .serial_number()
            .and_then(PhoneId::of_serial)
            .ok_or("this phone reports no serial number, so the install can't be pinned to it")?;
        let device = block_on(info.open())?;
        Ok((block_on(UsbTransport::open(&device))?, id))
    }
}

/// The tests' pretend phones, held in memory, in place of USB: one for each
/// state named with `--fake-phone` (`ready`, or `ready,slot-b` for two, or
/// `none`). Only in a build with the `fake-phone` feature.
#[cfg(feature = "fake-phone")]
struct Pretend {
    phones: Vec<PretendPhone>,
}

#[cfg(feature = "fake-phone")]
struct PretendPhone {
    state: String,
    phone: crate::fake::FakePhone,
    serial: String,
    opened: usize,
}

#[cfg(feature = "fake-phone")]
impl Pretend {
    fn new(states: &str, unplug: Option<(String, usize)>) -> Res<Self> {
        use crate::fake::FakePhone;
        let mut phones = Vec::new();
        for state in states.split(',').filter(|s| *s != "none") {
            let phone = FakePhone::scenario(state).ok_or_else(|| {
                format!(
                    "no pretend phone '{state}'; there are: {}, and none",
                    FakePhone::SCENARIOS.join(", ")
                )
            })?;
            // Placeholders, never a real serial number
            let serial = format!("FAKESERIAL{:04}", phones.len() + 1);
            phone.edit(|s| {
                s.vars.insert("serialno".into(), serial.clone());
                s.unplug_on = unplug.clone();
            });
            phones.push(PretendPhone {
                state: state.to_string(),
                phone,
                serial,
                opened: 0,
            });
        }
        Ok(Pretend { phones })
    }

    /// What each pretend phone received, on standard error: every command,
    /// then the count. A run that was not started to write shows `getvar`
    /// and, for a locked phone, the one read-only question, and nothing else.
    fn record(&self) {
        for (n, p) in self.phones.iter().enumerate() {
            let name = format!("pretend phone {} ({})", n + 1, p.state);
            let log = p.phone.log();
            for command in &log {
                eprintln!("{name} <- {command}");
            }
            let (asked, other) = (p.phone.unlock_questions(), p.phone.not_reads().len());
            eprintln!(
                "{name}: received {} commands: {} getvar, {asked} flashing get_unlock_ability, {other} others; partitions written: {}",
                log.len(),
                log.len() - asked - other,
                p.phone.written().len()
            );
        }
    }
}

#[cfg(feature = "fake-phone")]
impl Phones for Pretend {
    type Link = crate::fake::FakePhone;

    fn look(&mut self) -> Res<Vec<Connected>> {
        Ok(self
            .phones
            .iter()
            .enumerate()
            .map(|(n, p)| Connected {
                identity: format!("serial {}", p.serial),
                label: format!("Pretend phone {} ({})", n + 1, p.state),
            })
            .collect())
    }

    fn open(&mut self, nth: usize) -> Res<(Self::Link, PhoneId)> {
        let p = self.phones.get_mut(nth).ok_or("no such pretend phone")?;
        p.opened += 1;
        // A locked pretend phone is unlocked by its pretend owner after the
        // unlock steps were shown once, as in `flow --fake-phone locked`
        if p.state == "locked" && p.opened == 2 {
            eprintln!("(the pretend phone's owner unlocked it)");
            p.phone.edit(|s| {
                s.vars.insert("unlocked".into(), "yes".into());
            });
        }
        let id = PhoneId::of_serial(&p.serial).ok_or("the pretend phone has no serial")?;
        Ok((p.phone.clone(), id))
    }
}

// ---- The flow, message by message ----------------------------------------------

/// The release's files and the keys that may have signed them.
type Trust = fn(&Path) -> Res<(Bundle, TrustedKeys)>;

fn release_keys(dir: &Path) -> Res<(Bundle, TrustedKeys)> {
    Ok((
        Bundle::load(dir, "omamobile.gpg")?,
        TrustedKeys::omamobile(),
    ))
}

/// A test bundle's own key, which only a pretend run trusts: there is no way
/// from here to USB.
#[cfg(feature = "fake-phone")]
fn test_key(dir: &Path) -> Res<(Bundle, TrustedKeys)> {
    let fingerprint = String::from_utf8(crate::read(dir, "test-key.fingerprint")?)?;
    Ok((
        Bundle::load(dir, "test-key.gpg")?,
        TrustedKeys::new(&[fingerprint.trim()])?,
    ))
}

/// Why the mode ends other than by `quit`.
struct Fatal(String);

struct Started {
    flow: Flow,
    /// The release's directory, for the images a dry run walks.
    dir: PathBuf,
}

struct Driver<P: Phones> {
    phones: P,
    trust: Trust,
    release: Option<PathBuf>,
    work_dir: Option<PathBuf>,
    fetcher: Fetcher,
    /// The owner named a proxy for the download (`--proxy`).
    proxied: bool,
    /// The tool was started with `--write`.
    may_write: bool,
    lines: Lines,
    greeted: bool,
    started: Option<Started>,
    /// Every phone this run has listed. A phone's `id` on the wire is its
    /// place in this list, from 1: a name made up for this run.
    listed: Vec<String>,
    /// The phone the flow last looked at.
    chosen: Option<String>,
}

enum Found {
    Nobody,
    /// Its place in the last look, and what tells it from the others.
    One(usize, String),
    Several(Vec<Connected>),
}

const NO_PHONE: &str = "No phone in Fastboot Mode answered.";

fn text<'a>(message: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    message.get(field).and_then(Value::as_str)
}

/// A word of the app's, for an error: short, and nothing that could be more
/// than a name.
fn word(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_".contains(*c))
        .take(40)
        .collect()
}

impl<P: Phones> Driver<P> {
    fn screen(&self) {
        if let Some(started) = &self.started {
            say(json!({"type": "screen", "screen": started.flow.screen()}));
        }
    }

    /// Something that was sent did not go through. After `start` every such
    /// answer ends with the flow's screen, as every action does.
    fn refuse(&self, message: &str) {
        say(json!({"type": "error", "message": message}));
        self.screen();
    }

    /// Take the app's lines in turn until `quit` or the end of the input.
    fn run(&mut self) -> Result<(), Fatal> {
        loop {
            // Between two actions, and nowhere else, is where a tool that
            // nobody reads any more leaves
            if DEAF.load(Ordering::SeqCst) {
                return Err(Fatal("nobody reads the protocol any more".into()));
            }
            let message = match self.lines.next() {
                Input::Closed => break,
                Input::TooLong => {
                    self.refuse("That line is longer than a message of this protocol (1 MiB), so nothing was done with it.");
                    continue;
                }
                Input::Line(line) => match parse(&line) {
                    Some(message) => message,
                    None => {
                        self.refuse("That line is not a JSON object, so nothing was done with it.");
                        continue;
                    }
                },
            };
            let Some(kind) = kind(&message) else {
                self.refuse("That message has no \"type\", so nothing was done with it.");
                continue;
            };
            match kind {
                "quit" => break,
                "hello" if !self.greeted && self.started.is_none() => {
                    match message.get("protocol").and_then(Value::as_u64) {
                        Some(PROTOCOL) => self.greeted = true,
                        Some(other) => {
                            return Err(Fatal(format!(
                            "the app speaks protocol {other} and this tool speaks protocol {PROTOCOL}"
                        )))
                        }
                        None => {
                            return Err(Fatal(format!(
                                "the app's hello names no protocol number; this tool speaks protocol {PROTOCOL}"
                            )))
                        }
                    }
                }
                "start" if self.started.is_none() => {
                    if !self.greeted {
                        self.refuse("Say hello first: {\"type\":\"hello\",\"protocol\":1}, then start.");
                        continue;
                    }
                    let Some(reinstall) = message.get("reinstall").and_then(Value::as_bool) else {
                        self.refuse("start needs \"reinstall\": true or false.");
                        continue;
                    };
                    self.start(reinstall)?;
                    self.screen();
                }
                // A cancel that finds no download running is ignored
                "download" if self.started.is_some() && is_cancel(&message) => {}
                _ if self.started.is_none() => self.refuse(
                    "Nothing is taken before start but hello, start and quit, so nothing was done with that.",
                ),
                _ => {
                    if let Err(e) = self.act(kind, &message) {
                        say(json!({"type": "error", "message": e.to_string()}));
                    }
                    self.screen();
                }
            }
        }
        say(json!({"type": "bye"}));
        Ok(())
    }

    /// `start`: read the release and begin the flow. A release that can't be
    /// read at all is the tool's failure; one that fails its checks is the
    /// flow's first screen, a stop.
    fn start(&mut self, reinstall: bool) -> Result<(), Fatal> {
        let Some(dir) = self.release.clone() else {
            return Err(Fatal(
                "no release was named. This version can't find or fetch a release by itself: it is started as 'omamobile-install json --release DIR'".into(),
            ));
        };
        let (bundle, trusted) = (self.trust)(&dir)
            .map_err(|e| Fatal(format!("the release could not be read ({e})")))?;
        // The flow's write switch in this mode: the tool's own argument,
        // and nothing a message says
        let options = FlowOptions {
            reinstall,
            write: self.may_write,
            ..Default::default()
        };
        let flow = Flow::begin(&bundle.files(), bundle.index.as_deref(), &trusted, options);
        self.started = Some(Started { flow, dir });
        Ok(())
    }

    /// The phone an action is for: the one named, else the one the flow
    /// looked at before, else the only one there is.
    fn find(&mut self, named: Option<&str>) -> Res<Found> {
        let seen = self.phones.look()?;
        for (n, phone) in seen.iter().enumerate() {
            if seen[..n].iter().any(|p| p.identity == phone.identity) {
                return Err("Two of the phones in Fastboot Mode can't be told apart, so the installer looks at neither. Connect one phone only.".into());
            }
            if !self.listed.contains(&phone.identity) {
                self.listed.push(phone.identity.clone());
            }
        }
        let at = |identity: &String| seen.iter().position(|p| p.identity == *identity);
        if let Some(id) = named {
            let known = id
                .parse::<usize>()
                .ok()
                .and_then(|n| self.listed.get(n.checked_sub(1)?));
            let Some(identity) = known else {
                return Err(format!(
                    "There is no phone \"{}\" among the ones this run listed.",
                    word(id)
                )
                .into());
            };
            return match at(identity) {
                Some(n) => Ok(Found::One(n, identity.clone())),
                None => Err("That phone is no longer connected in Fastboot Mode.".into()),
            };
        }
        Ok(match self.chosen.as_ref().and_then(at) {
            Some(n) => Found::One(n, seen[n].identity.clone()),
            None if seen.is_empty() => Found::Nobody,
            None if seen.len() == 1 => Found::One(0, seen[0].identity.clone()),
            None => Found::Several(seen),
        })
    }

    /// One action. Refused, with nothing done, unless the flow's screen
    /// names it. An error is said as `error`; the screen follows either way.
    fn act(&mut self, kind: &str, message: &Map<String, Value>) -> Res<()> {
        let Some(started) = self.started.as_ref() else {
            return Err("the install has not started".into());
        };
        let asks = started.flow.screen().action;
        let fits = match kind {
            "acknowledge" | "decline" => asks == "acknowledge",
            "read-phone" => asks == "read-phone",
            "accept-terms" | "decline-terms" => asks == "accept-terms",
            "download" => asks == "download",
            "take-file" => asks == "download" || asks == "take-file",
            "answers" => asks == "answers",
            "dry-run" => asks == "dry-run",
            "confirm" => asks == "confirm",
            "write" => asks == "write",
            #[cfg(feature = "fake-phone")]
            "pretend-panic" => true,
            "hello" | "start" => {
                return Err(
                    format!("The install has started: \"{kind}\" is said once, before it.").into(),
                )
            }
            other => {
                return Err(format!(
                    "This tool knows no message \"{}\" (protocol {PROTOCOL}).",
                    word(other)
                )
                .into())
            }
        };
        if !fits {
            return Err(if asks == "none" {
                format!("The install has ended, so \"{kind}\" is not taken. A new run starts over.")
            } else {
                format!("\"{kind}\" is not what this step takes: the screen asks for \"{asks}\".")
            }
            .into());
        }
        match kind {
            "acknowledge" => self.flow().acknowledge_warning()?,
            "decline" => self.flow().decline_warning()?,
            "read-phone" => {
                let named = match message.get("phone") {
                    None => None,
                    Some(Value::String(id)) => Some(id.as_str()),
                    Some(_) => return Err("\"phone\" is the id of a phone, as text.".into()),
                };
                match self.find(named)? {
                    Found::Nobody => self.flow().no_phone()?,
                    Found::Several(seen) => {
                        let phones: Vec<Value> = seen
                            .iter()
                            .map(|p| {
                                let id = self.listed.iter().position(|i| *i == p.identity);
                                json!({"id": (id.unwrap_or(0) + 1).to_string(), "label": p.label})
                            })
                            .collect();
                        say(json!({
                            "type": "phones",
                            "title": "More than one phone in Fastboot Mode is connected. Which one is this install for?",
                            "phones": phones,
                        }));
                    }
                    Found::One(n, identity) => {
                        let (mut link, id) = self.phones.open(n)?;
                        block_on(self.flow().read_phone(&mut link, &id))?;
                        // The flow has looked at it: later looks and the dry
                        // run go to the same phone without asking again
                        self.chosen = Some(identity);
                    }
                }
            }
            "accept-terms" => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                self.flow().accept_google_terms(now)?;
            }
            "decline-terms" => self.flow().decline_google_terms()?,
            "download" => {
                if text(message, "do") != Some("start") {
                    return Err("download takes \"do\": \"start\" or \"cancel\".".into());
                }
                let Some(started) = self.started.as_mut() else {
                    return Ok(());
                };
                let name = started.flow.screen().file.map(|f| f.name);
                download_factory_zip(
                    &mut started.flow,
                    &self.fetcher,
                    self.proxied,
                    self.work_dir.as_deref(),
                    &name.ok_or("the flow names no file to download")?,
                    &mut Wire::new(&mut self.lines),
                )?;
            }
            "take-file" => {
                let path = Path::new(text(message, "path").ok_or("take-file needs \"path\".")?);
                if !path.is_absolute() {
                    return Err("take-file needs an absolute path.".into());
                }
                let Some(started) = self.started.as_mut() else {
                    return Ok(());
                };
                if !take_factory_zip(&mut started.flow, path, &mut Wire::new(&mut self.lines))? {
                    return Err(format!("There is no file {}", path.display()).into());
                }
            }
            "answers" => {
                let field = |name: &str| {
                    text(message, name).ok_or_else(|| {
                        format!("answers needs all seven fields as text, and \"{name}\" is missing or is not text.")
                    })
                };
                let input = AnswersInput {
                    username: field("username")?,
                    password: field("password")?,
                    password_again: field("password_again")?,
                    hostname: field("hostname")?,
                    timezone: field("timezone")?,
                    wifi_ssid: field("wifi_ssid")?,
                    wifi_psk: field("wifi_psk")?,
                };
                let zones = Path::new("/usr/share/zoneinfo");
                let problems = match OwnerAnswers::check(&input) {
                    // The core checks the zone's shape; this computer knows the zones
                    Ok(_) if zones.is_dir() && !zones.join(input.timezone).is_file() => {
                        vec![AnswerProblem {
                            field: AnswerField::Timezone,
                            message: "That isn't a time zone this computer knows (they look like Europe/Paris).".into(),
                        }]
                    }
                    Ok(answers) => {
                        self.flow().answers(&answers)?;
                        return Ok(());
                    }
                    Err(problems) => problems,
                };
                say(json!({"type": "problems", "problems": problems}));
            }
            "dry-run" => {
                let n = match self.find(None)? {
                    Found::One(n, _) => n,
                    Found::Nobody => return Err(NO_PHONE.into()),
                    Found::Several(_) => {
                        return Err("More than one phone in Fastboot Mode is connected, and none of them is the one this install looked at. Connect that phone only.".into())
                    }
                };
                let (link, _) = self.phones.open(n)?;
                let Some(started) = self.started.as_mut() else {
                    return Ok(());
                };
                let release = started
                    .flow
                    .release()
                    .ok_or("the flow has no release")?
                    .clone();
                if let Some(mut session) = block_on(started.flow.dry_run(link))? {
                    let mut wire = Wire::new(&mut self.lines);
                    match block_on(walk_images(&mut session, &release, &started.dir, &mut wire)) {
                        Ok(_) => {
                            started.flow.dry_run_done(session)?;
                        }
                        Err(e) => started.flow.dry_run_failed(&e)?,
                    }
                }
            }
            // What will be written was listed: this is what the owner typed,
            // as typed. The core takes the phone's codename, or stops
            "confirm" => {
                let typed = text(message, "typed").ok_or("confirm needs \"typed\", as text.")?;
                self.flow().confirm_install(typed)?;
            }
            "write" => {
                let n = match self.find(None)? {
                    Found::One(n, _) => n,
                    Found::Nobody => return Err(NO_PHONE.into()),
                    Found::Several(_) => {
                        return Err("More than one phone in Fastboot Mode is connected, and none of them is the one this install looked at. Connect that phone only.".into())
                    }
                };
                let (link, _) = self.phones.open(n)?;
                let Some(started) = self.started.as_mut() else {
                    return Ok(());
                };
                let release = started
                    .flow
                    .release()
                    .ok_or("the flow has no release")?
                    .clone();
                // The one session that can write is the flow's
                if let Some(mut session) = block_on(started.flow.begin_write(link))? {
                    let mut wire = Wire::new(&mut self.lines);
                    match block_on(walk_images(&mut session, &release, &started.dir, &mut wire)) {
                        Ok(true) => {
                            started.flow.write_done(session)?;
                        }
                        Ok(false) => {
                            let gone = Error::Transport(
                                "the release's files are no longer where the dry run read them"
                                    .into(),
                            );
                            started.flow.write_failed(session, &gone)?;
                        }
                        Err(e) => {
                            started.flow.write_failed(session, &e)?;
                        }
                    }
                }
            }
            // A build of this mode for the tests only: the way they see
            // what a failure of the tool itself looks like on the wire
            #[cfg(feature = "fake-phone")]
            "pretend-panic" => panic!("the tests asked for a panic"),
            _ => {}
        }
        Ok(())
    }

    fn flow(&mut self) -> &mut Flow {
        &mut self
            .started
            .as_mut()
            .expect("an action is taken only once the install has started")
            .flow
    }
}

fn drive<P: Phones>(phones: P, trust: Trust, fetcher: Fetcher, args: Args) -> (P, ExitCode) {
    // At once, before a release is read or anything is looked at
    say(json!({
        "type": "hello",
        "protocol": PROTOCOL,
        "tool": "omamobile-install",
        "version": env!("CARGO_PKG_VERSION"),
        // Whether this run's flow may write: what the tool was started with
        "writes": args.write,
    }));
    let mut driver = Driver {
        phones,
        trust,
        release: args.release,
        work_dir: args.work_dir,
        fetcher,
        proxied: args.proxy.is_some(),
        may_write: args.write,
        lines: Lines::read(),
        greeted: false,
        started: None,
        listed: Vec::new(),
        chosen: None,
    };
    let code = match driver.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fatal(message)) => fatal(&message),
    };
    (driver.phones, code)
}

/// `omamobile-install json ARGS`.
pub fn run(args: &[&str]) -> ExitCode {
    if let Err(e) = claim() {
        eprintln!("error: standard output could not be set up for the protocol: {e}");
        return ExitCode::FAILURE;
    }
    // A panic anywhere is the tool's failure: said on the wire, in a line
    // of its own, and the process ends with a status other than 0
    std::panic::set_hook(Box::new(|info| {
        eprintln!("omamobile-install: {info}");
        let at = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        say(json!({
            "type": "fatal",
            "message": format!("the installer's tool failed unexpectedly (a bug in it{at}); nothing more comes from this run"),
        }));
        std::process::exit(1);
    }));
    let args = match Args::parse(args) {
        Ok(args) => args,
        Err(e) => return fatal(&e),
    };
    #[cfg(feature = "fake-phone")]
    if let Some(states) = args.fake_phone.clone() {
        let (phones, fetcher) = match (
            Pretend::new(&states, args.fake_unplug.clone()),
            Fetcher::pretend(args.fake_google.as_deref(), args.proxy.clone()),
        ) {
            (Ok(phones), Ok(fetcher)) => (phones, fetcher),
            (Err(e), _) | (_, Err(e)) => return fatal(&e.to_string()),
        };
        eprintln!("Pretend phones ({states}), held in memory. No USB device is opened.");
        let (phones, code) = drive(phones, test_key, fetcher, args);
        phones.record();
        return code;
    }
    let fetcher = Fetcher::google(args.proxy.clone());
    drive(Usb::default(), release_keys, fetcher, args).1
}

#[cfg(test)]
mod tests {
    use super::Args;

    /// The mode's arguments are a closed list. Checked here, on the parser,
    /// in every build: nothing is started and no device is looked for.
    #[test]
    fn json_takes_no_argument_that_names_a_phone_states_a_slot_or_skips_a_check() {
        assert!(Args::parse(&[]).is_ok());
        assert!(Args::parse(&["--release", "DIR", "--work-dir", "DIR"]).is_ok());
        // The flow writes only when the tool was started with --write
        assert!(!Args::parse(&["--release", "DIR"]).unwrap().write);
        assert!(Args::parse(&["--release", "DIR", "--write"]).unwrap().write);
        assert!(Args::parse(&["--proxy", "http://127.0.0.1:8080"]).is_ok());
        assert!(Args::parse(&["--proxy", "socks5://127.0.0.1:8080"]).is_err());
        for refused in [
            "--flash",
            "--install",
            "--yes",
            "--no-dry-run",
            "--dry-run",
            "--erase",
            "--reinstall",
            "--serial",
            "--factory-zip",
            "--maintainer-skip-bootloader-slot-check",
            "--bootloader-slots-match",
            "write",
            "DIR",
        ] {
            for args in [
                vec![refused],
                vec!["--release", "DIR", refused],
                vec![refused, "--release", "DIR"],
            ] {
                let why = Args::parse(&args).expect_err(refused);
                assert!(why.contains("no argument that names a phone"), "{why}");
            }
        }
        assert!(Args::parse(&["--release"]).is_err());
    }

    /// A build that ships has no pretend phone and no stand-in for Google.
    #[cfg(not(feature = "fake-phone"))]
    #[test]
    fn a_shipping_build_has_no_pretend_phone_in_this_mode() {
        for args in [
            vec!["--fake-phone", "ready"],
            vec!["--release", "DIR", "--fake-phone", "ready"],
            vec!["--fake-google", "127.0.0.1:1"],
            vec!["--fake-unplug", "flash:userdata:1"],
        ] {
            assert!(Args::parse(&args).is_err(), "{args:?}");
        }
    }

    #[cfg(feature = "fake-phone")]
    #[test]
    fn the_stand_in_for_google_is_for_the_pretend_phone_only() {
        assert!(Args::parse(&["--fake-google", "127.0.0.1:1"]).is_err());
        assert!(Args::parse(&["--fake-unplug", "flash:userdata:1"]).is_err());
        assert!(Args::parse(&["--fake-phone", "ready", "--fake-unplug", "flash"]).is_err());
        let args = Args::parse(&["--fake-phone", "ready", "--fake-unplug", "flash:userdata:2"]);
        assert_eq!(
            args.unwrap().fake_unplug,
            Some(("flash:userdata".into(), 2))
        );
        assert!(Args::parse(&["--fake-phone", "ready", "--fake-google", "127.0.0.1:1"]).is_ok());
    }
}
