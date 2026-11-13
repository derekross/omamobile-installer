// SPDX-License-Identifier: MIT
//! The steps after the write, in the tool's `json` mode (docs/DESIGN.md
//! §13, decided by the maintainer on 2026-11-13): what Omarchy Mobile's
//! `tools/install-guided` does once `tools/install-phone` has written a
//! phone, with the release's own tools, until the install link does it.
//!
//! 1. the USB connection: the NetworkManager connection install-guided adds
//!    (a plain link, no sharing), shown first and added on the owner's
//!    click; or the command, for the owner to run;
//! 2. the first start: waited for over USB SSH, with a time bound;
//! 3. the radio backup (`tools/nv-backup`), into a folder the owner is shown;
//! 4. the firmware (`tools/phone-provision --first-install --nv-backup DIR
//!    FACTORY.zip`), from the factory image this run checked;
//! 5. the restart, and the check (`tools/phone-smoke --json` when the
//!    release ships it), in plain words.
//!
//! The release's tools run only as the signed release has them: each file
//! under `tools/`, and the descriptor beside them, is checked against the
//! signed manifest and copied into a folder of this tool's own before
//! anything of it runs, and the copy is what runs. Nothing here extracts
//! anything from a factory image: that is `tools/phone-provision`'s.
//!
//! Every step that talks to the phone first asks the phone that answers
//! over USB SSH for the digest of its serial number, and goes on only when
//! it is the digest of the phone the write was pinned to (install-guided's
//! `written_phone`, DESIGN §8).
//!
//! How far it got is kept in a record (`after.json` in the tool's state
//! folder), written after each step, so that a run that was cut off goes
//! on from the step that did not finish. The record holds the phone's
//! digest and the radio backup's folder: it is private, like the backup.
//!
//! The words on these screens are this file's: the core's flow ends at the
//! write. They pass through the core's filter for what a terminal acts on
//! ([`Screen::fit_to_print`]), since some of them quote a tool's output.
use crate::{Res, Watch};
use omamobile_installer_core::bundle::VerifiedRelease;
use omamobile_installer_core::flow::{Retry, Screen, ScreenChoice, ScreenStep};
use omamobile_installer_core::hash::StreamHasher;
use serde_json::{json, Map, Value};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The NetworkManager connection, as `tools/install-guided` adds it
/// (`nm_profile`, decided there on 2026-11-10: a plain link).
pub const NM_PROFILE: &str = "omamobile-phone";
const NM_ADD: [&str; 18] = [
    "connection",
    "add",
    "type",
    "ethernet",
    "con-name",
    NM_PROFILE,
    "match.driver",
    "cdc_ncm",
    "ipv4.method",
    "manual",
    "ipv4.addresses",
    "172.16.42.2/24",
    "ipv4.never-default",
    "yes",
    "ipv6.method",
    "disabled",
    "connection.autoconnect-priority",
    "-100",
];
/// The phone's USB address, the same on every phone (its initramfs sets
/// it up).
const PHONE_ADDR: &str = "172.16.42.1";

/// What the phone is asked over USB SSH. Each only reads, but the restart.
mod ask {
    /// The digest of the serial number the bootloader handed the kernel,
    /// worked out on the phone: install-guided's `PHONE_ID`. The kernel
    /// parameter's name is put together here so that this file's text
    /// holds no line an identifier scan would take for a leaked serial.
    pub fn phone_id() -> String {
        let param = ["androidboot", ".serial", "no"].concat();
        format!(
            r#"v=$(sed -n 's/^{p}[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' /proc/bootconfig 2>/dev/null | head -n 1)
[ -n "$v" ] || v=$(tr " " "\n" < /proc/cmdline 2>/dev/null | sed -n 's/^{p}=//p' | head -n 1)
[ -n "$v" ] || v=$(tr -d "\000" < /proc/device-tree/serial-number 2>/dev/null)
if [ -n "$v" ]; then printf %s "$v" | sha256sum | cut -d" " -f1; else echo no-serial; fi"#,
            p = param.replace('.', "\\.")
        )
    }
    /// The first start has finished: `omamobile-firstboot`'s marker. A
    /// phone whose image carries no first-start answers never makes one;
    /// its first start is over when systemd has finished starting.
    pub const FIRST_START_DONE: &str = "test -e /var/lib/omamobile/firstboot/done || { ! test -e /etc/omamobile/firstboot.conf && systemctl is-system-running 2>/dev/null | grep -cE '^(running|degraded)$' >/dev/null; }";
    pub const BOOT_ID: &str = "cat /proc/sys/kernel/random/boot_id";
    pub const SYSTEM: &str = "systemctl is-system-running";
    /// install-guided's restart: the modem's unit is stopped first, since
    /// `reboot-to` stops no unit (Omarchy Mobile's AGENTS.md, section 2).
    pub const RESTART: &str = "systemctl stop omamobile-modem >/dev/null 2>&1; (sleep 1; reboot-to omamobile) >/dev/null 2>&1 &";
}

/// How long each wait lasts, in seconds: install-guided's.
#[derive(Debug, Clone)]
pub struct Waits {
    pub first_start: u64,
    pub restart: u64,
    pub settle: u64,
    pub poll: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Waits {
            first_start: 1200,
            restart: 600,
            settle: 180,
            poll: Duration::from_secs(5),
        }
    }
}

