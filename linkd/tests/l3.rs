// SPDX-License-Identifier: MIT
//! Stage L3 (INSTALL-LINK-PLAN §11.1, docs/LINKD.md §5): the daemon on real
//! FunctionFS, driven by a host over USB, on the kernel's virtual USB
//! controller (`dummy_hcd`). `tools/l3-run-as-root` does the root part (the
//! modules, the gadget, the broker that runs `omamobile-link-gadget` as
//! root) and runs these as the invoking user.
//!
//! - `LINKD_L3=usb`: the real thing. The host opens only a device whose
//!   sysfs path is under a `dummy_hcd` platform device: nothing here can
//!   reach a real phone or any other USB device.
//! - `LINKD_L3=socket`: the same scenarios against the stage-L2 socket pair,
//!   without root, with shorter time limits: a check of these tests' own
//!   logic. What only USB shows is skipped there.
//! - unset (`cargo test`, CI): every test says it is skipped and passes.
//!
//! Each test prints `L3 ok - NAME (SPEED): WHAT WAS MEASURED` lines, which
//! the script's log keeps; docs/LINKD.md §5 says what each means.
mod common;

use common::{Peer, Phone, SERIAL};
use omamobile_link::digest::{sha256, Hasher};
use omamobile_link::frame::{self, Kind};
use omamobile_link::host::{
    Command as HostCommand, Event, Host, HostConfig, HostEffect, HostInput, HostOutcome, HostOutput,
};
use omamobile_link::msg::{Code, FactoryBegin, HostFrame, HostHello, HostMsg, PhoneFrame, Token};
use omamobile_link::sim::TestRng;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- Where the test runs ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Usb,
    Socket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Speed {
    Full,
    High,
    Super,
}

impl Speed {
    fn packet(self) -> usize {
        match self {
            Speed::Full => 64,
            Speed::High => 512,
            Speed::Super => 1024,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Speed::Full => "full",
            Speed::High => "high",
            Speed::Super => "super",
        }
    }
}

#[derive(Debug, Clone)]
struct Env {
    mode: Mode,
    speed: Speed,
    /// The broker's directory: `ask VERB ROOT` runs as root there.
    broker: PathBuf,
    /// The test gadget's configfs directory (its `UDC` is read here).
    gadget: PathBuf,
    /// The streamed file's length; 0: not run.
    big: u64,
    /// The daemon's time for a write (the plan's 30 s on USB).
    write_ms: u64,
    /// The host's `max_data`: the phone's own (1 MiB) on USB; on the socket
    /// pair less than a local datagram's limit.
    max_data: u32,
}

