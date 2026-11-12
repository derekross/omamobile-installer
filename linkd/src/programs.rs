// SPDX-License-Identifier: MIT
//! The fixed programs the daemon runs (INSTALL-LINK-PLAN §6.1), and
//! systemd. No message names a program or an argument: every command line
//! here is fixed, and what varies is a path of the daemon's own.
use crate::paths::Root;
use omamobile_link::answers::{parse_porcelain, Problem};
use omamobile_link::json::{self, Value};
use std::io;
use std::process::{Command, Stdio};

pub const FIRSTBOOT_UNIT: &str = "omamobile-firstboot.service";
pub const PROVISION_UNIT: &str = "omamobile-provision-run.service";
pub const DISPLAY_MANAGER: &str = "display-manager.service";

/// What the provisioning worker may write (omamobile-provision's README,
/// "For the daemon's unit"), beside its result file and the progress file.
const PROVISION_WRITES: &[&str] = &[
    "/usr/lib/firmware",
    "/usr/share/omamobile/sensors",
    "/usr/lib/omamobile-vendor/gnss",
    "/usr/lib/omamobile-vendor/modem",
    "/usr/lib/omamobile-fp",
    "/usr/lib/omamobile-provision",
    "/var/lib/omamobile-vendor/modem",
    "/etc/omamobile",
    "/var/lib/omamobile/provision",
    "/var/lib/omamobile/install-link",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitState {
    /// Not started in this boot, or ended well and gone.
    Inactive,
    Running,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Programs {
    root: Root,
}

impl Programs {
    pub fn new(root: &Root) -> Programs {
        Programs { root: root.clone() }
    }

    fn cmd(&self, name: &str) -> Command {
        let mut c = Command::new(self.root.program(name));
        c.stdin(Stdio::null());
        if self.root.scratch() {
            // The stand-ins of a scratch root, and the real first-boot
            // script, look here for the root
            c.env("OMAMOBILE_LINK_ROOT", self.root.dir());
            c.env("OMAMOBILE_FIRSTBOOT_ROOT", self.root.dir());
        }
        c
    }

    fn show(&self, unit: &str, property: &str) -> Option<String> {
        let out = self
            .cmd("systemctl")
            .args(["show", "--property", property, "--value", unit])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// A unit in this boot. `None`: systemd did not answer.
    pub fn unit_state(&self, unit: &str) -> Option<UnitState> {
        Some(match self.show(unit, "ActiveState")?.as_str() {
            "failed" => UnitState::Failed,
            "active" | "activating" | "reloading" | "deactivating" => UnitState::Running,
            _ => UnitState::Inactive,
        })
    }

    /// When the unit became active, in microseconds of the monotonic clock;
    /// `None` while it is not.
    pub fn active_since_us(&self, unit: &str) -> Option<u64> {
        self.show(unit, "ActiveEnterTimestampMonotonic")?
            .parse::<u64>()
            .ok()
            .filter(|t| *t > 0)
    }

    fn run(mut c: Command) -> io::Result<()> {
        let status = c.stdout(Stdio::null()).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("exit {status}")))
        }
    }

    /// Start a unit and do not wait for it.
    pub fn start_unit(&self, unit: &str) -> io::Result<()> {
        let mut c = self.cmd("systemctl");
        c.args(["start", "--no-block", unit]);
        Programs::run(c)
    }

    /// `systemctl reboot`, never `reboot-to`: the units' stops run (§5.6).
    pub fn reboot(&self) -> io::Result<()> {
        let mut c = self.cmd("systemctl");
        c.arg("reboot");
        Programs::run(c)
    }

    /// `omamobile-firstboot --check --porcelain CANDIDATE`: exit 0 for no
    /// objection, 1 for objections, anything else is the daemon's own fault.
    pub fn check_answers(&self) -> Option<Vec<Problem>> {
        let out = self
            .cmd("omamobile-firstboot")
            .args(["--check", "--porcelain"])
            .arg(self.root.candidate())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let problems = parse_porcelain(&String::from_utf8_lossy(&out.stdout))?;
        match (out.status.code(), problems.is_empty()) {
            (Some(0), true) | (Some(1), false) => Some(problems),
            _ => None,
        }
    }

    /// The provisioning worker as the transient unit
    /// `omamobile-provision-run.service` (§6.1, §6.3): the fixed paths of
    /// the image and of the pending checksums, no network, no device.
    pub fn start_provision(&self) -> io::Result<()> {
        let mut c = self.cmd("systemd-run");
        c.args([
            "--unit=omamobile-provision-run",
            "--no-block",
            "--collect",
            "--quiet",
            "--property=ProtectSystem=strict",
            "--property=PrivateNetwork=yes",
            "--property=PrivateDevices=yes",
            "--property=CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_CHOWN CAP_FOWNER CAP_DAC_OVERRIDE",
        ]);
        let writes: Vec<String> = PROVISION_WRITES
            .iter()
            .map(|p| self.root.at(p).display().to_string())
            .collect();
        c.arg(format!("--property=ReadWritePaths={}", writes.join(" ")));
        c.arg(format!(
            "--property=StandardOutput=file:{}",
            self.root.progress().display()
        ));
        c.arg(self.root.program("omamobile-provision"))
            .arg("first-install")
            .arg("--result")
            .arg(self.root.result())
            .arg(self.root.image())
            .arg(self.root.pending_sums());
        Programs::run(c)
    }

    /// `phone-smoke --here --json`: its report, as it wrote it.
    pub fn smoke(&self) -> Option<Value> {
        let out = self
            .cmd("phone-smoke")
            .args(["--here", "--json"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        json::parse(String::from_utf8_lossy(&out.stdout).trim().as_bytes()).ok()
    }

    /// `omamobile-link-gadget add|remove` (§3.3; the script is stage L3's).
    pub fn gadget(&self, what: &str) -> io::Result<()> {
        let mut c = Command::new(self.root.gadget());
        c.stdin(Stdio::null()).arg(what);
        if self.root.scratch() {
            c.env("OMAMOBILE_LINK_ROOT", self.root.dir());
        }
        Programs::run(c)
    }
}