/// The programs of this computer the steps run, and where things are kept.
#[derive(Debug, Clone)]
pub struct Seams {
    pub nmcli: PathBuf,
    pub ssh_keygen: PathBuf,
    pub waits: Waits,
    /// The owner's home, for `~/.ssh` and the radio backup's folder.
    pub home: PathBuf,
    /// Where the record and the tools' copies are kept.
    pub state: PathBuf,
    /// Where a key made for the installer is kept.
    pub data: PathBuf,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Seams {
    /// This computer's: the programs on `PATH`, and the XDG folders.
    pub fn system() -> Option<Self> {
        Self::with_programs(
            PathBuf::from("nmcli"),
            PathBuf::from("ssh-keygen"),
            Waits::default(),
        )
    }

    /// The programs named; the folders from the environment.
    pub fn with_programs(nmcli: PathBuf, ssh_keygen: PathBuf, waits: Waits) -> Option<Self> {
        let home = env_dir("HOME")?;
        Some(Seams {
            nmcli,
            ssh_keygen,
            waits,
            state: env_dir("XDG_STATE_HOME")
                .unwrap_or_else(|| home.join(".local/state"))
                .join("omamobile-installer"),
            data: env_dir("XDG_DATA_HOME")
                .unwrap_or_else(|| home.join(".local/share"))
                .join("omamobile-installer"),
            home,
        })
    }
}

// ---- The SSH key -------------------------------------------------------------

/// The key the phone will take from this computer.
#[derive(Debug, Clone)]
pub struct Key {
    /// The public key, one line.
    pub public: String,
    /// The private key's file, handed to `tools/phone-ssh`.
    pub private: PathBuf,
    /// It was made for the installer, in its data folder, because this
    /// computer had none.
    pub made: bool,
}

fn one_public_line(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let line = lines.next()?.trim().to_string();
    let kinds = [
        "ssh-ed25519 ",
        "ssh-rsa ",
        "ecdsa-sha2-",
        "sk-ssh-ed25519@",
        "sk-ecdsa-",
    ];
    (lines.next().is_none() && kinds.iter().any(|k| line.starts_with(k))).then_some(line)
}

/// The owner's key (`~/.ssh/id_ed25519.pub`, install-guided's default), or
/// the installer's own, made once with `ssh-keygen` in the installer's
/// data folder when the owner has none.
pub fn choose_key(seams: &Seams) -> Res<Key> {
    let owner = seams.home.join(".ssh/id_ed25519.pub");
    if owner.is_file() {
        let public = one_public_line(&owner)
            .ok_or_else(|| format!("{} is not one OpenSSH public key line", owner.display()))?;
        return Ok(Key {
            public,
            private: seams.home.join(".ssh/id_ed25519"),
            made: false,
        });
    }
    let dir = seams.data.join("ssh");
    let private = dir.join("id_ed25519");
    let public = dir.join("id_ed25519.pub");
    if !public.is_file() {
        private_dir(&dir)?;
        let made = Command::new(&seams.ssh_keygen)
            .args([
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "omamobile-installer",
                "-f",
            ])
            .arg(&private)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map_err(|e| {
                format!("ssh-keygen could not be started ({e}): this computer needs OpenSSH")
            })?;
        if !made.success() {
            return Err("ssh-keygen could not make a key for the installer".into());
        }
    }
    let line = one_public_line(&public)
        .ok_or_else(|| format!("{} is not one OpenSSH public key line", public.display()))?;
    Ok(Key {
        public: line,
        private,
        made: true,
    })
}

/// A folder only the owner can read.
fn private_dir(dir: &Path) -> Res<()> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path, bytes: &[u8]) -> Res<()> {
    let new = path.with_extension("new");
    fs::write(&new, bytes).map_err(|e| format!("{}: {e}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&new, path)?;
    Ok(())
}

// ---- The release's tools -----------------------------------------------------

/// What a release must ship for the steps after the write.
const NEEDED: [&str; 3] = [
    "tools/phone-ssh",
    "tools/nv-backup",
    "tools/phone-provision",
];
const SMOKE: &str = "tools/phone-smoke";

/// Whether the release's folder holds the tools these steps run. Only a
/// look at the names: they are checked against the signed manifest before
/// any of them runs ([`copy_tools`]).
pub fn release_has_tools(dir: &Path) -> bool {
    NEEDED.iter().all(|t| dir.join(t).is_file())
}

/// Every file under `tools/` that the signed manifest lists, and the
/// descriptor, checked and copied into `to`, which is made afresh. A file
/// that is not the signed one stops it; a file the manifest does not list
/// is not copied, so it can't run.
fn copy_tools(release: &VerifiedRelease, from: &Path, to: &Path) -> Res<Vec<String>> {
    let _ = fs::remove_dir_all(to);
    private_dir(&to.join("tools"))?;
    let mut copied = Vec::new();
    let listed: Vec<&str> = release
        .manifest()
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.starts_with("tools/") || *p == "device.conf")
        .collect();
    for path in listed {
        if path.contains("..") || path.matches('/').count() > 1 {
            return Err(format!(
                "the release lists {path}, which is not a file of its tools folder"
            )
            .into());
        }
        let bytes = fs::read(from.join(path)).map_err(|e| format!("{path}: {e}"))?;
        release
            .check_file(path, &bytes)
            .map_err(|e| format!("{path} is not the file the release signed ({e})"))?;
        let file = to.join(path);
        fs::write(&file, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o700))?;
        }
        copied.push(path.to_string());
    }
    for needed in NEEDED {
        if !copied.iter().any(|c| c == needed) {
            return Err(format!("the release's signed list has no {needed}").into());
        }
    }
    Ok(copied)
}

// ---- Running a program ---------------------------------------------------------

/// A program that runs, with what it says gathered as it says it.
struct Running {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
    began: Instant,
}

impl Running {
    fn start(mut command: Command) -> Res<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        for out in [
            child
                .stdout
                .take()
                .map(|o| Box::new(o) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|e| Box::new(e) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let lines = lines.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    lines.lock().unwrap_or_else(|e| e.into_inner()).push(line);
                }
            });
        }
        Ok(Running {
            child,
            lines,
            began: Instant::now(),
        })
    }

    /// Wait for it, `tick` every quarter of a second with the seconds it
    /// has run; `limit` ends it. The exit status, or `None` when it was
    /// ended for the limit.
    fn wait(&mut self, limit: Option<Duration>, tick: &mut dyn FnMut(u64)) -> Option<bool> {
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                // What it said last is still being read
                std::thread::sleep(Duration::from_millis(50));
                return Some(status.success());
            }
            if limit.is_some_and(|l| self.began.elapsed() > l) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return None;
            }
            tick(self.began.elapsed().as_secs());
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn said(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The last few lines a tool said, for the screen that says it failed.
fn tail(lines: &[String], n: usize) -> Vec<String> {
    let kept: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    kept[kept.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.trim().chars().take(300).collect())
        .collect()
}

// ---- The record ----------------------------------------------------------------

/// How far the steps after the write got, for one phone. Kept as JSON,
/// mode 0600, in a folder only the owner can read.
#[derive(Debug, Clone, Default)]
pub struct Record {
    map: Map<String, Value>,
}

impl Record {
    fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).and_then(Value::as_str)
    }
    fn done(&self, key: &str) -> bool {
        self.map
            .get(key)
            .is_some_and(|v| v.as_bool() == Some(true) || v.is_string())
    }
    pub fn release(&self) -> Option<&str> {
        self.get("release")
    }
    pub fn finished(&self) -> bool {
        self.done("finished")
    }
    pub fn started(&self) -> &str {
        self.get("started").unwrap_or("earlier")
    }
}

