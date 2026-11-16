// SPDX-License-Identifier: MIT
//! A scratch root that stands in for the phone's file system, stand-ins for
//! the fixed programs and for systemd, a real `omamobile-linkd` process on
//! it, and the crate's host machine on the other end of a socket pair.
//! Nothing here opens a USB device, a gadget or anything outside the
//! scratch root.
#![allow(dead_code)]

use omamobile_link::digest::sha256;
use omamobile_link::host::{Command as HostCommand, Event, HostConfig, HostInput};
use omamobile_link::msg::{Code, FactoryBegin};
use omamobile_link::phone::Persistent;
use omamobile_link::sim::{SimHost, TestRng};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// A placeholder, as everywhere in this repository: no real serial.
pub const SERIAL: &str = "SERIAL-PLACEHOLDER";
pub const MAX_DATA: u32 = 1024;
pub const ACK_EVERY: u64 = 4096;
pub const WINDOW: u64 = 8192;
/// 10 frames, the last one short.
pub const IMAGE_LEN: usize = 9 * 1024 + 100;
/// Both machines' clocks run this many times faster than the wall's.
pub const SCALE: u64 = 50;

pub fn image() -> Vec<u8> {
    TestRng(7).bytes(IMAGE_LEN)
}

pub fn factory_begin(image: &[u8]) -> FactoryBegin {
    FactoryBegin {
        size: image.len() as u64,
        sha256: sha256(image),
        build: "test.260101.001".into(),
        name: "cheetah-test.260101.001-factory.zip".into(),
    }
}

/// The stand-in radio partitions: name, bytes (whole sectors), in `NV_PARTS`.
pub const PARTS: [(&str, usize, bool); 4] = [
    ("efs", 3072, true),
    ("persist", 1024, true),
    ("modem_a", 2048, true),
    ("devinfo", 512, false),
];

const SYSTEMCTL: &str = r#"#!/bin/bash
# A stand-in for systemctl under a scratch root: units are files.
R=$OMAMOBILE_LINK_ROOT; S=$R/fake-systemd
# (the test may have ended and taken its root away: write nothing then)
[[ -d $S ]] || exit 0
unit=${@: -1}
put() { printf '%s\n' "$2" > "$S/.$1.$$" && mv "$S/.$1.$$" "$S/$1"; }
case "$1" in
  show)
    case "$3" in
      ActiveState) cat "$S/$unit" 2>/dev/null || echo inactive ;;
      ActiveEnterTimestampMonotonic) cat "$S/$unit.since" 2>/dev/null || echo 0 ;;
    esac ;;
  start)
    echo "start $unit" >> "$S/log"
    if [[ $unit == omamobile-firstboot.service && -f $R/etc/omamobile/firstboot.conf ]]; then
      state=$(cat "$S/$unit" 2>/dev/null || echo inactive)
      [[ $state == activating || $state == active ]] || put "$unit" activating
    fi ;;
  reboot) echo reboot >> "$S/log" ;;
esac
"#;

const SYSTEMD_RUN: &str = r#"#!/bin/bash
# A stand-in for systemd-run: the transient unit is a file, the worker is
# the test.
R=$OMAMOBILE_LINK_ROOT; S=$R/fake-systemd
[[ -d $S ]] || exit 0
mkdir -p "$R/var/lib/omamobile/provision/work.standin"
rm -f "$R/var/lib/omamobile/install-link/provision.result"
printf 'active\n' > "$S/.run.$$" && mv "$S/.run.$$" "$S/omamobile-provision-run.service"
echo "systemd-run $*" >> "$S/log"
"#;

const FIRSTBOOT: &str = r#"#!/bin/bash
# A stand-in for `omamobile-firstboot --check --porcelain FILE`: it objects
# to what the test says, and notes the candidate's mode.
R=$OMAMOBILE_LINK_ROOT
[[ -d $R/fake-systemd ]] || exit 2
[[ $1 == --check && $2 == --porcelain && -f $3 ]] || exit 2
stat -c %a "$3" >> "$R/fake-systemd/candidate-modes"
if [[ -s $R/fake-porcelain ]]; then cat "$R/fake-porcelain"; exit 1; fi
exit 0
"#;