impl Env {
    fn get(test: &str) -> Option<Env> {
        let mode = match std::env::var("LINKD_L3").as_deref() {
            Ok("usb") => Mode::Usb,
            Ok("socket") => Mode::Socket,
            _ => {
                eprintln!("{test}: skipped: LINKD_L3 is not set (tools/l3-run-as-root sets it)");
                return None;
            }
        };
        let speed = match std::env::var("L3_SPEED").as_deref() {
            Ok("full") => Speed::Full,
            Ok("super") => Speed::Super,
            _ => Speed::High,
        };
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from).unwrap_or_default();
        let big = std::env::var("L3_BIG_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(if mode == Mode::Usb {
                (4 << 30) + 1000
            } else {
                0
            });
        let env = Env {
            mode,
            speed,
            broker: var("L3_BROKER"),
            gadget: var("L3_GADGET"),
            big,
            write_ms: if mode == Mode::Usb { 30_000 } else { 2_000 },
            max_data: if mode == Mode::Usb { 1 << 20 } else { 64 << 10 },
        };
        if mode == Mode::Usb {
            assert!(
                env.broker.join("ask").exists() && env.gadget.join("UDC").exists(),
                "LINKD_L3=usb needs L3_BROKER and L3_GADGET (tools/l3-run-as-root)"
            );
        }
        Some(env)
    }

    fn usb(&self) -> bool {
        self.mode == Mode::Usb
    }

    /// One line for the log.
    fn ok(&self, test: &str, what: &str) {
        let speed = if self.usb() {
            self.speed.name()
        } else {
            "socket"
        };
        println!("L3 ok - {test} ({speed}): {what}");
    }

    /// `ask VERB ROOT` as root, through the broker: its exit status.
    fn ask(&self, verb: &str, root: &Path) -> i32 {
        Command::new(self.broker.join("ask"))
            .arg(verb)
            .arg(root)
            .status()
            .map(|s| s.code().unwrap_or(-1))
            .unwrap_or(-1)
    }

    fn udc(&self) -> String {
        fs::read_to_string(self.gadget.join("UDC"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

fn wait_for(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// --- The scratch root and the daemon ---------------------------------------

/// The scratch root's `omamobile-link-gadget`: the real script, run as root
/// by the broker with this root's paths (tools/l3-run-as-root).
const GADGET_VIA_BROKER: &str = "#!/bin/bash
echo \"$1\" >> \"$OMAMOBILE_LINK_ROOT/fake-gadget.log\"
exec \"$L3_BROKER/ask\" \"$1\" \"$OMAMOBILE_LINK_ROOT\"
";

fn phone(env: &Env) -> Phone {
    let p = Phone::new();
    if env.usb() {
        let path = p.path("/usr/lib/omamobile-install-link/omamobile-link-gadget");
        fs::write(&path, GADGET_VIA_BROKER).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

/// Make the stand-in partition `efs` `len` bytes long: a sparse file with a
/// pattern at its ends, so that a mix-up of offsets shows in the digest.
fn big_partition(p: &Phone, len: u64) -> [u8; 32] {
    let path = p.path("/dev/sda1");
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(len).unwrap();
    use std::os::unix::fs::FileExt;
    let mut rng = TestRng(5);
    for at in [0, len / 2, len - 4096] {
        f.write_all_at(&rng.bytes(4096), at).unwrap();
    }
    p.write(
        "/sys/class/block/sda1/size",
        format!("{}\n", len / 512).as_bytes(),
    );
    let mut h = Hasher::new();
    let mut file = fs::File::open(&path).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    h.finish()
}

/// A running `omamobile-linkd` on FunctionFS (or on a socket pair).
struct Linkd {
    child: Child,
    env: Env,
    root: PathBuf,
    journal: PathBuf,
    /// The gadget is put back when this ends (as the unit's
    /// `ExecStopPost=` would): not for a daemon the test kills to see what
    /// the next start does.
    put_back: bool,
}

impl Drop for Linkd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.env.usb() && self.put_back {
            self.env.ask("remove", &self.root);
        }
    }
}

impl Linkd {
    /// Start the daemon; for a socket pair, also this end of it.
    fn start(env: &Env, p: &Phone, extra: &[&str]) -> (Linkd, Option<UnixStream>) {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p.path("/linkd.log"))
            .unwrap();
        let mut c = Command::new(env!("CARGO_BIN_EXE_omamobile-linkd"));
        c.arg("--root")
            .arg(&p.dir)
            .args([
                "--clock-scale",
                "1",
                "--poll-ms",
                "5",
                "--working-space",
                "0",
            ])
            .args(["--write-timeout-ms", &env.write_ms.to_string()])
            .args(extra.iter().filter(|a| **a != "--l3-small-sndbuf"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));
        let mine = if env.usb() {
            c.args(["--transport", "ffs"]).stdin(Stdio::null());
            None
        } else {
            let (mine, theirs) = rustix::net::socketpair(
                rustix::net::AddressFamily::UNIX,
                rustix::net::SocketType::SEQPACKET,
                rustix::net::SocketFlags::CLOEXEC,
                None,
            )
            .unwrap();
            if extra.contains(&"--l3-small-sndbuf") {
                // A local socket takes many small frames where a bulk IN
                // endpoint holds one: make the daemon's end take few
                rustix::net::sockopt::set_socket_send_buffer_size(&theirs, 1).unwrap();
            }
            c.args(["--transport", "stdio"]).stdin(Stdio::from(theirs));
            Some(UnixStream::from(mine))
        };
        let child = c.spawn().unwrap();
        (
            Linkd {
                child,
                env: env.clone(),
                root: p.dir.clone(),
                journal: p.path("/linkd.log"),
                put_back: true,
            },
            mine,
        )
    }
    fn journal(&self) -> String {
        fs::read_to_string(&self.journal).unwrap_or_default()
    }
    fn says(&self, what: &str) -> usize {
        self.journal().matches(what).count()
    }
    fn exited(&mut self) -> Option<i32> {
        self.child
            .try_wait()
            .unwrap()
            .map(|s| s.code().unwrap_or(-1))
    }
    /// SIGKILL, with the function bound: nothing of the daemon's runs.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.put_back = false;
    }
    fn screen(&self) -> Option<Peer> {
        let path = self.root.join("run/omamobile-install-link/screen");
        let mut peer = None;
        wait_for(Duration::from_secs(10), || {
            peer = Peer::connect(&path);
            peer.is_some()
        });
        peer
    }
}

// --- The two wires ----------------------------------------------------------

/// The host's end of the link: one transfer per send, one completed read per
/// frame, and a read always pending unless paused.
trait Wire {
    fn send(&mut self, frame: &[u8], limit: Duration) -> Result<(), String>;
    fn recv(&mut self, wait: Duration) -> Option<Result<Vec<u8>, String>>;
    /// Paused: no new read is asked for (one that is pending may still
    /// complete).
    fn pause(&self, paused: bool) {
        self.pause_for(if paused {
            Duration::from_secs(1 << 30)
        } else {
            Duration::ZERO
        });
    }
    /// Paused for a while, then reading again by itself.
    fn pause_for(&self, d: Duration);
}

/// Until when the reader is paused.
#[derive(Default)]
struct Pause(std::sync::Mutex<Option<Instant>>);

impl Pause {
    fn set(&self, d: Duration) {
        *self.0.lock().unwrap() = (!d.is_zero()).then(|| Instant::now() + d);
    }
    fn on(&self) -> bool {
        let mut p = self.0.lock().unwrap();
        match *p {
            Some(t) if Instant::now() < t => true,
            Some(_) => {
                *p = None;
                false
            }
            None => false,
        }
    }
}

/// The link's interface on the virtual bus, claimed.
struct Usb {
    dev: nusb::Device,
    iface_no: u8,
    out: nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>,
    packet: usize,
    rx: Receiver<Result<Vec<u8>, String>>,
    paused: Arc<Pause>,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
    _iface: nusb::Interface,
}

impl Drop for Usb {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

/// On the virtual bus: the device's sysfs path is under `dummy_hcd`.
fn on_dummy_bus(info: &nusb::DeviceInfo) -> bool {
    fs::canonicalize(info.sysfs_path())
        .map(|p| p.to_string_lossy().contains("/dummy_hcd."))
        .unwrap_or(false)
}

/// The devices on the virtual bus that are the test gadget.
fn gadgets() -> Vec<nusb::DeviceInfo> {
    use nusb::MaybeFuture;
    nusb::list_devices()
        .wait()
        .map(|l| {
            l.filter(|d| on_dummy_bus(d) && d.vendor_id() == 0x1d6b && d.product_id() == 0x0104)
                .collect()
        })
        .unwrap_or_default()
}

fn link_interface(info: &nusb::DeviceInfo) -> Option<u8> {
    info.interfaces()
        .find(|i| {
            (i.class(), i.subclass(), i.protocol())
                == (
                    omamobile_linkd::ffs::CLASS,
                    omamobile_linkd::ffs::SUBCLASS,
                    omamobile_linkd::ffs::PROTOCOL,
                )
        })
        .map(|i| i.interface_number())
}

impl Usb {
    /// Wait for the link's interface on the virtual bus, open and claim it.
    fn open(limit: Duration) -> Result<Usb, String> {
        use nusb::MaybeFuture;
        let end = Instant::now() + limit;
        let mut last = String::from("no device with the link's interface on the virtual bus");
        loop {
            if let Some(info) = gadgets().into_iter().find(|d| link_interface(d).is_some()) {
                let iface_no = link_interface(&info).unwrap();
                // (udev hands the node to the user a moment after it appears)
                match info
                    .open()
                    .wait()
                    .and_then(|dev| dev.claim_interface(iface_no).wait().map(|i| (dev, i)))
                {
                    Ok((dev, iface)) => return Usb::with(dev, iface, iface_no),
                    Err(e) => last = format!("open or claim: {e}"),
                }
            }
            if Instant::now() >= end {
                return Err(last);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn with(dev: nusb::Device, iface: nusb::Interface, iface_no: u8) -> Result<Usb, String> {
        use nusb::descriptors::TransferType;
        use nusb::transfer::{Bulk, Direction, In, Out};
        let alt = iface
            .descriptor()
            .ok_or("the interface has no descriptor")?;
        let addr = |dir: Direction| {
            alt.endpoints()
                .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir)
                .map(|e| (e.address(), e.max_packet_size()))
        };
        let (out_addr, packet) = addr(Direction::Out).ok_or("no bulk OUT endpoint")?;
        let (in_addr, _) = addr(Direction::In).ok_or("no bulk IN endpoint")?;
        let out = iface
            .endpoint::<Bulk, Out>(out_addr)
            .map_err(|e| e.to_string())?;
        let mut inp = iface
            .endpoint::<Bulk, In>(in_addr)
            .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let paused = Arc::new(Pause::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (paused.clone(), stop.clone());
        // One read always pending: a frame is at most a megabyte and a
        // message; the request is a multiple of every packet size
        let len = frame::read_buffer_len(1 << 20);
        let reader = std::thread::spawn(move || loop {
            if s.load(Ordering::SeqCst) {
                inp.cancel_all();
                while inp.pending() > 0 {
                    let _ = inp.wait_next_complete(Duration::from_millis(200));
                }
                return;
            }
            if inp.pending() == 0 && !p.on() {
                let buf = inp.allocate(len);
                inp.submit(buf);
            }
            if inp.pending() == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let Some(c) = inp.wait_next_complete(Duration::from_millis(50)) else {
                continue;
            };
            let got = c.buffer[..c.actual_len].to_vec();
            match c.status {
                Ok(()) => {
                    if tx.send(Ok(got)).is_err() {
                        return;
                    }
                }
                Err(nusb::transfer::TransferError::Disconnected) => {
                    let _ = tx.send(Err("disconnected".into()));
                    return;
                }
                Err(e) => {
                    if !got.is_empty() {
                        let _ = tx.send(Ok(got));
                    }
                    let _ = tx.send(Err(format!("{e:?}")));
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        Ok(Usb {
            dev,
            iface_no,
            out,
            packet,
            rx,
            paused,
            stop,
            reader: Some(reader),
            _iface: iface,
        })
    }
}

impl Wire for Usb {
    fn send(&mut self, frame: &[u8], limit: Duration) -> Result<(), String> {
        self.out.submit(frame.to_vec().into());
        match self.out.wait_next_complete(limit) {
            Some(c) => c.status.map_err(|e| format!("{e:?}")),
            None => {
                // §5.7, item 5: a write not taken in its time is cancelled
                self.out.cancel_all();
                let _ = self.out.wait_next_complete(Duration::from_secs(5));
                Err("timed out".into())
            }
        }
    }
    fn recv(&mut self, wait: Duration) -> Option<Result<Vec<u8>, String>> {
        match self.rx.recv_timeout(wait) {
            Ok(r) => Some(r),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err("reader gone".into())),
        }
    }
    fn pause_for(&self, d: Duration) {
        self.paused.set(d);
    }
}

/// The stage-L2 socket pair, as a wire: for checking these tests' logic.
struct Sock {
    sock: UnixStream,
    rx: Receiver<Result<Vec<u8>, String>>,
    paused: Arc<Pause>,
}

impl Sock {
    fn new(sock: UnixStream) -> Sock {
        let reader = sock.try_clone().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let paused = Arc::new(Pause::default());
        let p = paused.clone();
        std::thread::spawn(move || {
            let mut r = reader;
            let mut buf = vec![0u8; frame::read_buffer_len(1 << 20)];
            loop {
                if p.on() {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                match r.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(Err("disconnected".into()));
                        return;
                    }
                    Ok(n) => {
                        if tx.send(Ok(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
        });
        Sock { sock, rx, paused }
    }
}

impl Wire for Sock {
    fn send(&mut self, frame: &[u8], limit: Duration) -> Result<(), String> {
        self.sock.set_write_timeout(Some(limit)).unwrap();
        self.sock
            .write(frame)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn recv(&mut self, wait: Duration) -> Option<Result<Vec<u8>, String>> {
        match self.rx.recv_timeout(wait) {
            Ok(r) => Some(r),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err("reader gone".into())),
        }
    }
    fn pause_for(&self, d: Duration) {
        self.paused.set(d);
    }
}

/// Open the wire to a daemon just started.
fn wire(env: &Env, sock: Option<UnixStream>) -> Box<dyn Wire> {
    match sock {
        Some(s) => {
            let mut w = Sock::new(s);
            // The stand-in's ENABLE
            w.send(&[1], Duration::from_secs(1)).unwrap();
            Box::new(w)
        }
        None => {
            assert!(env.usb());
            Box::new(Usb::open(Duration::from_secs(30)).expect("the link on the virtual bus"))
        }
    }
}

// --- The installer's shell --------------------------------------------------

/// What the factory image is: bytes in memory, or a pattern made as it is
/// read (a 4 GiB file costs no disk on this side).
#[derive(Clone)]
enum Source {
    Bytes(Vec<u8>),
    /// Each 512-byte block starts with its own number and the seed, then
    /// zeros: a frame sent twice, dropped or out of order changes the
    /// digest, and the phone's copy compresses to nearly nothing.
    Pattern {
        len: u64,
        seed: u64,
    },
}

impl Source {
    fn len(&self) -> u64 {
        match self {
            Source::Bytes(b) => b.len() as u64,
            Source::Pattern { len, .. } => *len,
        }
    }
    fn read(&self, offset: u64, len: u32) -> Option<Vec<u8>> {
        let end = offset.checked_add(len as u64)?;
        if end > self.len() {
            return None;
        }
        Some(match self {
            Source::Bytes(b) => b[offset as usize..end as usize].to_vec(),
            Source::Pattern { seed, .. } => {
                let mut out = vec![0u8; len as usize];
                let mut block = offset / 512;
                loop {
                    let start = block * 512;
                    if start >= end {
                        break;
                    }
                    let mut head = [0u8; 16];
                    head[..8].copy_from_slice(&block.to_le_bytes());
                    head[8..].copy_from_slice(&seed.to_le_bytes());
                    for (i, b) in head.iter().enumerate() {
                        let at = start + i as u64;
                        if at >= offset && at < end {
                            out[(at - offset) as usize] = *b;
                        }
                    }
                    block += 1;
                }
                out
            }
        })
    }
    fn sha256(&self) -> [u8; 32] {
        let mut h = Hasher::new();
        let mut at = 0;
        while at < self.len() {
            let n = (self.len() - at).min(8 << 20) as u32;
            h.update(&self.read(at, n).unwrap());
            at += n as u64;
        }
        h.finish()
    }
    fn begin(&self) -> FactoryBegin {
        FactoryBegin {
            size: self.len(),
            sha256: self.sha256(),
            build: "test.260101.001".into(),
            name: "l3-test-factory.zip".into(),
        }
    }
}

/// The crate's host machine and its shell, as an installer has them.
struct Shell {
    host: Host,
    rng: TestRng,
    backup: BTreeMap<String, Vec<u8>>,
    src: Source,
    token: Option<Token>,
    events: Vec<Event>,
    outbox: VecDeque<Vec<u8>>,
    start: Instant,
    /// Frames written, and how many it took to be paired.
    sent: u64,
    paired_after: Option<u64>,
    /// Completed reads; reads that were not exactly one frame; padded ones.
    reads: u64,
    bad_reads: u64,
    padded: u64,
    bytes_out: u64,
    /// The host's own time for a write.
    write_limit: Duration,
    /// A send that failed, and why.
    send_failed: Option<String>,
}

impl Shell {
    fn new(
        max_data: u32,
        token: Option<Token>,
        src: Source,
        seed: u64,
        write_limit: Duration,
    ) -> Shell {
        let mut cfg = HostConfig::new("omamobile-install-l3", "0.0.1", SERIAL.as_bytes());
        cfg.max_data = max_data;
        cfg.token = token;
        Shell {
            host: Host::new(cfg),
            rng: TestRng(seed),
            backup: BTreeMap::new(),
            src,
            token,
            events: Vec::new(),
            outbox: VecDeque::new(),
            start: Instant::now(),
            sent: 0,
            paired_after: None,
            reads: 0,
            bad_reads: 0,
            padded: 0,
            bytes_out: 0,
            write_limit,
            send_failed: None,
        }
    }
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn feed(&mut self, input: HostInput<'_>) {
        let now = self.now();
        let mut queue: VecDeque<HostOutput> = self.host.handle(now, input, &mut self.rng).into();
        while let Some(o) = queue.pop_front() {
            let answer = match o {
                HostOutput::Send(f) => {
                    self.outbox.push_back(f);
                    None
                }
                HostOutput::Event(e) => {
                    if matches!(e, Event::Paired { .. }) && self.paired_after.is_none() {
                        self.paired_after = Some(self.sent + self.outbox.len() as u64);
                    }
                    self.events.push(e);
                    None
                }
                HostOutput::Do(HostEffect::RecordToken(t)) => {
                    self.token = Some(t);
                    None
                }
                HostOutput::Do(HostEffect::ForgetToken) => {
                    self.token = None;
                    None
                }
                HostOutput::Do(HostEffect::BackupOpen { ticket, name, .. }) => Some((
                    ticket,
                    HostOutcome::Have(self.backup.get(&name).map_or(0, |f| f.len() as u64)),
                )),
                HostOutput::Do(HostEffect::BackupWrite { name, offset, data }) => {
                    let f = self.backup.entry(name).or_default();
                    f.truncate(offset as usize);
                    f.extend_from_slice(&data);
                    None
                }
                HostOutput::Do(HostEffect::BackupDigest { ticket, name }) => Some((
                    ticket,
                    match self.backup.get(&name) {
                        Some(f) => HostOutcome::Digest(sha256(f)),
                        None => HostOutcome::Failed,
                    },
                )),
                HostOutput::Do(HostEffect::BackupDiscard { name }) => {
                    self.backup.remove(&name);
                    None
                }
                HostOutput::Do(HostEffect::BackupFinish { ticket, .. }) => {
                    Some((ticket, HostOutcome::Done))
                }
                HostOutput::Do(HostEffect::FactoryRead {
                    ticket,
                    offset,
                    len,
                }) => Some((
                    ticket,
                    match self.src.read(offset, len) {
                        Some(d) => HostOutcome::Data(d),
                        None => HostOutcome::Failed,
                    },
                )),
            };
            if let Some((ticket, outcome)) = answer {
                let now = self.now();
                queue.extend(self.host.handle(
                    now,
                    HostInput::Done(ticket, outcome),
                    &mut self.rng,
                ));
            }
        }
    }
    fn command(&mut self, cmd: HostCommand) {
        self.feed(HostInput::Command(cmd));
    }
    fn saw(&self, f: impl Fn(&Event) -> bool) -> bool {
        self.events.iter().any(f)
    }

    /// Carry frames both ways and keep the machine's deadlines until `until`
    /// holds or `limit` passes. `on_frame` sees each read before the machine
    /// does. True: `until` held.
    fn pump(
        &mut self,
        wire: &mut dyn Wire,
        limit: Duration,
        mut until: impl FnMut(&Shell) -> bool,
        mut on_frame: impl FnMut(&[u8], &dyn Wire),
    ) -> bool {
        let end = Instant::now() + limit;
        loop {
            if until(self) {
                return true;
            }
            if Instant::now() >= end || self.send_failed.is_some() {
                return false;
            }
            if let Some(f) = self.outbox.pop_front() {
                match wire.send(&f, self.write_limit) {
                    Ok(()) => {
                        self.sent += 1;
                        self.bytes_out += f.len() as u64;
                    }
                    Err(e) => self.send_failed = Some(e),
                }
            }
            let wait = if self.outbox.is_empty() {
                Duration::from_millis(2)
            } else {
                Duration::ZERO
            };
            match wire.recv(wait) {
                Some(Ok(bytes)) => {
                    self.reads += 1;
                    match frame::decode(&bytes, frame::MAX_DATA_LIMIT) {
                        Ok(f) => {
                            if bytes.len() == frame::HEADER_LEN + f.payload.len() + 1 {
                                self.padded += 1;
                            }
                        }
                        Err(_) => self.bad_reads += 1,
                    }
                    on_frame(&bytes, wire);
                    self.feed(HostInput::Frame(&bytes));
                }
                Some(Err(e)) if e == "disconnected" => {
                    self.feed(HostInput::LinkDown);
                    return until(self);
                }
                Some(Err(_)) | None => {}
            }
            if self.host.deadline().is_some_and(|t| t <= self.now()) {
                self.feed(HostInput::Tick);
            }
        }
    }

    /// Connect, and pair: with the screen's code, or with the token.
    fn pair(&mut self, wire: &mut dyn Wire, d: &Linkd) {
        self.feed(HostInput::LinkUp);
        let mut screen = None;
        let ok = self.pump(
            wire,
            Duration::from_secs(30),
            |s| s.host.ready() || s.saw(|e| *e == Event::NeedCode),
            |_, _| {},
        );
        assert!(ok, "neither paired nor asked for a code: {:?}", self.events);
        if !self.host.ready() {
            let s = screen.get_or_insert_with(|| d.screen().expect("the screen socket"));
            let code = s
                .wait(|l| l.starts_with("code "))
                .and_then(|l| Code::parse(&l[5..]))
                .expect("the screen shows a code");
            self.command(HostCommand::Pair(code));
            assert!(
                self.pump(wire, Duration::from_secs(30), |s| s.host.ready(), |_, _| {}),
                "not paired: {:?}",
                self.events
            );
        }
    }

    /// One step, to its end.
    fn step(&mut self, wire: &mut dyn Wire, cmd: HostCommand, limit: Duration) -> bool {
        self.events.clear();
        self.command(cmd);
        self.pump(wire, Duration::from_millis(1), |_| false, |_, _| {});
        self.pump(wire, limit, |s| !s.host.busy(), |_, _| {})
    }
}

/// The text of a message frame, for matching `"t":"..."`.
fn text(bytes: &[u8]) -> String {
    match frame::decode(bytes, frame::MAX_DATA_LIMIT) {
        Ok(f) if f.kind == Kind::Message => String::from_utf8_lossy(f.payload).to_string(),
        _ => String::new(),
    }
}

// --- The tests --------------------------------------------------------------

/// §4: the descriptors as a host reads them, the strings, the OS
/// descriptors, `bcdDevice`; and after `remove`, the gadget as it was.
#[test]
fn l3_01_descriptors() {
    let Some(env) = Env::get("l3_01_descriptors") else {
        return;
    };
    if !env.usb() {
        return eprintln!("l3_01_descriptors: USB only");
    }
    use nusb::MaybeFuture;
    let before: Vec<_> = gadgets();
    assert_eq!(before.len(), 1, "one test gadget on the virtual bus");
    let base_bcd = before[0].device_version();
    assert!(
        link_interface(&before[0]).is_none(),
        "the gadget starts without the link"
    );
    let p = phone(&env);
    let (d, _) = Linkd::start(&env, &p, &[]);
    let usb = Usb::open(Duration::from_secs(30)).expect("the link on the virtual bus");
    let t = Duration::from_secs(2);
    let dd = usb.dev.device_descriptor();
    assert_eq!((dd.vendor_id(), dd.product_id()), (0x1d6b, 0x0104));
    assert_eq!(
        dd.device_version(),
        0x0200,
        "§4: bcdDevice 0x0200 with the link"
    );
    let speed = usb.dev.speed();
    let want = match env.speed {
        Speed::Full => nusb::Speed::Full,
        Speed::High => nusb::Speed::High,
        Speed::Super => nusb::Speed::Super,
    };
    assert_eq!(speed, Some(want), "the bus runs at the speed asked for");
    assert_eq!(
        usb.packet,
        env.speed.packet(),
        "wMaxPacketSize at this speed"
    );
    let cfg = usb.dev.active_configuration().unwrap();
    let mut ours = 0;
    let mut others = Vec::new();
    for alt in cfg.interface_alt_settings() {
        if (alt.class(), alt.subclass(), alt.protocol()) == (0xff, 0x6f, 0x01) {
            ours += 1;
            let eps: Vec<_> = alt
                .endpoints()
                .map(|e| (e.direction(), e.transfer_type(), e.max_packet_size()))
                .collect();
            assert_eq!(eps.len(), 2, "one bulk OUT and one bulk IN: {eps:?}");
            for (_, kind, packet) in eps {
                assert_eq!(kind, nusb::descriptors::TransferType::Bulk);
                assert_eq!(packet, env.speed.packet());
            }
            let s = usb
                .dev
                .get_string_descriptor(alt.string_index().expect("iInterface"), 0x0409, t)
                .wait()
                .unwrap();
            assert_eq!(s, omamobile_linkd::ffs::INTERFACE_STRING);
        } else {
            others.push(alt.interface_number());
        }
    }
    assert_eq!(ours, 1, "the link's interface, once");
    assert!(
        !others.is_empty(),
        "the gadget's other function is still there"
    );
    // SuperSpeed: each endpoint has its companion
    let raw = usb.dev.get_descriptor(2, 0, 0, t).wait().unwrap();
    let companions = raw.windows(2).filter(|w| w == &[6, 0x30]).count();
    if env.speed == Speed::Super {
        assert!(companions >= 2, "SuperSpeed endpoint companions");
    }
    // Microsoft OS 1.0: the string at 0xEE, then the extended compatible ID
    let os = usb.dev.get_descriptor(3, 0xee, 0, t).wait().unwrap();
    let sign: Vec<u8> = "MSFT100"
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    assert_eq!(&os[2..16], &sign[..], "the OS string descriptor");
    let code = os[16];
    let compat = usb
        .dev
        .control_in(
            nusb::transfer::ControlIn {
                control_type: nusb::transfer::ControlType::Vendor,
                recipient: nusb::transfer::Recipient::Device,
                request: code,
                value: 0,
                index: 4,
                length: 4096,
            },
            t,
        )
        .wait()
        .unwrap();
    let winusb = compat[16..]
        .chunks(24)
        .any(|s| s.len() == 24 && s[0] == usb.iface_no && &s[2..10] == b"WINUSB\0\0");
    assert!(
        winusb,
        "the link's interface has the compatible ID WINUSB: {compat:02x?}"
    );
    env.ok(
        "descriptors",
        &format!(
            "1d6b:0104 bcdDevice 0x0200, interface ff/6f/01 \"{}\" #{}, bulk {} bytes{}, OS string vendor code {code:#04x}, WINUSB",
            omamobile_linkd::ffs::INTERFACE_STRING,
            usb.iface_no,
            usb.packet,
            if companions > 0 { ", companions" } else { "" }
        ),
    );
    drop(usb);
    drop(d); // the daemon ends, and `remove` runs, as ExecStopPost= would
    assert!(
        wait_for(Duration::from_secs(10), || gadgets()
            .first()
            .is_some_and(|g| link_interface(g).is_none())),
        "the link left the bus"
    );
    let after = gadgets();
    assert_eq!(after[0].device_version(), base_bcd, "bcdDevice back");
    assert!(
        env.udc().starts_with("dummy_udc."),
        "the gadget is bound again"
    );
    env.ok(
        "remove",
        &format!(
            "the gadget as before: bcdDevice {base_bcd:#06x}, no link interface, bound to {}",
            env.udc()
        ),
    );
}

/// §5.1: one frame per completed read, both ways, with frames of every
/// length around the packet size; the pad rule; and what happens without
/// it (two transfers read as one).
#[test]
fn l3_02_one_frame_per_read_and_the_pad_rule() {
    let Some(env) = Env::get("l3_02_one_frame_per_read_and_the_pad_rule") else {
        return;
    };
    let p = phone(&env);
    let (d, sock) = Linkd::start(&env, &p, &[]);
    let mut w = wire(&env, sock);
    let packet = if env.usb() { env.speed.packet() } else { 512 };
    let limit = Duration::from_secs(10);
    // A session at the frame level: hello, then pings of chosen lengths
    let seq = std::cell::Cell::new(0u32);
    let send = |w: &mut dyn Wire, msg: HostMsg, id: u64, pad_to: Option<usize>| {
        let mut json = HostFrame { id, msg }.to_json().unwrap();
        if let Some(total) = pad_to {
            // white space after the brace makes the frame `total` bytes
            let want = total - frame::HEADER_LEN;
            assert!(want >= json.len());
            json.insert_str(1, &" ".repeat(want - json.len()));
        }
        let f = frame::encode(Kind::Message, seq.get(), 0, json.as_bytes(), 1024).unwrap();
        seq.set(seq.get() + 1);
        w.send(&f, limit).unwrap();
        f.len()
    };
    let recv = |w: &mut dyn Wire| -> Vec<u8> {
        match w.recv(limit) {
            Some(Ok(b)) => b,
            other => panic!("no frame from the phone: {other:?}"),
        }
    };
    send(
        &mut *w,
        HostMsg::Hello(HostHello {
            proto: vec![1],
            installer_name: "l3".into(),
            installer_version: "0".into(),
            max_data: 1024,
            nonce: [7; 32],
        }),
        1,
        None,
    );
    let hello = recv(&mut *w);
    assert!(text(&hello).contains("\"t\":\"hello\""), "{}", text(&hello));
    // Lengths of whole transfers: around one, two and three packets, and
    // around 1024 and 4096 bytes (every packet size's multiples)
    let mut lens: Vec<usize> = Vec::new();
    for k in [1, 2, 3] {
        for dl in [-1i64, 0, 1] {
            lens.push(((k * packet) as i64 + dl) as usize);
        }
    }
    for n in [1023, 1024, 1025, 2048, 4095, 4096, 4097] {
        lens.push(n);
    }
    lens.retain(|n| *n >= frame::HEADER_LEN + 24);
    lens.sort();
    lens.dedup();
    let (mut padded, mut id) = (0, 2u64);
    for total in &lens {
        // A frame of `total` bytes before its pad: the pad makes it one more
        let sent = send(&mut *w, HostMsg::Ping, id, Some(*total));
        if sent == total + 1 {
            padded += 1;
            assert_eq!(total % 64, 0, "a pad only where 64 divides the length");
        }
        let back = recv(&mut *w);
        let t = text(&back);
        assert!(
            t.contains("\"t\":\"pong\""),
            "{total} bytes: one ping, read as one frame: {t}"
        );
        let f = frame::decode(&back, 1024).unwrap();
        assert_eq!(f.seq, (id - 1) as u32, "the phone's frames in order");
        let re = PhoneFrame::parse(f.payload).unwrap().re;
        assert_eq!(re, Some(id));
        id += 1;
    }
    env.ok(
        "one frame per read (OUT)",
        &format!(
            "{} pings of {}..{} bytes, {padded} padded, each one pong, in order, at packet {packet}",
            lens.len(),
            lens[0],
            lens[lens.len() - 1]
        ),
    );
    // Without the pad: a transfer of a whole number of packets has no
    // short packet, so the phone's read goes on into the next transfer
    if env.usb() {
        let mut json = HostFrame {
            id,
            msg: HostMsg::Ping,
        }
        .to_json()
        .unwrap();
        json.insert_str(1, &" ".repeat(2 * packet - frame::HEADER_LEN - json.len()));
        let mut f = frame::encode(Kind::Message, seq.get(), 0, json.as_bytes(), 1024).unwrap();
        f[6] = 0; // no pad flag
        f.pop(); // and no pad byte
        assert_eq!(f.len(), 2 * packet);
        w.send(&f, limit).unwrap();
        seq.set(seq.get() + 1);
        send(&mut *w, HostMsg::Ping, id + 1, None);
        let back = recv(&mut *w);
        let t = text(&back);
        assert!(
            t.contains("\"t\":\"error\"") && t.contains("protocol"),
            "two transfers without a short packet are one read, refused: {t}"
        );
        env.ok(
            "the pad rule is needed",
            &format!(
                "a {}-byte frame without its pad and the next frame were read as one, and refused",
                2 * packet
            ),
        );
    }
    // The other way: data frames of 1992 bytes, 56 + 1992 = 2048, a multiple
    // of 64, 512 and 1024, so each needs its pad; and the factory image the
    // same size of frames back to the phone
    let src = Source::Bytes(TestRng(9).bytes(1992 * 40 + 7));
    let errors_before = d.says("Sent(\"error\")");
    let mut sh = Shell::new(
        1992,
        None,
        src.clone(),
        1,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    assert!(
        sh.step(&mut *w, HostCommand::Backup, Duration::from_secs(60)),
        "{:?}",
        sh.events
    );
    assert!(sh.saw(|e| *e == Event::BackupDone), "{:?}", sh.events);
    assert_eq!(sh.bad_reads, 0, "every read from the phone was one frame");
    assert!(sh.padded > 0);
    let backup_reads = (sh.reads, sh.padded);
    assert!(sh.step(
        &mut *w,
        HostCommand::Factory(src.begin()),
        Duration::from_secs(60)
    ));
    assert!(sh.saw(|e| *e == Event::FactoryVerified), "{:?}", sh.events);
    assert_eq!(sh.bad_reads, 0);
    assert_eq!(
        d.says("Sent(\"error\")"),
        errors_before,
        "no error from the phone: {}",
        d.journal()
    );
    env.ok(
        "one frame per read (IN) and the pad",
        &format!(
            "backup: {} reads from the phone, {} of them 2049-byte padded frames, none merged; factory image of {} bytes in 1992-byte frames verified",
            backup_reads.0, backup_reads.1, src.len()
        ),
    );
}

/// §5.7 (F6) flow (a): a host that writes the whole image and never reads.
/// The daemon's queue fills, it stops reading, its oldest write is not
/// taken for its time and is cancelled (`io_cancel`), the session ends,
/// the file is at its last acknowledgement, and the next `hello` is
/// answered with no unbind.
#[test]
fn l3_03_flow_a_a_host_that_never_reads() {
    let Some(env) = Env::get("l3_03_flow_a_a_host_that_never_reads") else {
        return;
    };
    let mib = 1u64 << 20;
    let len = 40 * mib;
    let src = Source::Pattern { len, seed: 3 };
    let p = phone(&env);
    // A window the whole image fits: the host may send it all at once
    let (d, sock) = Linkd::start(
        &env,
        &p,
        &[
            "--ack-every",
            &mib.to_string(),
            "--window",
            &len.to_string(),
            "--l3-small-sndbuf",
        ],
    );
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        None,
        src.clone(),
        2,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    let token = sh.token.expect("a token");
    let usb_devnum = gadgets().first().map(|g| {
        fs::read_to_string(g.sysfs_path().join("devnum"))
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    // The host reads `factory-ready`, then nothing more
    sh.command(HostCommand::Factory(src.begin()));
    let stopped = std::cell::Cell::new(false);
    sh.pump(
        &mut *w,
        Duration::from_secs(30),
        |_| stopped.get(),
        |b, w| {
            if text(b).contains("factory-ready") {
                w.pause(true);
                stopped.set(true);
            }
        },
    );
    assert!(stopped.get(), "factory-ready: {:?}", sh.events);
    // Send everything; time the longest single write
    let t0 = Instant::now();
    let (mut longest, mut blocked_at) = (Duration::ZERO, None);
    while let Some(f) = sh.outbox.pop_front() {
        let t = Instant::now();
        let r = w.send(&f, sh.write_limit);
        let took = t.elapsed();
        if took > longest {
            longest = took;
            blocked_at = Some(t.duration_since(t0));
        }
        if r.is_err() {
            break;
        }
        sh.bytes_out += f.len() as u64;
        // more frames for the window as the machine hands them out
        if sh.outbox.is_empty() {
            sh.feed(HostInput::Tick);
        }
        if d.says("not taken in time") > 0 && sh.outbox.is_empty() {
            break;
        }
    }
    let stall = d.says("not taken in time");
    assert_eq!(stall, 1, "the daemon gave up one write: {}", d.journal());
    let w_s = env.write_ms as f64 / 1000.0;
    assert!(
        longest.as_secs_f64() >= w_s * 0.9 && longest.as_secs_f64() <= w_s * 1.5 + 5.0,
        "the host was held for about the daemon's {w_s}s: {longest:?}"
    );
    assert_eq!(
        d.says("link: DISABLE") + d.says("link: UNBIND"),
        0,
        "no unbind: {}",
        d.journal()
    );
    if let (Some(before), Some(now)) = (
        usb_devnum,
        gadgets().first().map(|g| {
            fs::read_to_string(g.sysfs_path().join("devnum"))
                .unwrap_or_default()
                .trim()
                .to_string()
        }),
    ) {
        assert_eq!(before, now, "the device was not enumerated again");
    }
    env.ok(
        "flow (a)",
        &format!(
            "the host wrote {} MiB without reading; its writes were held {:.1}s from {:.1}s in; the daemon cancelled its write and ended the session; no DISABLE, no new enumeration",
            sh.bytes_out >> 20,
            longest.as_secs_f64(),
            blocked_at.unwrap_or_default().as_secs_f64()
        ),
    );
    // The host reads again: a new run with the token, `hello`, `resume`,
    // and the transfer goes on from the last acknowledgement
    w.pause(false);
    let mut again = Shell::new(
        env.max_data,
        Some(token),
        src.clone(),
        3,
        Duration::from_millis(env.write_ms * 2),
    );
    again.pair(&mut *w, &d);
    let discarded = again.host.discarded();
    let mut resumed_at = None;
    again.command(HostCommand::Factory(src.begin()));
    assert!(again.pump(
        &mut *w,
        Duration::from_secs(120),
        |s| !s.host.busy(),
        |b, _| {
            let t = text(b);
            if t.contains("factory-ready") && resumed_at.is_none() {
                resumed_at = t
                    .split("\"offset\":")
                    .nth(1)
                    .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|n| n.parse::<u64>().ok());
            }
        }
    ));
    let at = resumed_at.expect("factory-ready's offset");
    assert!(
        at > 0 && at % mib == 0,
        "resumed at an acknowledgement: {at}"
    );
    assert!(at < sh.bytes_out, "and not past what was sent");
    assert!(
        again.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        again.events
    );
    env.ok(
        "flow (a), after",
        &format!(
            "the next hello was answered ({discarded} stale frames passed over); the stream resumed at {} MiB, an acknowledged offset; the image verified",
            at >> 20
        ),
    );
}

/// Flow (b): a host that keeps to the window and reads each `ack` 5 s
/// late finishes the transfer.
#[test]
fn l3_04_flow_b_window_and_late_acks() {
    let Some(env) = Env::get("l3_04_flow_b_window_and_late_acks") else {
        return;
    };
    let mib = 1u64 << 20;
    let src = Source::Pattern {
        len: 4 * mib + 100,
        seed: 4,
    };
    let p = phone(&env);
    let late = if env.usb() { 5 } else { 1 };
    let (d, sock) = Linkd::start(
        &env,
        &p,
        &[
            "--ack-every",
            &mib.to_string(),
            "--window",
            &(2 * mib).to_string(),
        ],
    );
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        None,
        src.clone(),
        4,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    sh.command(HostCommand::Factory(src.begin()));
    let mut acks = 0;
    let t0 = Instant::now();
    let done = sh.pump(
        &mut *w,
        Duration::from_secs(120),
        |s| !s.host.busy(),
        |b, w| {
            if text(b).contains("\"t\":\"ack\"") {
                acks += 1;
                w.pause_for(Duration::from_secs(late));
            }
        },
    );
    assert!(
        done && sh.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        sh.events
    );
    assert_eq!(d.says("not taken in time"), 0, "{}", d.journal());
    env.ok(
        "flow (b)",
        &format!(
            "{acks} acks each read {late}s late, the window of 2 MiB kept, the image verified in {:.1}s, no write given up",
            t0.elapsed().as_secs_f64()
        ),
    );
}

/// Flow (c): `abort` in the middle of a partition with the host's reader
/// stopped for 5 s is answered `aborted`.
#[test]
fn l3_05_flow_c_abort_in_a_partition() {
    let Some(env) = Env::get("l3_05_flow_c_abort_in_a_partition") else {
        return;
    };
    let p = phone(&env);
    big_partition(&p, 64 << 20);
    let (d, sock) = Linkd::start(&env, &p, &[]);
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        None,
        Source::Bytes(Vec::new()),
        5,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    sh.command(HostCommand::Backup);
    let data = std::cell::Cell::new(0);
    let started = sh.pump(
        &mut *w,
        Duration::from_secs(30),
        |_| data.get() >= 4,
        |b, _| {
            if frame::decode(b, frame::MAX_DATA_LIMIT).is_ok_and(|f| f.kind == Kind::Data) {
                data.set(data.get() + 1);
            }
        },
    );
    let data = data.get();
    assert!(started, "the partition's data: {:?}", sh.events);
    let pause = if env.usb() { 5 } else { 1 };
    w.pause(true);
    let t0 = Instant::now();
    sh.command(HostCommand::Abort);
    // The `abort` goes out while the reader is stopped
    sh.pump(&mut *w, Duration::from_secs(pause), |_| false, |_, _| {});
    w.pause(false);
    let ok = sh.pump(
        &mut *w,
        Duration::from_secs(30),
        |s| s.saw(|e| *e == Event::Aborted),
        |_, _| {},
    );
    assert!(ok, "aborted: {:?}", sh.events);
    assert_eq!(d.says("not taken in time"), 0, "{}", d.journal());
    env.ok(
        "flow (c)",
        &format!(
            "abort sent after {data} data frames with the reader stopped {pause}s; aborted {:.1}s after the abort; no write given up",
            t0.elapsed().as_secs_f64()
        ),
    );
}

/// The child side of `l3_06`: one run of an installer, killed by the test
/// (`L3_CHILD=1`), or run to the end (`L3_CHILD_FINISH=1`).
#[test]
fn l3_child() {
    if std::env::var_os("L3_CHILD").is_none() {
        return;
    }
    let env = Env::get("l3_child").unwrap();
    let root = PathBuf::from(std::env::var("L3_CHILD_ROOT").unwrap());
    let log = root.join("host-child.log");
    let say = |line: &str| {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        writeln!(f, "{line}").unwrap();
        let _ = f.sync_all();
    };
    let token = fs::read_to_string(root.join("host-token"))
        .ok()
        .and_then(|h| omamobile_link::digest::from_hex32(h.trim()).map(Token));
    let len: u64 = std::env::var("L3_CHILD_LEN").unwrap().parse().unwrap();
    let src = Source::Pattern { len, seed: 6 };
    let mut w: Box<dyn Wire> = if env.usb() {
        Box::new(Usb::open(Duration::from_secs(30)).unwrap())
    } else {
        // The socket is our standard input
        use std::os::fd::AsFd;
        let fd = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
        Box::new(Sock::new(UnixStream::from(fd)))
    };
    let seed = std::process::id() as u64;
    let mut sh = Shell::new(
        env.max_data,
        token,
        src.clone(),
        seed,
        Duration::from_millis(env.write_ms * 2),
    );
    if std::env::var_os("L3_CHILD_HELLO_ONLY").is_some() {
        // F2: killed between writing `hello` and reading its answer
        sh.feed(HostInput::LinkUp);
        while let Some(f) = sh.outbox.pop_front() {
            let _ = w.send(&f, Duration::from_secs(5));
        }
        w.pause(true);
        say("hello written");
        std::thread::sleep(Duration::from_secs(3600));
    }
    sh.feed(HostInput::LinkUp);
    let mut screen: Option<Peer> = None;
    let ok = sh.pump(
        &mut *w,
        Duration::from_secs(60),
        |s| s.host.ready() || s.saw(|e| *e == Event::NeedCode),
        |_, _| {},
    );
    assert!(ok, "{:?}", sh.events);
    if !sh.host.ready() {
        let s = screen.get_or_insert_with(|| {
            Peer::connect(&root.join("run/omamobile-install-link/screen")).unwrap()
        });
        let code = s
            .wait(|l| l.starts_with("code "))
            .and_then(|l| Code::parse(&l[5..]))
            .unwrap();
        sh.command(HostCommand::Pair(code));
        assert!(sh.pump(
            &mut *w,
            Duration::from_secs(30),
            |s| s.host.ready(),
            |_, _| {}
        ));
    }
    if let Some(t) = &sh.token {
        fs::write(
            root.join("host-token"),
            omamobile_link::digest::to_hex(&t.0),
        )
        .unwrap();
    }
    let wrong = sh.saw(|e| matches!(e, Event::WrongCode { .. } | Event::Locked { .. }));
    say(&format!(
        "paired after={} discarded={} bad_reads={} wrong={wrong}",
        sh.paired_after.unwrap_or(0),
        sh.host.discarded(),
        sh.bad_reads
    ));
    sh.command(HostCommand::Factory(src.begin()));
    let done = sh.pump(
        &mut *w,
        Duration::from_secs(600),
        |s| !s.host.busy(),
        |b, _| {
            let t = text(b);
            if t.contains("\"t\":\"ack\"") {
                say(&format!("ack {t}"));
            }
        },
    );
    say(&format!(
        "ended done={done} verified={}",
        sh.saw(|e| *e == Event::FactoryVerified)
    ));
}

/// Flow (d) and F1, F2: the installer killed (no unbind, the cable left in)
/// in the stream, and between writing `hello` and reading its answer, 50
/// times; each next run pairs within its first three frames with no wrong
/// count, and the last one finishes the image from where it was.
#[test]
fn l3_06_flow_d_a_killed_host_resumes() {
    let Some(env) = Env::get("l3_06_flow_d_a_killed_host_resumes") else {
        return;
    };
    let mib = 1u64 << 20;
    let len = 48 * mib + 77;
    let p = phone(&env);
    let (d, sock) = Linkd::start(
        &env,
        &p,
        &[
            "--ack-every",
            &mib.to_string(),
            "--window",
            &(2 * mib).to_string(),
        ],
    );
    // The socket pair stays open in this process, as a cable left in
    let sock = sock.map(|mut s| {
        s.write_all(&[1u8]).unwrap();
        s
    });
    let runs = if env.usb() { 50 } else { 20 };
    let child = |extra: &[(&str, &str)]| {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["l3_child", "--exact", "--nocapture", "--test-threads=1"])
            .env("L3_CHILD", "1")
            .env("L3_CHILD_ROOT", &p.dir)
            .env("L3_CHILD_LEN", len.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra {
            c.env(k, v);
        }
        if let Some(s) = &sock {
            c.stdin(Stdio::from(std::os::fd::OwnedFd::from(
                s.try_clone().unwrap(),
            )));
        }
        c.spawn().unwrap()
    };
    let lines = || -> Vec<String> {
        p.read("/host-child.log")
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    let mut rng = TestRng(66);
    let (mut killed_hello, mut killed_stream) = (0, 0);
    for run in 0..runs {
        let hello_only = run % 3 == 2;
        let before = lines().len();
        let mut c = if hello_only {
            child(&[("L3_CHILD_HELLO_ONLY", "1")])
        } else {
            child(&[])
        };
        if hello_only {
            assert!(
                wait_for(Duration::from_secs(60), || lines()[before..]
                    .iter()
                    .any(|l| l == "hello written")),
                "run {run}: hello not written"
            );
            killed_hello += 1;
        } else {
            assert!(
                wait_for(Duration::from_secs(60), || lines()[before..]
                    .iter()
                    .any(|l| l.starts_with("paired"))),
                "run {run}: not paired: {:?}",
                &lines()[before..]
            );
            std::thread::sleep(Duration::from_millis(50 + rng.next_u64() % 400));
            killed_stream += 1;
        }
        let _ = c.kill();
        let _ = c.wait();
    }
    // The last run goes to the end
    let before = lines().len();
    let mut c = child(&[]);
    let status = c.wait().unwrap();
    let all = lines();
    let mut paired: Vec<(u64, u64)> = Vec::new();
    for l in &all {
        if let Some(rest) = l.strip_prefix("paired after=") {
            let mut it = rest.split_whitespace();
            let after: u64 = it.next().unwrap().parse().unwrap();
            let discarded: u64 = it
                .next()
                .unwrap()
                .trim_start_matches("discarded=")
                .parse()
                .unwrap();
            assert!(l.ends_with("wrong=false"), "a wrong count: {l}");
            paired.push((after, discarded));
        }
    }
    let worst = paired.iter().map(|p| p.0).max().unwrap_or(0);
    let discarded: u64 = paired.iter().map(|p| p.1).sum();
    assert!(
        all[before..]
            .iter()
            .any(|l| l == "ended done=true verified=true"),
        "the last run finished the image: {status:?} {:?}",
        &all[before..]
    );
    assert!(
        worst <= 3,
        "every run paired within its first three frames: {paired:?}"
    );
    assert_eq!(d.says("wrong-code"), 0);
    env.ok(
        "flow (d), F1, F2",
        &format!(
            "{runs} installers killed with the cable in ({killed_stream} in the stream, {killed_hello} between hello and its answer); every next run paired within {worst} frames, {discarded} stale frames passed over in all, no wrong count; the last run finished the image ({} MiB) and it verified",
            len >> 20
        ),
    );
}

/// FunctionFS's events: a host reset (DISABLE, ENABLE), the UDC unbound
/// and bound (UNBIND, BIND), the bus suspended and resumed (SUSPEND,
/// RESUME, and nothing ends).
#[test]
fn l3_07_events() {
    let Some(env) = Env::get("l3_07_events") else {
        return;
    };
    if !env.usb() {
        return eprintln!("l3_07_events: USB only");
    }
    use nusb::MaybeFuture;
    let p = phone(&env);
    let (d, _) = Linkd::start(&env, &p, &[]);
    let mut usb = Usb::open(Duration::from_secs(30)).unwrap();
    let mut sh = Shell::new(
        env.max_data,
        None,
        Source::Bytes(Vec::new()),
        7,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut usb, &d);
    let token = sh.token;
    let count = |what: &str| d.says(what);
    // A reset by the host: the configuration goes and comes back
    let (dis, en) = (count("link: DISABLE"), count("link: ENABLE"));
    let _ = usb.dev.reset().wait();
    drop(usb);
    assert!(
        wait_for(Duration::from_secs(10), || count("link: DISABLE") > dis
            && count("link: ENABLE") > en),
        "DISABLE then ENABLE after a reset: {}",
        d.journal()
    );
    let mut usb = Usb::open(Duration::from_secs(30)).unwrap();
    let mut sh = Shell::new(
        env.max_data,
        token,
        Source::Bytes(Vec::new()),
        8,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut usb, &d);
    assert!(sh.step(&mut usb, HostCommand::Status, Duration::from_secs(10)));
    env.ok("reset", "a host reset: the daemon logged DISABLE then ENABLE, and the next session resumed with the token");
    drop(usb);
    // The UDC unbound and bound again (as a cable pull looks to the gadget)
    let (unb, bind) = (count("link: UNBIND"), count("link: BIND"));
    assert_eq!(env.ask("unbind", &p.dir), 0);
    assert!(
        wait_for(Duration::from_secs(10), || count("link: UNBIND") > unb),
        "UNBIND: {}",
        d.journal()
    );
    assert_eq!(env.ask("bind", &p.dir), 0);
    assert!(
        wait_for(Duration::from_secs(10), || count("link: BIND") > bind
            && count("link: ENABLE") > en + 1),
        "BIND and ENABLE: {}",
        d.journal()
    );
    let mut usb = Usb::open(Duration::from_secs(30)).unwrap();
    let mut sh = Shell::new(
        env.max_data,
        token,
        Source::Bytes(Vec::new()),
        9,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut usb, &d);
    env.ok("unbind and bind", "the UDC unbound: UNBIND (the session ended); bound again: BIND, ENABLE, and the next session resumed");
    if env.speed == Speed::Super {
        // dummy_hcd has no link states at SuperSpeed (its SET_LINK_STATE
        // does nothing), so the gadget is never suspended there
        return eprintln!("l3_07_events: SUSPEND and RESUME not at super speed (dummy_hcd)");
    }
    // Suspend: with nothing open the device may sleep
    let downs = count("link: DISABLE") + count("link: UNBIND");
    let ends_before = count("session");
    drop(sh);
    drop(usb);
    let (sus, res) = (count("link: SUSPEND"), count("link: RESUME"));
    assert_eq!(env.ask("autosuspend", &p.dir), 0);
    let slept = wait_for(Duration::from_secs(15), || count("link: SUSPEND") > sus);
    assert_eq!(env.ask("noautosuspend", &p.dir), 0);
    assert!(slept, "SUSPEND: {}", d.journal());
    assert!(
        wait_for(Duration::from_secs(10), || count("link: RESUME") > res),
        "RESUME: {}",
        d.journal()
    );
    assert_eq!(
        count("link: DISABLE") + count("link: UNBIND"),
        downs,
        "SUSPEND and RESUME end nothing"
    );
    assert_eq!(count("session"), ends_before);
    let mut usb = Usb::open(Duration::from_secs(30)).unwrap();
    let mut sh = Shell::new(
        env.max_data,
        token,
        Source::Bytes(Vec::new()),
        10,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut usb, &d);
    env.ok("suspend and resume", "the bus suspended: SUSPEND, resumed: RESUME; no DISABLE between, nothing ended; the link answered after");
}

/// A daemon that dies with the function bound: what the host sees, and the
/// next start's `remove` then `add` (E3) brings a clean function back.
#[test]
fn l3_08_a_daemon_that_dies_with_the_function_bound() {
    let Some(env) = Env::get("l3_08_a_daemon_that_dies_with_the_function_bound") else {
        return;
    };
    let p = phone(&env);
    let (mut d, sock) = Linkd::start(&env, &p, &[]);
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        None,
        Source::Bytes(Vec::new()),
        11,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    let token = sh.token;
    assert!(
        p.exists("/run/omamobile-install-link/offered"),
        "E3's record"
    );
    d.kill();
    // What the host sees
    let gone = sh.pump(
        &mut *w,
        Duration::from_secs(10),
        |s| s.saw(|e| *e == Event::LinkLost) || !s.host.ready(),
        |_, _| {},
    );
    let udc = if env.usb() { env.udc() } else { String::new() };
    let on_bus = if env.usb() {
        gadgets().into_iter().any(|g| link_interface(&g).is_some())
    } else {
        false
    };
    if env.usb() {
        env.ok(
            "a dead daemon, the host's view",
            &format!(
                "SIGKILL with the function bound: the host {}; the gadget's UDC reads \"{udc}\"; the link's interface is {} the bus",
                if gone { "lost the device" } else { "saw nothing" },
                if on_bus { "still on" } else { "off" }
            ),
        );
    }
    drop(w);
    let marker_before = p.exists("/var/lib/omamobile/link-gadget-started");
    // The next start: E3, `remove` then `add`
    let (d2, sock) = Linkd::start(&env, &p, &[]);
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        token,
        Source::Bytes(Vec::new()),
        12,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d2);
    assert!(sh.step(&mut *w, HostCommand::Status, Duration::from_secs(10)));
    let calls = p.gadget();
    assert_eq!(
        calls,
        ["add", "remove", "add"],
        "E3: remove before the next add"
    );
    assert!(
        !marker_before && !p.exists("/var/lib/omamobile/link-gadget-started"),
        "no hang marker: the changes finished"
    );
    if env.usb() {
        assert!(env.udc().starts_with("dummy_udc."));
    }
    env.ok(
        "a dead daemon, the next start",
        "E3's record found; remove then add; a clean function, resumed with the token; no hang marker left by either",
    );
}

/// The gadget script's hang marker on a real gadget: a marker of an
/// earlier boot leaves the gadget as it was for this boot; the next boot
/// adds the link.
#[test]
fn l3_09_the_hang_marker() {
    let Some(env) = Env::get("l3_09_the_hang_marker") else {
        return;
    };
    let mut p = phone(&env);
    if !env.usb() {
        // The daemon's side only: a stand-in that skips as the script does
        p.write(
            "/usr/lib/omamobile-install-link/omamobile-link-gadget",
            b"#!/bin/bash\necho \"$1\" >> \"$OMAMOBILE_LINK_ROOT/fake-gadget.log\"\nm=$OMAMOBILE_LINK_ROOT/var/lib/omamobile/link-gadget-started\nif [[ $1 == add && -e $m ]]; then rm -f \"$m\"; exit 3; fi\n",
        );
        fs::set_permissions(
            p.path("/usr/lib/omamobile-install-link/omamobile-link-gadget"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    p.write(
        "/var/lib/omamobile/link-gadget-started",
        b"add 00000000-0000-4000-8000-000000000000 2026-01-01T00:00:00+00:00\n",
    );
    let (mut d, _) = Linkd::start(&env, &p, &[]);
    assert!(
        wait_for(Duration::from_secs(30), || d.exited().is_some()),
        "the daemon ends"
    );
    assert_ne!(d.exited(), Some(0), "no link this boot: {}", d.journal());
    assert!(
        d.says("skipped for this boot") > 0,
        "the journal says why: {}",
        d.journal()
    );
    assert!(
        !p.exists("/var/lib/omamobile/link-gadget-started"),
        "the marker is gone"
    );
    if env.usb() {
        assert!(
            !gadgets().iter().any(|g| link_interface(g).is_some()),
            "no link on the bus"
        );
        assert!(
            env.udc().starts_with("dummy_udc."),
            "the gadget as it was, bound"
        );
        assert!(
            p.exists("/run/omamobile-install-link/gadget-skipped"),
            "the skip is on the tmpfs"
        );
    }
    assert!(
        !p.exists("/run/omamobile-install-link/offered"),
        "E3's record agrees: nothing to put back"
    );
    drop(d);
    // The next boot
    p.new_boot();
    let (d, sock) = Linkd::start(&env, &p, &[]);
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        None,
        Source::Bytes(Vec::new()),
        13,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    env.ok("the hang marker", "a marker of an earlier boot: no link, the gadget bound as it was, the marker gone, E3's record gone; the next boot: the link");
}

/// Sizes: megabyte frames both ways, and a streamed file of 4 GiB with its
/// digest; the time the phone's whole-file check takes.
#[test]
fn l3_10_sizes() {
    let Some(env) = Env::get("l3_10_sizes") else {
        return;
    };
    if env.usb() && env.speed == Speed::Full {
        return eprintln!("l3_10_sizes: not at full speed (12 Mbit/s)");
    }
    let p = phone(&env);
    let part = big_partition(&p, 64 << 20);
    let (d, sock) = Linkd::start(&env, &p, &[]);
    let mut w = wire(&env, sock);
    let mut sh = Shell::new(
        env.max_data,
        None,
        Source::Bytes(Vec::new()),
        14,
        Duration::from_millis(env.write_ms * 2),
    );
    sh.pair(&mut *w, &d);
    let t0 = Instant::now();
    assert!(
        sh.step(&mut *w, HostCommand::Backup, Duration::from_secs(300)),
        "{:?}",
        sh.events
    );
    assert!(sh.saw(|e| *e == Event::BackupDone));
    assert_eq!(
        sh.backup.get("efs").map(|b| sha256(b)),
        Some(part),
        "the 64 MiB partition's digest"
    );
    assert_eq!(sh.bad_reads, 0);
    env.ok(
        "megabyte frames (IN)",
        &format!(
            "a 64 MiB partition in {} KiB data frames, read twice by the phone, in {:.1}s; digest equal",
            env.max_data >> 10,
            t0.elapsed().as_secs_f64()
        ),
    );
    if env.big == 0 {
        return eprintln!("l3_10_sizes: the streamed file is off (L3_BIG_BYTES=0)");
    }
    let src = Source::Pattern {
        len: env.big,
        seed: 10,
    };
    let t = Instant::now();
    let begin = src.begin();
    let host_digest = t.elapsed();
    let t1 = Instant::now();
    let mut acked_at: Option<Instant> = None;
    sh.src = src.clone();
    sh.events.clear();
    sh.command(HostCommand::Factory(begin));
    let ok = sh.pump(
        &mut *w,
        Duration::from_secs(3 * 3600),
        |s| !s.host.busy(),
        |b, _| {
            if text(b).contains("\"t\":\"ack\"") {
                acked_at = Some(Instant::now());
            }
        },
    );
    assert!(
        ok && sh.saw(|e| *e == Event::FactoryVerified),
        "{:?}",
        sh.events
    );
    let total = t1.elapsed();
    let check = acked_at.map(|a| a.elapsed()).unwrap_or_default();
    let rate = src.len() as f64 / (total - check).as_secs_f64() / (1 << 20) as f64;
    env.ok(
        "a streamed file",
        &format!(
            "{} bytes in {} KiB frames, 64 MiB acks, 128 MiB window: sent at {rate:.0} MiB/s; the phone's whole-file check took {:.1}s; verified (the host's own digest took {:.1}s)",
            src.len(),
            env.max_data >> 10,
            check.as_secs_f64(),
            host_digest.as_secs_f64()
        ),
    );
    drop(d);
    let _ = fs::remove_file(p.path("/var/lib/omamobile/install-link/factory.zip"));
}

/// The real `systemctl show`, read as the daemon reads it (read-only; it
/// asks for two properties of the display manager and of first boot's
/// unit, which need not exist on this computer).
#[test]
fn l3_11_the_real_systemctl_show() {
    let Some(env) = Env::get("l3_11_the_real_systemctl_show") else {
        return;
    };
    if !Path::new("/run/systemd/system").exists() {
        return eprintln!("l3_11: no systemd here");
    }
    let p = Phone::new();
    let real = Path::new("/usr/bin/systemctl");
    if !real.exists() {
        return eprintln!("l3_11: no /usr/bin/systemctl");
    }
    let stand_in = p.path("/usr/bin/systemctl");
    fs::remove_file(&stand_in).unwrap();
    std::os::unix::fs::symlink(real, &stand_in).unwrap();
    let root = omamobile_linkd::paths::Root::new(&p.dir);
    let programs = omamobile_linkd::programs::Programs::new(&root);
    let fb = programs.unit_state("omamobile-firstboot.service");
    let dm = programs.unit_state("display-manager.service");
    let since = programs.active_since_us("display-manager.service");
    assert!(
        fb.is_some() && dm.is_some(),
        "systemctl answered: {fb:?} {dm:?}"
    );
    env.ok(
        "systemctl show",
        &format!("first boot's unit (absent here): {fb:?}; the display manager: {dm:?}, active since {since:?} µs"),
    );
}