fn now_utc() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    // Civil from days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let date = format!("{y:04}-{m:02}-{d:02}");
    let time = format!("{:02}:{:02}", (secs % 86400) / 3600, (secs % 3600) / 60);
    (date, time)
}

/// Where a codename's record is.
pub fn record_path(seams: &Seams, codename: &str) -> PathBuf {
    seams.state.join(codename).join("after.json")
}

/// The record for a codename, if there is one this tool can read.
pub fn load_record(seams: &Seams, codename: &str) -> Option<Record> {
    let bytes = fs::read(record_path(seams, codename)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(map)) if map.get("format") == Some(&json!(1)) => Some(Record { map }),
        _ => None,
    }
}

pub fn forget_record(seams: &Seams, codename: &str) -> Res<()> {
    match fs::remove_file(record_path(seams, codename)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// What the write leaves for the steps after it.
pub struct Written {
    pub codename: String,
    pub release: String,
    pub device_name: String,
    /// The digest of the phone the write was pinned to.
    pub phone: String,
    pub factory_zip: PathBuf,
    pub factory_sha256: String,
}

// ---- The steps -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// The core's last screen, with the way on.
    Installed,
    /// An unfinished record was found when the tool started.
    Resume,
    Network,
    FirstStart,
    Backup,
    Firmware,
    Restart,
    Ready,
    /// The phone that answers is not the one that was written, or tells no
    /// serial: nothing more is done with it.
    Stopped {
        title: String,
        body: Vec<String>,
        retry: Retry,
    },
    /// The owner chose to start a new install: the record is gone.
    Forgotten,
}

pub struct After {
    seams: Seams,
    key: Key,
    record: Record,
    codename: String,
    device_name: String,
    release_dir: PathBuf,
    release: VerifiedRelease,
    /// The core's last screen, shown first after a write.
    installed: Option<Screen>,
    step: Step,
    /// Why what was last tried did not go through; the step is still open.
    problem: Option<String>,
    /// What the tool that last failed said last.
    said: Vec<String>,
    /// The NetworkManager command, shown for the owner to run.
    show_command: bool,
    /// The check's rows, for the last screen.
    rows: Vec<String>,
    tools: Option<PathBuf>,
    /// The phone's last answer refused this computer's key.
    refused: bool,
}

/// One answer of the owner's on a screen of these steps.
const BEGIN: &str = "begin";
const GO_ON: &str = "go-on";
const FORGET: &str = "forget";
const ADD: &str = "add";
const MYSELF: &str = "myself";
const WAIT: &str = "wait";
const BACKUP: &str = "backup";
const FIRMWARE: &str = "firmware";
const RESTART: &str = "restart";

fn choice(answer: &'static str, label: &str) -> ScreenChoice {
    ScreenChoice {
        answer,
        label: label.into(),
    }
}

fn blank(state: &'static str, title: &str) -> Screen {
    Screen {
        state,
        stop: None,
        retry: None,
        title: title.into(),
        notice: None,
        body: Vec::new(),
        quote: None,
        steps: Vec::new(),
        link: None,
        file: None,
        problem: None,
        plan: Vec::new(),
        footer: Vec::new(),
        choices: Vec::new(),
        action: "after",
    }
}

impl After {
    /// After the write: the record is begun (an earlier one for this phone
    /// model is replaced), and the core's last screen is shown with the way
    /// on.
    pub fn after_write(
        seams: Seams,
        key: Key,
        release: VerifiedRelease,
        release_dir: PathBuf,
        installed: Screen,
        written: Written,
    ) -> Res<Self> {
        let (date, time) = now_utc();
        let map = json!({
            "format": 1,
            "codename": written.codename,
            "release": written.release,
            "phone": written.phone,
            "key": key.private.display().to_string(),
            "factory_zip": written.factory_zip.display().to_string(),
            "factory_sha256": written.factory_sha256,
            "started": format!("{date} {time} UTC"),
        });
        let Value::Object(map) = map else {
            unreachable!()
        };
        let after = After {
            seams,
            key,
            record: Record { map },
            codename: written.codename,
            device_name: written.device_name,
            release_dir,
            release,
            installed: Some(installed),
            step: Step::Installed,
            problem: None,
            said: Vec::new(),
            show_command: false,
            rows: Vec::new(),
            tools: None,
            refused: false,
        };
        after.save()?;
        Ok(after)
    }

    /// A record of a setup that did not finish, found when the tool started.
    pub fn resume(
        seams: Seams,
        key: Key,
        release: VerifiedRelease,
        release_dir: PathBuf,
        record: Record,
    ) -> Self {
        let m = release.manifest();
        After {
            codename: m.codename.clone(),
            device_name: m.device_name.clone().unwrap_or_else(|| m.codename.clone()),
            seams,
            key,
            record,
            release_dir,
            release,
            installed: None,
            step: Step::Resume,
            problem: None,
            said: Vec::new(),
            show_command: false,
            rows: Vec::new(),
            tools: None,
            refused: false,
        }
    }

    /// The owner chose to start a new install: the json mode shows the
    /// flow's first screen again.
    pub fn forgotten(&self) -> bool {
        self.step == Step::Forgotten
    }

    fn save(&self) -> Res<()> {
        let path = record_path(&self.seams, &self.codename);
        if let Some(dir) = path.parent() {
            private_dir(dir)?;
        }
        let mut text = serde_json::to_vec_pretty(&Value::Object(self.record.map.clone()))?;
        text.push(b'\n');
        private_file(&path, &text)
    }

    fn put(&mut self, key: &str, value: Value) -> Res<()> {
        self.record.map.insert(key.into(), value);
        self.save()
    }

    /// The first step the record has not done.
    fn next_step(&self) -> Step {
        let r = &self.record;
        if !r.done("network") {
            Step::Network
        } else if !r.done("first_start") {
            Step::FirstStart
        } else if !r.done("backup") {
            Step::Backup
        } else if !r.done("provision") {
            Step::Firmware
        } else if !r.done("check") {
            Step::Restart
        } else {
            Step::Ready
        }
    }

    fn backup_dir(&self) -> PathBuf {
        match self.record.get("backup_dir") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let (date, time) = now_utc();
                self.seams.home.join("omamobile-backup").join(format!(
                    "{}-radio-backup-{}-{}",
                    self.codename,
                    date.replace('-', ""),
                    time.replace(':', "")
                ))
            }
        }
    }

    fn nm_command(&self) -> String {
        format!("nmcli {}", NM_ADD.join(" "))
    }

    // ---- The screens ----------------------------------------------------------

    pub fn screen(&self) -> Screen {
        let name = &self.device_name;
        let mut s = match &self.step {
            Step::Installed => {
                let mut s = self
                    .installed
                    .clone()
                    .unwrap_or_else(|| blank("installed", ""));
                s.action = "after";
                s.body.push(if self.key.made {
                    format!("This computer had no SSH key, so the installer made one for itself ({}). It went to the phone with your answers, for root and your account, and into slot B's initramfs for its rescue SSH. The steps that follow use it to talk to the phone.", self.key.private.display())
                } else {
                    format!("This computer's SSH key ({}.pub) went to the phone with your answers, for root and your account, and into slot B's initramfs for its rescue SSH. The steps that follow use it to talk to the phone.", self.key.private.display())
                });
                s.choices = vec![choice(
                    BEGIN,
                    "Set up the phone: its USB connection, radio backup and firmware",
                )];
                s
            }
            Step::Resume if self.record.release() != Some(&self.release.manifest().version) => {
                let mut s = blank(
                    "after-resume",
                    "An install on this computer is not finished",
                );
                s.body = vec![
                    format!(
                        "Omarchy Mobile {} was written to a {name} from this computer ({}), and the steps after the write did not all run: {}.",
                        self.record.release().unwrap_or("of another release"),
                        self.record.started(),
                        self.left().join(", ")
                    ),
                    format!(
                        "This is the installer for release {}. Those steps use the tools of the release that was written: to go on with them, open the installer with that release. Or start a new install with this one. That forgets how far the other one got; the radio backup, if one was taken, stays in its folder.",
                        self.release.manifest().version
                    ),
                ];
                s.choices = vec![choice(FORGET, "Forget it, and start a new install")];
                s
            }
            Step::Resume => {
                let mut s = blank(
                    "after-resume",
                    "An install on this computer is not finished",
                );
                s.body = vec![
                    format!(
                        "Omarchy Mobile {} was written to a {name} from this computer ({}), and the steps after the write did not all run: {}.",
                        self.record.release().unwrap_or("of another release"),
                        self.record.started(),
                        self.left().join(", ")
                    ),
                    "You can go on with them now, from the step that did not finish. The phone must be the one that was written: the installer asks it which phone it is before it does anything with it.".into(),
                    "Or start a new install. That forgets how far this one got; the radio backup, if one was taken, stays in its folder.".into(),
                ];
                s.choices = vec![
                    choice(GO_ON, "Go on with this phone's setup"),
                    choice(FORGET, "Forget it, and start a new install"),
                ];
                s
            }
            Step::Network => {
                let mut s = blank("network", "The USB connection to the phone");
                s.body = vec![
                    "The rest of the install talks to the phone over the USB cable, as a small network between this computer and the phone. This computer needs one saved connection for it.".into(),
                    "What it does: it gives this computer the address 172.16.42.2 on the phone's USB link, and that is all. This computer's internet connection is not shared over it, and none of this computer's own traffic goes through the phone.".into(),
                    "It is matched by its driver (cdc_ncm), because nothing more is known of the phone's USB network before the phone has started. Another USB network device with no connection of its own (the network port of some USB-C docks, another phone sharing its internet) would be given this one too, and get the address above and no internet. Its priority is below every connection you have saved, so where one of yours fits, yours is used.".into(),
                    format!("It stays saved after the install. To remove it later: nmcli connection delete {NM_PROFILE}"),
                ];
                s.steps = vec![ScreenStep {
                    text: if self.show_command {
                        "Run this in a terminal, then choose \"I have added it\":".into()
                    } else {
                        "The installer adds it with this command, when you choose \"Add the connection\":".into()
                    },
                    command: Some(self.nm_command()),
                }];
                s.choices = vec![
                    choice(
                        ADD,
                        if self.show_command {
                            "Try adding it again"
                        } else {
                            "Add the connection"
                        },
                    ),
                    choice(
                        MYSELF,
                        if self.show_command {
                            "I have added it"
                        } else {
                            "I will set it up myself"
                        },
                    ),
                ];
                s
            }
            Step::FirstStart => {
                let mut s = blank("first-start", "The phone's first start");
                s.body = vec![
                    "The phone is starting Omarchy Mobile for the first time. It makes the system fill the phone's storage and sets up your account, and this computer's SSH key, from your answers, which takes a few minutes. Leave the phone connected and alone.".into(),
                    format!(
                        "The installer waits for it over the USB connection, for up to {}.",
                        minutes(self.seams.waits.first_start)
                    ),
                ];
                s.choices = vec![choice(WAIT, "Wait for the first start")];
                s
            }
            Step::Backup => {
                let mut s = blank("backup", "Back up the phone's radio partitions");
                s.body = vec![
                    "Before the phone's modem ever starts, the installer copies the partitions that hold its radio calibration and identity to this computer, with the release's tools/nv-backup. This only reads from the phone. The copy goes into this folder:".into(),
                    self.backup_dir().display().to_string(),
                ];
                s.notice = Some("The backup holds the phone's IMEI and serial numbers. Keep it, and never post or share it, not even in a bug report.".into());
                s.choices = vec![choice(BACKUP, "Back up the radio partitions")];
                s
            }
            Step::Firmware => {
                let mut s = blank("firmware", "Add the phone's firmware");
                s.body = vec![
                    "Without Google's firmware the phone has no Wi-Fi, Bluetooth, sound, haptics, sensors, modem, camera or fingerprint. The release's tools/phone-provision copies it out of the factory image this install checked, on this computer, and puts it on the phone with the phone's own modem files from the radio backup. This takes several minutes.".into(),
                    format!(
                        "The factory image: {}",
                        self.record.get("factory_zip").unwrap_or("not recorded")
                    ),
                    "The cameras' data needs two Python modules on this computer (unicorn and pyelftools, in ~/.cache/omamobile-venv). This release does not carry their pinned list, so the installer does not fetch them: without them everything else works, and the cameras don't.".into(),
                ];
                s.choices = vec![choice(FIRMWARE, "Add the firmware")];
                s
            }
            Step::Restart => {
                let mut s = blank("restart", "Restart the phone and check it");
                s.body = vec![
                    "The firmware is on the phone. The installer now restarts the phone, so that it starts with it: its screen goes dark for a minute. Then it checks the phone and shows what works.".into(),
                ];
                if self.record.get("restart_from").is_some() {
                    s.body.push("The restart was already sent; the installer waits for the phone to come back and does not send another.".into());
                }
                s.choices = vec![choice(
                    RESTART,
                    if self.record.get("restart_from").is_some() {
                        "Wait for the phone"
                    } else {
                        "Restart the phone and check it"
                    },
                )];
                s
            }
            Step::Ready => {
                let failed = self.record.get("check") == Some("failed");
                let mut s = blank(
                    "ready",
                    if failed {
                        "The phone is set up, and a check it needs did not pass"
                    } else {
                        "The phone is set up"
                    },
                );
                s.action = "none";
                s.body = self.rows.clone();
                if s.body.is_empty() {
                    s.body
                        .push("The check's results are in the report below.".into());
                }
                s.footer = vec![
                    format!(
                        "The radio backup is in {}: keep it, and never share it.",
                        self.record.get("backup_dir").unwrap_or("its folder")
                    ),
                    format!(
                        "The phone's report is in {}. It is made to name no phone, but its \"new dmesg errors\" row quotes the phone's own messages: read it before you post the file anywhere.",
                        self.seams.state.join(&self.codename).join("report.json").display()
                    ),
                    format!("The USB connection stays saved on this computer. To remove it: nmcli connection delete {NM_PROFILE}"),
                    "The phone updates itself from its own menu (Update), or with: omarchy update".into(),
                ];
                s
            }
            Step::Stopped { title, body, retry } => {
                let mut s = blank("stopped", title);
                s.stop = Some("after-other-phone");
                s.retry = Some(*retry);
                s.action = "none";
                s.body = body.clone();
                s.footer = vec![
                    "Omarchy Mobile is written to the phone, and what the setup has done so far stays done. Starting the installer again goes on from here.".into(),
                ];
                s
            }
            Step::Forgotten => blank("after-resume", "Forgotten"),
        };
        if let Some(problem) = &self.problem {
            s.problem = Some(problem.clone());
        }
        if !self.said.is_empty() && s.problem.is_some() {
            s.plan = self.said.clone();
        }
        s.fit_to_print();
        s
    }

    fn left(&self) -> Vec<&'static str> {
        let r = &self.record;
        [
            ("network", "the USB connection"),
            ("first_start", "the first start"),
            ("backup", "the radio backup"),
            ("provision", "the firmware"),
            ("check", "the restart and the check"),
        ]
        .into_iter()
        .filter(|(k, _)| !r.done(k))
        .map(|(_, w)| w)
        .collect()
    }

    // ---- The steps --------------------------------------------------------------

    /// One answer of the owner's. Refused, with nothing done, unless the
    /// screen on show offers it.
    pub fn act(&mut self, answer: &str, watch: &mut dyn Watch) -> Res<()> {
        let offered = self.screen();
        if offered.action != "after" || !offered.choices.iter().any(|c| c.answer == answer) {
            let names: Vec<&str> = offered.choices.iter().map(|c| c.answer).collect();
            return Err(if names.is_empty() {
                "This screen offers no choice.".to_string()
            } else {
                format!("after takes \"answer\": one of {}.", names.join(", "))
            }
            .into());
        }
        self.problem = None;
        self.said.clear();
        match (self.step.clone(), answer) {
            (Step::Resume, FORGET) => {
                forget_record(&self.seams, &self.codename)?;
                self.step = Step::Forgotten;
            }
            (Step::Resume, GO_ON) => {
                self.step = self.next_step();
                if self.step == Step::Network {
                    self.network_first(watch)?;
                }
            }
            (Step::Installed, BEGIN) => {
                self.step = Step::Network;
                self.network_first(watch)?;
            }
            (Step::Network, ADD) => self.add_connection(watch)?,
            (Step::Network, MYSELF) => {
                self.put("network", json!("set up by the owner"))?;
                self.step = Step::FirstStart;
                self.first_start(watch)?;
            }
            (Step::FirstStart, WAIT) => self.first_start(watch)?,
            (Step::Backup, BACKUP) => self.backup(watch)?,
            (Step::Firmware, FIRMWARE) => self.firmware(watch)?,
            (Step::Restart, RESTART) => self.restart_and_check(watch)?,
            _ => {}
        }
        Ok(())
    }

    /// Ask the phone a question over USB SSH, with the release's
    /// `tools/phone-ssh` and this computer's key. `None`: no answer (no
    /// connection, a refused key, a command that failed).
    fn phone(&mut self, command: &str) -> Option<String> {
        let tools = self.tools.clone()?;
        let mut c = Command::new("bash");
        c.arg(tools.join("tools/phone-ssh"))
            .arg("--fresh")
            .arg(command)
            .env("OMAMOBILE_SSH_KEY", &self.key.private);
        self.refused = false;
        let mut running = Running::start(c).ok()?;
        let ok = running.wait(Some(Duration::from_secs(60)), &mut |_| {})?;
        let said = running.said();
        if said.iter().any(|l| l.contains("Permission denied")) {
            self.refused = true;
            self.problem = Some(format!(
                "The phone answers at {PHONE_ADDR}, and does not take this computer's SSH key ({}.pub). That key went to the phone with your answers, and its first start set it up for root; the phone no longer takes it, so it was changed on the phone, or this computer's key file is not the one this install used. Put that key file back, or add this computer's key to /root/.ssh/authorized_keys on the phone, and try again.",
                self.key.private.display()
            ));
        }
        ok.then(|| said.join("\n"))
    }

    fn answers(&mut self) -> bool {
        self.phone("true").is_some()
    }

    /// The tools, checked and copied, once per run.
    fn tools_ready(&mut self) -> Res<()> {
        if self.tools.is_none() {
            let to = self.seams.state.join(&self.codename).join("tools-copy");
            copy_tools(&self.release, &self.release_dir, &to).map_err(|e| {
                format!("The release's tools can't be used: {e}. Fetch the release again.")
            })?;
            self.tools = Some(to);
        }
        Ok(())
    }

    /// Goes on only when the phone that answers is the one the write was
    /// pinned to. `false`: the step is stopped or stays open, and says why.
    fn written_phone(&mut self) -> bool {
        let Some(id) = self.phone(&ask::phone_id()) else {
            if self.problem.is_none() {
                self.problem = Some("The phone doesn't answer over the USB cable. Unplug it, plug it back in, wait ten seconds and try again.".into());
            }
            return false;
        };
        let id: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if id == "no-serial" {
            self.step = Step::Stopped {
                title: "The phone doesn't tell its serial number".into(),
                body: vec![
                    "The phone that answers over the USB cable doesn't tell its serial number, so the installer can't be sure it is the phone it wrote, and it won't back up, change or restart a phone it can't be sure of. Nothing was read from it but that.".into(),
                    "Please open an issue: https://github.com/derekross/omarchy-mobile/issues".into(),
                ],
                retry: Retry::StopsAgain,
            };
            return false;
        }
        if Some(id.as_str()) != self.record.get("phone") {
            self.step = Step::Stopped {
                title: "Another phone answers over the USB cable".into(),
                body: vec![
                    "The phone that answers over the USB cable is not the phone this install wrote. It was asked which phone it is, and nothing else: nothing was copied from it, put on it or restarted.".into(),
                    format!("Unplug every other phone that runs Omarchy Mobile, connect the {} this install is for, and start the installer again.", self.device_name),
                ],
                retry: Retry::AfterFix,
            };
            return false;
        }
        true
    }

    fn connection_saved(&self) -> Option<bool> {
        let out = Command::new(&self.seams.nmcli)
            .args(["-t", "-f", "NAME", "connection", "show"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l == NM_PROFILE),
        )
    }

    /// The USB connection, before anything is added: a phone that already
    /// answers, or a connection that is already saved, needs nothing.
    fn network_first(&mut self, watch: &mut dyn Watch) -> Res<()> {
        self.tools_ready()?;
        if self.answers() {
            self.put("network", json!("already working"))?;
        } else if self.connection_saved() == Some(true) {
            self.put("network", json!("already saved"))?;
        } else {
            self.problem = None;
            self.show_command = self.connection_saved().is_none();
            if self.show_command {
                self.problem = Some("This computer doesn't run NetworkManager (nmcli), so the installer can't add the connection. Give this computer the address 172.16.42.2/24 on the phone's USB network interface with your system's own network settings, or run the command below where NetworkManager is.".into());
            }
            return Ok(());
        }
        self.step = Step::FirstStart;
        self.first_start(watch)
    }

    fn add_connection(&mut self, watch: &mut dyn Watch) -> Res<()> {
        watch.note(&format!("adding the connection: {}", self.nm_command()));
        let out = Command::new(&self.seams.nmcli)
            .args(NM_ADD)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(out) if out.status.success() => {
                self.put("network", json!(format!("added {NM_PROFILE}")))?;
                self.step = Step::FirstStart;
                self.first_start(watch)
            }
            other => {
                let why = match other {
                    Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                    Err(e) => e.to_string(),
                };
                self.show_command = true;
                self.problem = Some(format!(
                    "NetworkManager didn't add the connection ({}). Adding a connection for every user of this computer may need an administrator's permission. Run the command below in a terminal, then choose \"I have added it\".",
                    if why.is_empty() { "no reason given".into() } else { why.chars().take(200).collect::<String>() }
                ));
                Ok(())
            }
        }
    }

    fn first_start(&mut self, watch: &mut dyn Watch) -> Res<()> {
        self.tools_ready()?;
        let began = Instant::now();
        let limit = self.seams.waits.first_start;
        self.problem = None;
        loop {
            if self.phone(ask::FIRST_START_DONE).is_some() {
                break;
            }
            // The phone's system takes this computer's key only once its
            // first start has set it up from the answers, early on
            // (omamobile-firstboot's "SSH keys" step, after the keyring,
            // the host name and the account): until then a refused
            // key is the first start still running
            let refused = self.refused;
            self.problem = None;
            let waited = began.elapsed().as_secs();
            watch.progress("first-start", waited.min(limit), limit);
            if waited >= limit && refused {
                self.problem = Some(format!(
                    "The phone answers at {PHONE_ADDR}, and after {} it still does not take this computer's SSH key ({}.pub). The key went to the phone with your answers, and the phone's first start sets it up for root early on. If the phone shows its lock screen, its first start has finished, so this computer's key file is not the one this install used: put that one back and wait again. If the phone's screen is dark or stuck, its first start stopped before it set up the key: hold its power key to restart it, and a first start that stopped tries again.",
                    minutes(limit),
                    self.key.private.display()
                ));
                return Ok(());
            }
            if waited >= limit {
                self.problem = Some(format!(
                    "The phone hasn't finished its first start after {}. If it shows the lock screen, unplug the cable, plug it back in, wait ten seconds and wait again. If it still doesn't answer, another of this computer's saved connections may have taken the phone's USB link: \"nmcli device\" shows which connection each link uses. If the phone's screen is dark or stuck, hold its power key to restart it.",
                    minutes(limit)
                ));
                return Ok(());
            }
            if !watch.go_on("first-start") {
                return Ok(());
            }
            std::thread::sleep(self.seams.waits.poll);
        }
        if !self.written_phone() {
            return Ok(());
        }
        watch.note("the first start has finished");
        self.put("first_start", json!(true))?;
        self.step = Step::Backup;
        Ok(())
    }

    /// Run one of the release's tools, with progress, never cut off: what
    /// it says goes to standard error as it says it.
    fn run_tool(
        &mut self,
        what: &'static str,
        tool: &str,
        args: &[&std::ffi::OsStr],
        watch: &mut dyn Watch,
    ) -> Res<bool> {
        let tools = self
            .tools
            .clone()
            .ok_or("the release's tools were not checked")?;
        let mut c = Command::new("bash");
        c.arg(tools.join(tool))
            .args(args)
            .env("OMAMOBILE_SSH_KEY", &self.key.private);
        let mut running = Running::start(c)?;
        let lines = running.lines.clone();
        let mut shown = 0;
        let mut pass_on = |watch: &mut dyn Watch| {
            let said = lines.lock().unwrap_or_else(|e| e.into_inner());
            for line in &said[shown..] {
                watch.note(line);
            }
            shown = said.len();
        };
        let ok = running
            .wait(None, &mut |secs| {
                watch.progress(what, secs, 0);
                pass_on(&mut *watch);
            })
            .unwrap_or(false);
        pass_on(watch);
        if !ok {
            self.said = tail(&running.said(), 12);
        }
        Ok(ok)
    }

    fn backup(&mut self, watch: &mut dyn Watch) -> Res<()> {
        self.tools_ready()?;
        if !self.written_phone() {
            return Ok(());
        }
        let dir = self.backup_dir();
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.run_tool("backup", "tools/nv-backup", &[dir.as_os_str()], watch)? {
            self.put("backup_dir", json!(dir.display().to_string()))?;
            self.put("backup", json!(true))?;
            self.step = Step::Firmware;
        } else {
            self.problem = Some("The backup didn't finish (the tool's last words are below), and what it had copied was removed. If they are about a partition (not found, writable or mounted), don't work around it: ask in an issue. Otherwise try again.".into());
        }
        Ok(())
    }

    fn firmware(&mut self, watch: &mut dyn Watch) -> Res<()> {
        self.tools_ready()?;
        let zip = PathBuf::from(self.record.get("factory_zip").unwrap_or_default());
        let want = self
            .record
            .get("factory_sha256")
            .unwrap_or_default()
            .to_string();
        // Checked when it was taken, perhaps days ago: the bytes about to be
        // unpacked are checked again
        let Ok(mut file) = File::open(&zip) else {
            self.problem = Some(format!("The factory image ({}) is gone. Put it back where it was, or start a new install, which asks for it again.", zip.display()));
            return Ok(());
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut hasher = StreamHasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            watch.progress("check", hasher.len(), size);
        }
        if hasher.finish().0 != want {
            self.problem = Some(format!("The factory image {} changed since it was checked, so it is not used. Delete it and start a new install: the installer asks for it again.", zip.display()));
            return Ok(());
        }
        if !self.written_phone() {
            return Ok(());
        }
        let backup = PathBuf::from(self.record.get("backup_dir").unwrap_or_default());
        let args = [
            std::ffi::OsStr::new("--first-install"),
            std::ffi::OsStr::new("--nv-backup"),
            backup.as_os_str(),
            zip.as_os_str(),
        ];
        if self.run_tool("firmware", "tools/phone-provision", &args, watch)? {
            self.put("provision", json!(true))?;
            self.step = Step::Restart;
        } else {
            self.problem = Some("Adding the firmware stopped (the tool's last words are below). Each part is put on the phone whole or not at all, and trying again does all of it again.".into());
        }
        Ok(())
    }

    fn boot_id(&mut self) -> Option<String> {
        self.phone(ask::BOOT_ID)
            .map(|s| {
                s.chars()
                    .filter(|c| c.is_ascii_hexdigit() || *c == '-')
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
    }

    fn restart_and_check(&mut self, watch: &mut dyn Watch) -> Res<()> {
        self.tools_ready()?;
        // Before the restart is sent, and before one sent earlier is taken
        // as finished: another phone has another boot id
        if !self.written_phone() {
            return Ok(());
        }
        let before = match self.record.get("restart_from") {
            Some(b) => b.to_string(),
            None => {
                let Some(before) = self.boot_id() else {
                    self.problem = Some("The phone doesn't answer over the USB cable. Unplug it, plug it back in, wait ten seconds and try again.".into());
                    return Ok(());
                };
                // Recorded before it is sent, so a second try waits for this
                // restart and sends no other
                self.put("restart_from", json!(before))?;
                watch.note("restarting the phone");
                if self.phone(ask::RESTART).is_none() {
                    self.record.map.remove("restart_from");
                    self.save()?;
                    self.problem = Some("The phone didn't take the restart. Try again.".into());
                    return Ok(());
                }
                before
            }
        };
        let began = Instant::now();
        let limit = self.seams.waits.restart;
        loop {
            std::thread::sleep(self.seams.waits.poll);
            if self.boot_id().is_some_and(|now| now != before) {
                break;
            }
            let waited = began.elapsed().as_secs();
            watch.progress("restart", waited.min(limit), limit);
            if waited >= limit {
                self.problem = Some(format!("The phone hasn't come back {} after its restart. Wait a minute more: a stuck restart resets itself. If it is still stuck, hold its power key to restart it, then wait again.", minutes(limit)));
                return Ok(());
            }
            if !watch.go_on("restart") {
                return Ok(());
            }
        }
        if !self.written_phone() {
            return Ok(());
        }
        self.put("restart", json!(true))?;
        // Sound, sensors and radios come up over the first seconds
        let began = Instant::now();
        while began.elapsed().as_secs() < self.seams.waits.settle {
            match self.phone(ask::SYSTEM).as_deref().map(str::trim) {
                Some("running") | Some("degraded") => break,
                _ => {}
            }
            watch.progress("check", 0, 0);
            std::thread::sleep(self.seams.waits.poll);
        }
        self.check(watch)
    }

    fn check(&mut self, watch: &mut dyn Watch) -> Res<()> {
        let report_path = self.seams.state.join(&self.codename).join("report.json");
        let has_smoke = self.tools.as_ref().is_some_and(|t| t.join(SMOKE).is_file());
        let mut rows = Vec::new();
        let mut needed_failed = false;
        if has_smoke {
            let tools = self.tools.clone().unwrap_or_default();
            let mut c = Command::new("bash");
            c.arg(tools.join(SMOKE))
                .arg("--json")
                .env("OMAMOBILE_SSH_KEY", &self.key.private)
                .stdin(Stdio::null())
                .stderr(Stdio::piped());
            let out = c.output()?;
            private_file(&report_path, &out.stdout)?;
            for line in String::from_utf8_lossy(&out.stderr).lines() {
                watch.note(line);
            }
            let checks = serde_json::from_slice::<Value>(&out.stdout)
                .ok()
                .and_then(|v| v.get("checks").and_then(Value::as_array).cloned());
            let Some(checks) = checks else {
                self.problem = Some("The phone didn't answer the check. Unplug the cable, plug it back in, wait ten seconds and try again.".into());
                return Ok(());
            };
            let (mut works, mut not_checked) = (Vec::new(), Vec::new());
            let mut broken = Vec::new();
            for c in &checks {
                let name = c.get("name").and_then(Value::as_str).unwrap_or("?");
                let label = plain_name(name);
                let detail = c.get("detail").and_then(Value::as_str).unwrap_or("");
                match c.get("status").and_then(Value::as_str) {
                    Some("ok") => works.push(label),
                    Some("skip") => not_checked.push(format!("{label} ({detail})")),
                    _ => {
                        let needed = c.get("required").and_then(Value::as_bool) == Some(true);
                        needed_failed |= needed;
                        broken.push(format!(
                            "{label} ({detail}){}",
                            if needed { ", needed" } else { "" }
                        ));
                    }
                }
            }
            if !works.is_empty() {
                rows.push(format!("Works: {}.", works.join(", ")));
            }
            if !broken.is_empty() {
                rows.push(format!("Not working: {}.", broken.join("; ")));
                rows.push(format!("A line under \"Not working\" is either something this release doesn't do yet on the {}, or a fault. The list of what isn't done yet: https://omamobile.org/devices/{}", self.device_name, self.codename));
            }
            if !not_checked.is_empty() {
                rows.push(format!("Not checked: {}.", not_checked.join("; ")));
            }
            if !out.status.success() && !needed_failed {
                needed_failed = true;
            }
        } else {
            // The release ships no phone-smoke: what systemd says, and no more
            let state = self.phone(ask::SYSTEM).unwrap_or_default();
            let state = state.trim();
            private_file(
                &report_path,
                format!("{}\n", json!({"system": state})).as_bytes(),
            )?;
            rows.push(format!(
                "The phone started from slot B and its system says \"{}\".",
                if state.is_empty() { "nothing" } else { state }
            ));
            rows.push("This release does not carry the full check (tools/phone-smoke), so the installer checked no more than that.".into());
            needed_failed = !(state == "running" || state == "degraded");
        }
        self.rows = rows;
        self.put(
            "check",
            json!(if needed_failed { "failed" } else { "passed" }),
        )?;
        let (date, _) = now_utc();
        self.put("finished", json!(date))?;
        self.step = Step::Ready;
        Ok(())
    }
}

/// Seconds as whole minutes, in words.
fn minutes(seconds: u64) -> String {
    match seconds.div_ceil(60) {
        1 => "1 minute".into(),
        n => format!("{n} minutes"),
    }
}

/// phone-smoke's names for its checks, in install-guided's plain words.
fn plain_name(name: &str) -> String {
    match name {
        "boot" => "Starts its own system (slot B)",
        "storage guard" => "Radio and Android partitions are protected",
        "root" => "System is on the phone's storage",
        "kernel health" => "No serious errors since it started",
        "USB gadget" => "USB connection",
        "session" => "Desktop is running",
        "display" => "Display",
        "touch" => "Touch",
        "buttons" => "Buttons",
        "battery" => "Battery",
        "sound cards" => "Sound",
        "sensors" => "Sensors",
        "cameras" => "Cameras",
        "lock" => "Lock screen",
        "systemd units" => "Background services",
        "thermal" => "Temperature sensors",
        "new dmesg errors" => "New system messages",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connection_is_install_guideds_word_for_word() {
        assert_eq!(
            format!("nmcli {}", NM_ADD.join(" ")),
            "nmcli connection add type ethernet con-name omamobile-phone match.driver cdc_ncm ipv4.method manual ipv4.addresses 172.16.42.2/24 ipv4.never-default yes ipv6.method disabled connection.autoconnect-priority -100"
        );
    }

    #[test]
    fn the_phones_digest_is_worked_out_as_install_guided_works_it_out() {
        let script = ask::phone_id();
        assert!(script.contains("/proc/bootconfig") && script.contains("/proc/cmdline"));
        assert!(script.contains("/proc/device-tree/serial-number"));
        assert!(script.contains("sha256sum | cut -d\" \" -f1; else echo no-serial"));
    }

    #[test]
    fn a_date_is_the_calendars() {
        let (date, time) = now_utc();
        assert_eq!(date.len(), 10);
        assert_eq!(time.len(), 5);
    }
}