const SMOKE: &str = "#!/bin/bash\necho '{\"device\":\"cheetah\",\"rows\":[]}'\n";

const GADGET: &str = "#!/bin/bash\n[[ -d $OMAMOBILE_LINK_ROOT ]] || exit 1\necho \"$1\" >> \"$OMAMOBILE_LINK_ROOT/fake-gadget.log\"\n";

static NEXT: AtomicU32 = AtomicU32::new(0);

fn put(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension(format!("tmp{}", NEXT.fetch_add(1, Ordering::SeqCst)));
    fs::write(&tmp, bytes).unwrap();
    fs::rename(&tmp, path).unwrap();
}

fn program(path: &Path, text: &str) {
    put(path, text.as_bytes());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The scratch root.
pub struct Phone {
    pub dir: PathBuf,
    boot: u32,
    /// How often first-boot setup ran to its end, and with which file.
    pub firstboot_runs: u32,
    pub firstboot_ran_with: Vec<u8>,
}

impl Drop for Phone {
    fn drop(&mut self) {
        // LINKD_KEEP=1 keeps a failed test's root to look at
        if !std::thread::panicking() || std::env::var_os("LINKD_KEEP").is_none() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

impl Phone {
    /// A freshly installed phone before its first start: the marker, the
    /// descriptor, the radio partitions, and no state.
    pub fn new() -> Phone {
        // (short: a local socket's path holds 107 bytes)
        let dir = std::env::temp_dir().join(format!(
            "linkd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        let p = Phone {
            dir,
            boot: 0,
            firstboot_runs: 0,
            firstboot_ran_with: Vec::new(),
        };
        p.write("/etc/omamobile/install-link", b"");
        p.write(
            "/usr/share/omamobile/device.conf",
            b"CODENAME=cheetah\nCOMPATIBLE=\"google,gs201-cheetah\"\n\
              NV_PARTS=\"efs persist modem_a\"\nNV_BACKUP_EXTRA=\"devinfo\"\n",
        );
        p.write(
            "/usr/share/omamobile/rootfs/MANIFEST",
            b"# Omarchy Mobile root image\nversion: 2026.01.01\n",
        );
        p.write(
            "/proc/bootconfig",
            format!("androidboot.slot_suffix = \"_b\"\nandroidboot.serialno = \"{SERIAL}\"\n")
                .as_bytes(),
        );
        p.write("/proc/sys/kernel/osrelease", b"7.0.0-test\n");
        p.write(
            "/proc/device-tree/compatible",
            b"google,gs201-cheetah\0google,gs201\0",
        );
        p.write("/proc/mounts", b"/dev/sda99 / ext4 rw 0 0\n");
        p.write(
            "/etc/passwd",
            b"root:x:0:0::/root:/bin/bash\nnobody:x:65534:65534::/:/usr/bin/nologin\n",
        );
        p.write("/sys/class/power_supply/battery/type", b"Battery\n");
        p.write("/sys/class/power_supply/battery/capacity", b"80\n");
        p.write("/sys/class/power_supply/battery/status", b"Charging\n");
        let mut rng = TestRng(11);
        for (i, (name, len, _)) in PARTS.iter().enumerate() {
            let dev = format!("sda{}", i + 1);
            p.write(&format!("/dev/{dev}"), &rng.bytes(*len));
            p.write(
                &format!("/sys/class/block/{dev}/uevent"),
                format!("DEVNAME={dev}\nPARTNAME={name}\n").as_bytes(),
            );
            p.write(&format!("/sys/class/block/{dev}/ro"), b"1\n");
            p.write(
                &format!("/sys/class/block/{dev}/size"),
                format!("{}\n", len / 512).as_bytes(),
            );
        }
        program(&p.path("/usr/bin/systemctl"), SYSTEMCTL);
        program(&p.path("/usr/bin/systemd-run"), SYSTEMD_RUN);
        program(&p.path("/usr/bin/omamobile-firstboot"), FIRSTBOOT);
        program(&p.path("/usr/bin/phone-smoke"), SMOKE);
        program(
            &p.path("/usr/lib/omamobile-install-link/omamobile-link-gadget"),
            GADGET,
        );
        fs::create_dir_all(p.path("/fake-systemd")).unwrap();
        fs::create_dir_all(p.path("/var/lib/omamobile/install-link")).unwrap();
        let mut p = p;
        p.new_boot();
        p
    }

    pub fn path(&self, abs: &str) -> PathBuf {
        self.dir.join(abs.trim_start_matches('/'))
    }
    pub fn write(&self, abs: &str, bytes: &[u8]) {
        put(&self.path(abs), bytes);
    }
    pub fn read(&self, abs: &str) -> Option<String> {
        fs::read_to_string(self.path(abs)).ok()
    }
    pub fn exists(&self, abs: &str) -> bool {
        self.path(abs).exists()
    }
    pub fn remove(&self, abs: &str) {
        let _ = fs::remove_file(self.path(abs));
    }

    /// The phone restarts: another boot id, an empty `/run`, no unit has
    /// run; first boot's unit starts by itself while `firstboot.conf` is
    /// there.
    pub fn new_boot(&mut self) {
        self.boot += 1;
        self.write(
            "/proc/sys/kernel/random/boot_id",
            format!("00000000-0000-4000-8000-{:012}\n", self.boot).as_bytes(),
        );
        let _ = fs::remove_dir_all(self.path("/run"));
        // (the next boot's initramfs composes the gadget anew)
        if self.exists("/fake-gadget.log") {
            let mut log = self.read("/fake-gadget.log").unwrap();
            log.push_str("boot\n");
            self.write("/fake-gadget.log", log.as_bytes());
        }
        for unit in [
            "omamobile-firstboot.service",
            "omamobile-provision-run.service",
            "display-manager.service.since",
        ] {
            self.remove(&format!("/fake-systemd/{unit}"));
        }
        if self.exists("/etc/omamobile/firstboot.conf") {
            self.set_unit("omamobile-firstboot.service", "activating");
        }
    }
    pub fn boot_id(&self) -> String {
        self.read("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .to_string()
    }

    pub fn unit(&self, unit: &str) -> String {
        self.read(&format!("/fake-systemd/{unit}"))
            .map_or("inactive".into(), |s| s.trim().to_string())
    }
    pub fn set_unit(&self, unit: &str, state: &str) {
        self.write(
            &format!("/fake-systemd/{unit}"),
            format!("{state}\n").as_bytes(),
        );
    }
    /// What the stand-in systemd was asked: `start UNIT`, `reboot`,
    /// `systemd-run ...`.
    pub fn systemd_log(&self) -> Vec<String> {
        self.read("/fake-systemd/log")
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    pub fn reboots(&self) -> usize {
        self.systemd_log().iter().filter(|l| *l == "reboot").count()
    }
    pub fn provision_starts(&self) -> usize {
        self.systemd_log()
            .iter()
            .filter(|l| l.starts_with("systemd-run "))
            .count()
    }
    /// `omamobile-link-gadget`'s calls: `add`, `remove`.
    pub fn gadget(&self) -> Vec<String> {
        self.read("/fake-gadget.log")
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The link's state file, read as the daemon reads it.
    pub fn state(&self) -> Option<Persistent> {
        let bytes = fs::read(self.path("/var/lib/omamobile/install-link/state")).ok()?;
        Some(Persistent::from_json(&bytes).expect("the state file reads"))
    }
    pub fn put_state(&self, p: &Persistent) {
        self.write(
            "/var/lib/omamobile/install-link/state",
            p.to_json().as_bytes(),
        );
    }
    /// A state file written in this boot.
    pub fn state_now(&self, change: impl FnOnce(&mut Persistent)) -> Persistent {
        let mut p = Persistent {
            boot_id: self.boot_id(),
            ..Persistent::default()
        };
        change(&mut p);
        self.put_state(&p);
        p
    }
    pub fn done(&self) -> Option<String> {
        self.read("/var/lib/omamobile/install-link/done")
            .map(|s| s.trim().to_string())
    }
    pub fn marker(&self) -> bool {
        self.exists("/etc/omamobile/install-link")
    }
    pub fn image_len(&self) -> Option<u64> {
        fs::metadata(self.path("/var/lib/omamobile/install-link/factory.zip"))
            .ok()
            .map(|m| m.len())
    }

    /// The uid the account gets: the test's own, so that its agent is the
    /// account's (`SO_PEERCRED`).
    pub fn my_uid() -> u32 {
        rustix::process::getuid().as_raw()
    }
    /// An account as first boot makes it, and its `user` file.
    pub fn add_account(&self, name: &str, uid: u32, firstboots: bool) {
        let mut passwd = self.read("/etc/passwd").unwrap();
        passwd.push_str(&format!("{name}:x:{uid}:{uid}::/home/{name}:/bin/bash\n"));
        self.write("/etc/passwd", passwd.as_bytes());
        if firstboots {
            self.write(
                "/var/lib/omamobile/firstboot/user",
                format!("{name}\n").as_bytes(),
            );
        }
    }
    /// What a phone looks like when first boot ran to its end.
    pub fn firstboot_was_done(&self) {
        self.add_account("owner", Phone::my_uid().max(1000), true);
        self.write("/var/lib/omamobile/firstboot/done", b"2026-01-01\n");
        self.write("/etc/omamobile/boot-update.conf", b"AUTO_WRITE=yes\n");
    }

    /// First-boot setup runs to its end, as the script does: the account,
    /// then `firstboot.conf` deleted, then its `done`. With `failed`, an
    /// essential step fails and the file is kept.
    pub fn finish_firstboot(&mut self, failed: &[&str]) {
        let conf = fs::read(self.path("/etc/omamobile/firstboot.conf")).expect("a firstboot.conf");
        let mut log = self
            .read("/var/log/omamobile/firstboot.log")
            .unwrap_or_default();
        log.push_str("=== omamobile-firstboot 2026-01-01T00:00:00+00:00 ===\n");
        for step in [
            "pacman keyring",
            "user owner",
            "SSH keys",
            "login",
            "user setup",
        ] {
            log.push_str(&format!("--- {step}\n"));
            // ("user": the account step, which the script logs with the name)
            let bad = failed
                .iter()
                .any(|f| step == *f || (*f == "user" && step == "user owner"));
            log.push_str(if bad {
                "    FAILED (1)\n"
            } else {
                "    done\n"
            });
        }
        self.write("/var/log/omamobile/firstboot.log", log.as_bytes());
        if !self.read("/etc/passwd").unwrap().contains("owner:") {
            self.add_account("owner", Phone::my_uid().max(1000), true);
        }
        if failed.is_empty() {
            self.firstboot_runs += 1;
            self.firstboot_ran_with = conf;
            self.remove("/etc/omamobile/firstboot.conf");
            self.write("/var/lib/omamobile/firstboot/done", b"2026-01-01\n");
            self.set_unit("omamobile-firstboot.service", "active");
        } else {
            self.set_unit("omamobile-firstboot.service", "failed");
        }
    }

    /// The provisioning worker ends: the checksum file as its last write,
    /// then its result, the working directory gone, the unit gone.
    pub fn finish_provision(&self, failed: Option<&str>) {
        self.write(
            "/run/omamobile-install-link/provision.progress",
            b"{\"what\":\"provision\",\"step\":\"camera\",\"done\":6,\"total\":6}\n",
        );
        let steps: Vec<String> = [
            "firmware",
            "sensors",
            "gnss",
            "modem",
            "fingerprint",
            "camera",
        ]
        .iter()
        .map(|s| {
            let state = if failed == Some(s) { "failed" } else { "ok" };
            format!("{{\"step\":\"{s}\",\"state\":\"{state}\"}}")
        })
        .collect();
        let ok = failed.is_none() || failed == Some("camera");
        if ok {
            let sums = fs::read(self.path("/var/lib/omamobile/install-link/modem-nv.sha256"))
                .expect("the pending checksums");
            self.write("/etc/omamobile/modem-nv.sha256", &sums);
        }
        self.write(
            "/var/lib/omamobile/install-link/provision.result",
            format!(
                "{{\"version\":1,\"ok\":{ok},\"steps\":[{}]}}\n",
                steps.join(",")
            )
            .as_bytes(),
        );
        let _ = fs::remove_dir_all(self.path("/var/lib/omamobile/provision/work.standin"));
        self.remove("/fake-systemd/omamobile-provision-run.service");
    }

    /// The daemon's journal: everything it wrote to standard error.
    pub fn journal(&self) -> String {
        self.read("/linkd.log").unwrap_or_default()
    }

    /// `omamobile-install-link VERB --root DIR`: exit status, output.
    pub fn command(&self, verb: &str) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_omamobile-install-link"))
            .arg(verb)
            .arg("--root")
            .arg(&self.dir)
            .output()
            .unwrap();
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    /// Start the daemon on this root, its link one end of a new socket pair.
    pub fn daemon(&self) -> Daemon {
        self.daemon_with(&[])
    }

    pub fn daemon_with(&self, extra: &[&str]) -> Daemon {
        let (mine, theirs) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("/linkd.log"))
            .unwrap();
        let offers_before = self.journal().matches("link: offered").count();
        let mut c = Command::new(env!("CARGO_BIN_EXE_omamobile-linkd"));
        c.arg("--root")
            .arg(&self.dir)
            .args(["--transport", "stdio"])
            .args(["--clock-scale", &SCALE.to_string()])
            .args(["--poll-ms", "5"])
            .args(["--max-data", &MAX_DATA.to_string()])
            .args(["--ack-every", &ACK_EVERY.to_string()])
            .args(["--window", &WINDOW.to_string()])
            .args(["--working-space", "0"]);
        // (a later value of an option replaces an earlier one)
        c.args(extra)
            .stdin(Stdio::from(theirs))
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));
        let child = c.spawn().unwrap();
        let sock = UnixStream::from(mine);
        sock.set_read_timeout(Some(Duration::from_millis(1)))
            .unwrap();
        sock.set_write_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        Daemon {
            child,
            sock,
            up: false,
            gone: false,
            screen: None,
            agent: None,
            screen_path: self.path("/run/omamobile-install-link/screen"),
            agent_path: self.path("/run/omamobile-install-link/agent"),
            gadget_log: self.path("/fake-gadget.log"),
            journal: self.path("/linkd.log"),
            offers_before,
        }
    }
}

/// A peer of one of the daemon's local sockets: lines in, lines out.
pub struct Peer {
    stream: UnixStream,
    buf: Vec<u8>,
    pub lines: Vec<String>,
    pub closed: bool,
}

impl Peer {
    pub fn connect(path: &Path) -> Option<Peer> {
        let stream = UnixStream::connect(path).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .unwrap();
        Some(Peer {
            stream,
            buf: Vec::new(),
            lines: Vec::new(),
            closed: false,
        })
    }
    pub fn say(&mut self, line: &str) {
        let _ = self.stream.write_all(format!("{line}\n").as_bytes());
    }
    /// Take in what has arrived.
    pub fn poll(&mut self) {
        let mut b = [0u8; 256];
        loop {
            match self.stream.read(&mut b) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => self.buf.extend_from_slice(&b[..n]),
                Err(_) => break,
            }
        }
        while let Some(at) = self.buf.iter().position(|c| *c == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=at).collect();
            self.lines
                .push(String::from_utf8_lossy(&line[..at]).to_string());
        }
    }
    /// Wait for a line that `f` takes.
    pub fn wait(&mut self, f: impl Fn(&str) -> bool) -> Option<String> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            self.poll();
            if let Some(l) = self.lines.iter().rev().find(|l| f(l)) {
                return Some(l.clone());
            }
            if self.closed || Instant::now() > end {
                return None;
            }
        }
    }
    /// The newest code a `code DIGITS ...` line carried.
    pub fn code(&mut self) -> Option<Code> {
        self.poll();
        // Only if it is the newest thing said about a code
        let last = self.lines.iter().rev().find(|l| {
            ["code ", "locked", "open ", "paired", "closed ", "off"]
                .iter()
                .any(|p| l.starts_with(p))
        })?;
        Code::parse(last.strip_prefix("code ")?.split(' ').next()?)
    }
}

/// A running `omamobile-linkd`, and the test's end of its link.
pub struct Daemon {
    pub child: Child,
    pub sock: UnixStream,
    /// `ENABLE` was sent.
    pub up: bool,
    /// The daemon closed its end.
    pub gone: bool,
    pub screen: Option<Peer>,
    pub agent: Option<Peer>,
    screen_path: PathBuf,
    agent_path: PathBuf,
    gadget_log: PathBuf,
    journal: PathBuf,
    offers_before: usize,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    /// Its exit status, if it has ended. A signal's number, negated.
    pub fn exited(&mut self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        let status = self.child.try_wait().unwrap()?;
        Some(
            status
                .code()
                .unwrap_or_else(|| -status.signal().unwrap_or(0)),
        )
    }
    pub fn wait_exit(&mut self) -> i32 {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(code) = self.exited() {
                return code;
            }
            assert!(Instant::now() < end, "the daemon does not end");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// Power is lost, or systemd stops it: no clean-up runs.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn gadget_calls(&self) -> Vec<String> {
        fs::read_to_string(&self.gadget_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    /// Wait until the daemon has offered the link (its journal says so) or
    /// has ended. True: offered.
    pub fn settled(&mut self) -> bool {
        let before = Instant::now();
        loop {
            if self.exited().is_some() {
                return false;
            }
            let offers = fs::read_to_string(&self.journal)
                .unwrap_or_default()
                .matches("link: offered")
                .count();
            if offers > self.offers_before {
                return true;
            }
            assert!(
                before.elapsed() < Duration::from_secs(10),
                "the daemon neither offers nor ends"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// FunctionFS's `ENABLE` and `DISABLE`, as the socket pair has them.
    pub fn enable(&mut self) {
        let _ = self.sock.write(&[1]);
        self.up = true;
    }
    pub fn disable(&mut self) {
        let _ = self.sock.write(&[0]);
        self.up = false;
    }
    /// One frame to the daemon.
    pub fn send(&mut self, frame: &[u8]) -> bool {
        self.sock.write(frame).is_ok()
    }
    /// One frame from the daemon, if one is there within `wait`.
    pub fn recv(&mut self, wait: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + wait;
        let mut buf = vec![0u8; 300_000];
        loop {
            match self.sock.read(&mut buf) {
                Ok(0) => {
                    self.gone = true;
                    return None;
                }
                Ok(n) => return Some(buf[..n].to_vec()),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => {
                    self.gone = true;
                    return None;
                }
            }
            if Instant::now() >= end {
                return None;
            }
        }
    }
    /// The first-start screen program's stand-in, connected.
    pub fn screen(&mut self) -> &mut Peer {
        if self.screen.is_none() {
            self.screen = Peer::connect(&self.screen_path);
        }
        self.screen.as_mut().expect("the screen socket")
    }
    /// The owner's session agent's stand-in, connected: the session is up.
    pub fn agent(&mut self) -> &mut Peer {
        if self.agent.as_ref().is_none_or(|a| a.closed) {
            self.agent = Peer::connect(&self.agent_path);
        }
        self.agent.as_mut().expect("the agent socket")
    }
}

/// The installer: the crate's host machine and its stand-in shell, on a
/// clock that runs as fast as the daemon's.
pub struct Installer {
    pub host: SimHost,
    start: Instant,
}

pub fn host_config() -> HostConfig {
    let mut c = HostConfig::new("omamobile-install", "0.0.1", SERIAL.as_bytes());
    c.max_data = MAX_DATA;
    c
}

impl Installer {
    pub fn new() -> Installer {
        Installer::with(SimHost::new(host_config()))
    }
    pub fn with(mut host: SimHost) -> Installer {
        host.factory = image();
        Installer {
            host,
            start: Instant::now(),
        }
    }
    pub fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64 * SCALE
    }
    fn write(&mut self, d: &mut Daemon, frames: Vec<Vec<u8>>) {
        for f in frames {
            d.send(&f);
        }
    }
    pub fn feed(&mut self, d: &mut Daemon, input: HostInput<'_>) {
        let now = self.now();
        let frames = self.host.feed(now, input);
        self.write(d, frames);
    }
    /// The cable goes in.
    pub fn connect(&mut self, d: &mut Daemon) {
        d.enable();
        self.feed(d, HostInput::LinkUp);
    }
    pub fn command(&mut self, d: &mut Daemon, cmd: HostCommand) {
        self.feed(d, HostInput::Command(cmd));
    }
    /// Carry frames and keep the host's deadlines for `real`.
    pub fn pump(&mut self, d: &mut Daemon, real: Duration) {
        let end = Instant::now() + real;
        loop {
            if let Some(frame) = d.recv(Duration::from_millis(1)) {
                self.feed(d, HostInput::Frame(&frame));
            } else if d.gone && d.up {
                d.up = false;
                self.feed(d, HostInput::LinkDown);
            }
            if self.host.host.deadline().is_some_and(|t| t <= self.now()) {
                self.feed(d, HostInput::Tick);
            }
            if Instant::now() >= end {
                return;
            }
        }
    }
    /// Pump until `f` holds; panics with `what` after ten seconds.
    pub fn until(&mut self, d: &mut Daemon, what: &str, f: impl Fn(&Installer) -> bool) {
        let before = Instant::now();
        while !f(self) {
            assert!(
                before.elapsed() < Duration::from_secs(10),
                "never: {what}: {:?}",
                self.host.events
            );
            self.pump(d, Duration::from_millis(2));
        }
    }
    pub fn saw(&self, f: impl Fn(&Event) -> bool) -> bool {
        self.host.events.iter().any(f)
    }
    /// Connect and pair with the code on the first-start screen.
    pub fn pair_at_screen(&mut self, d: &mut Daemon) {
        self.connect(d);
        self.until(d, "a code is asked for", |i| {
            i.saw(|e| *e == Event::NeedCode)
        });
        let code = d
            .screen()
            .wait(|l| l.starts_with("code "))
            .and_then(|l| Code::parse(&l[5..]))
            .expect("the screen shows a code");
        self.command(d, HostCommand::Pair(code));
        self.until(d, "paired", |i| i.host.host.ready());
    }
    /// One step, to its closing event.
    pub fn step(&mut self, d: &mut Daemon, cmd: HostCommand) {
        self.host.take_events();
        self.command(d, cmd);
        self.pump(d, Duration::from_millis(2));
        self.until(d, "the step ends", |i| !i.host.host.busy());
    }
}

/// The owner's answers, as the crate's tests have them.
pub fn answers() -> omamobile_link::answers::ValidAnswers {
    omamobile_link::answers::Answers {
        username: "owner".into(),
        // The form of a sha512-crypt hash; not the hash of anything
        password_hash: format!("$6$saltsalt${}", "a".repeat(86)),
        credential: Some("pin".into()),
        hostname: Some("phone".into()),
        timezone: Some("Europe/Berlin".into()),
        wifi_ssid: Some("<SSID>".into()),
        wifi_psk: Some("a passphrase".into()),
        ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA owner@computer".into()],
        full_name: Some("An Owner".into()),
        email: Some("owner@example.org".into()),
    }
    .validate()
    .expect("the test's answers are valid")
}
