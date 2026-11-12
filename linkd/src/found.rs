// SPDX-License-Identifier: MIT
//! What is on the phone: the facts the start-up rules take (`Found`,
//! INSTALL-LINK-PLAN §6.5), `status`'s facts, and the radio partitions as
//! `tools/nv-backup` finds and checks them. Everything here reads; nothing
//! changes anything.
use crate::descriptor::Descriptor;
use crate::paths::{is_tmp_name, Root};
use crate::programs::{Programs, UnitState, FIRSTBOOT_UNIT, PROVISION_UNIT};
use omamobile_link::json::{self, Value};
use omamobile_link::msg::{Progress, StepResult, StepState, What};
use omamobile_link::phone::{Account, Facts, FirstbootUnit, Found, PartFound, StateFile};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

/// The steps `omamobile-firstboot` runs.
const FIRSTBOOT_STEPS: u32 = 10;

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// This boot's name: the kernel's boot id. Empty when it cannot be read.
pub fn boot_id(root: &Root) -> String {
    read_trimmed(&root.boot_id()).unwrap_or_default()
}

/// `androidboot.serialno` from `/proc/bootconfig`. Never printed.
pub fn serial(root: &Root) -> Option<Vec<u8>> {
    let text = fs::read_to_string(root.at("/proc/bootconfig")).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "androidboot.serialno")
            .then(|| v.trim().trim_matches('"').as_bytes().to_vec())
            .filter(|v| !v.is_empty())
    })
}

/// The accounts a person can have: a uid of 1000 or more (65534 is nobody).
fn accounts(root: &Root) -> Vec<(String, u32)> {
    let text = fs::read_to_string(root.passwd()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let uid: u32 = f.nth(1)?.parse().ok()?;
            (uid >= 1000 && uid != 65534).then(|| (name.to_string(), uid))
        })
        .collect()
}

/// The uid of the account first boot made: the one its `user` file names.
pub fn firstboot_uid(root: &Root) -> Option<u32> {
    let user = read_trimmed(&root.firstboot_user()).filter(|u| !u.is_empty())?;
    accounts(root)
        .into_iter()
        .find(|(name, _)| *name == user)
        .map(|(_, uid)| uid)
}

fn account(root: &Root) -> Account {
    let all = accounts(root);
    if all.is_empty() {
        return Account::None;
    }
    let user = read_trimmed(&root.firstboot_user()).unwrap_or_default();
    if all.iter().all(|(name, _)| *name == user) {
        Account::Firstboot
    } else {
        Account::Other
    }
}

/// A step's name as it may cross the link. First boot logs its account
/// step as "user NAME"; `status` names no user (§5.4), so that step is
/// "user" here.
pub fn step_name(logged: &str) -> String {
    let s = logged.trim();
    if s.starts_with("user ") && s != "user setup" {
        return "user".to_string();
    }
    s.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(64)
        .collect()
}

/// First boot's last run in its log: the steps begun, the last one's name,
/// and those marked `FAILED`.
fn firstboot_log(root: &Root) -> (u32, String, Vec<String>) {
    let text = fs::read_to_string(root.firstboot_log()).unwrap_or_default();
    let run = text
        .rfind("=== omamobile-firstboot ")
        .map_or(text.as_str(), |at| &text[at..]);
    let (mut n, mut step, mut failed) = (0u32, String::new(), Vec::new());
    for line in run.lines() {
        if let Some(name) = line.strip_prefix("--- ") {
            n += 1;
            step = step_name(name);
        } else if line.trim_start().starts_with("FAILED") && !step.is_empty() {
            failed.push(step.clone());
        }
    }
    (n, step, failed)
}

/// The steps first boot's log marks `FAILED` in its last run.
pub fn firstboot_failed(root: &Root) -> Vec<String> {
    firstboot_log(root).2
}

/// The worker's result file: each step and its state. `None`: no file. A
/// file that cannot be read as one is an empty list, which the machine
/// judges as a run that was interrupted.
pub fn provision_result(root: &Root) -> Option<Vec<StepResult>> {
    let bytes = fs::read(root.result()).ok()?;
    let steps = || -> Option<Vec<StepResult>> {
        let Value::Obj(top) = json::parse(&bytes).ok()? else {
            return None;
        };
        let Value::Arr(steps) = &top.iter().find(|(k, _)| k == "steps")?.1 else {
            return None;
        };
        steps
            .iter()
            .map(|s| {
                let Value::Obj(o) = s else { return None };
                let get = |key: &str| match &o.iter().find(|(k, _)| k == key)?.1 {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                };
                Some(StepResult {
                    step: get("step")?,
                    state: StepState::from_wire(&get("state")?)?,
                })
            })
            .collect()
    };
    Some(steps().unwrap_or_default())
}

/// The last whole progress line the worker wrote.
pub fn provision_progress(root: &Root) -> Option<Progress> {
    let text = fs::read_to_string(root.progress()).ok()?;
    let whole = &text[..text.rfind('\n')?];
    let line = whole.lines().next_back()?;
    let Value::Obj(o) = json::parse(line.as_bytes()).ok()? else {
        return None;
    };
    let get = |key: &str| o.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let Value::Str(step) = get("step")? else {
        return None;
    };
    Some(Progress {
        what: What::Provision,
        step: step_name(step),
        done: get("done")?.as_u64()?,
        total: get("total")?.as_u64()?,
    })
}

fn work_dirs(root: &Root) -> Vec<std::path::PathBuf> {
    fs::read_dir(root.work_parent())
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("work."))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

/// The extraction's working directories, for removal.
pub fn work_dirs_of(root: &Root) -> Vec<std::path::PathBuf> {
    work_dirs(root)
}

/// The daemon's own temporary files and the candidate, for removal.
pub fn leftovers(root: &Root) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if root.candidate().exists() {
        v.push(root.candidate());
    }
    for dir in [root.state_dir(), root.etc(), root.run_dir()] {
        if let Ok(d) = fs::read_dir(dir) {
            v.extend(
                d.flatten()
                    .filter(|e| is_tmp_name(&e.file_name().to_string_lossy()))
                    .map(|e| e.path()),
            );
        }
    }
    v
}

/// What a start of the daemon finds (§6.5).
pub fn found(root: &Root, programs: &Programs) -> Found {
    let state = fs::read(root.state()).ok();
    let firstboot_unit = match programs.unit_state(FIRSTBOOT_UNIT) {
        Some(UnitState::Running) => FirstbootUnit::Running,
        Some(UnitState::Failed) => FirstbootUnit::Failed(firstboot_failed(root)),
        Some(UnitState::Inactive) | None => FirstbootUnit::NotRun,
    };
    Found {
        boot_id: boot_id(root),
        done: root.done().exists(),
        marker: root.marker().exists(),
        state: StateFile::read(state.as_deref()),
        firstboot_conf: root.firstboot_conf().exists(),
        firstboot_done: root.firstboot_done().exists(),
        account: account(root),
        firstboot_unit,
        boot_update_conf: root.boot_update_conf().exists(),
        image_len: fs::metadata(root.image()).ok().map(|m| m.len()),
        pending_sums: root.pending_sums().exists(),
        provision_unit: programs.unit_state(PROVISION_UNIT) == Some(UnitState::Running),
        provision_result: provision_result(root),
        work_dir: !work_dirs(root).is_empty(),
        leftovers: !leftovers(root).is_empty(),
    }
}

/// Bytes free on the file system that holds the image.
pub fn free_bytes(root: &Root) -> u64 {
    rustix::fs::statvfs(root.state_dir())
        .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
        .unwrap_or(0)
}

fn battery(root: &Root) -> (u8, bool) {
    let (mut percent, mut charging) = (0u8, false);
    let Ok(dir) = fs::read_dir(root.at("/sys/class/power_supply")) else {
        return (percent, charging);
    };
    for e in dir.flatten() {
        let p = e.path();
        let kind = read_trimmed(&p.join("type")).unwrap_or_default();
        if kind == "Battery" {
            if let Some(c) = read_trimmed(&p.join("capacity")).and_then(|c| c.parse::<u8>().ok()) {
                percent = c.min(100);
            }
            let status = read_trimmed(&p.join("status")).unwrap_or_default();
            charging |= status == "Charging" || status == "Full";
        } else if read_trimmed(&p.join("online")).as_deref() == Some("1") {
            charging = true;
        }
    }
    (percent, charging)
}

/// `status`'s facts, and the battery rule's (§5.4, §5.6).
pub fn facts(root: &Root) -> Facts {
    let release = fs::read_to_string(root.manifest())
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("version: ").map(step_name))
        })
        .unwrap_or_default();
    let kernel: String = read_trimmed(&root.at("/proc/sys/kernel/osrelease"))
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_graphic)
        .take(128)
        .collect();
    let (firstboot_n, firstboot_step, _) = firstboot_log(root);
    let progress = provision_progress(root);
    let (battery, charging) = battery(root);
    Facts {
        release,
        kernel,
        firstboot_step,
        firstboot_n,
        firstboot_of: FIRSTBOOT_STEPS,
        provision_step: progress
            .as_ref()
            .map(|p| p.step.clone())
            .unwrap_or_default(),
        provision_n: progress.as_ref().map_or(0, |p| p.done as u32),
        provision_of: progress.as_ref().map_or(0, |p| p.total as u32),
        battery,
        charging,
        free: free_bytes(root),
    }
}

// --- The radio partitions (§5.5) --------------------------------------------

fn part_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 36
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// What `tools/nv-backup` checks before it reads anything: the board is the
/// descriptor's; each of `NV_PARTS` and `NV_BACKUP_EXTRA` is found exactly
/// once by `PARTNAME`, is a block device (a regular file under a scratch
/// root), reads `ro` 1 and is not mounted. `Err`: the names that fail, and
/// nothing is read.
pub fn backup_list(root: &Root, desc: &Descriptor) -> Result<Vec<PartFound>, Vec<String>> {
    let checked = desc.words("NV_PARTS");
    let mut names = checked.clone();
    names.extend(desc.words("NV_BACKUP_EXTRA"));
    names.retain(|n| part_name_ok(n));
    let compatible = fs::read(root.at("/proc/device-tree/compatible")).unwrap_or_default();
    let first = compatible.split(|b| *b == 0).next().unwrap_or_default();
    let board_ok = desc
        .get("COMPATIBLE")
        .is_some_and(|c| !c.is_empty() && c.as_bytes() == first);
    if !board_ok || names.is_empty() {
        return Err(names);
    }
    // PARTNAME -> the kernel devices that carry it
    let mut by_name: Vec<(String, String)> = Vec::new();
    if let Ok(dir) = fs::read_dir(root.at("/sys/class/block")) {
        for e in dir.flatten() {
            let dev = e.file_name().to_string_lossy().to_string();
            let uevent = fs::read_to_string(e.path().join("uevent")).unwrap_or_default();
            if let Some(name) = uevent.lines().find_map(|l| l.strip_prefix("PARTNAME=")) {
                by_name.push((name.to_string(), dev));
            }
        }
    }
    let mounts = fs::read_to_string(root.at("/proc/mounts")).unwrap_or_default();
    let (mut parts, mut bad) = (Vec::new(), Vec::new());
    for name in &names {
        let devs: Vec<&String> = by_name
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, d)| d)
            .collect();
        let good = || -> Option<PartFound> {
            let [dev] = devs[..] else { return None };
            if dev.is_empty()
                || !dev
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            {
                return None;
            }
            let kind = fs::metadata(root.at("/dev").join(dev)).ok()?.file_type();
            let right_kind = if root.scratch() {
                kind.is_file()
            } else {
                kind.is_block_device()
            };
            let sys = root.at("/sys/class/block").join(dev);
            let ro = read_trimmed(&sys.join("ro"))?;
            let mounted = mounts
                .lines()
                .any(|l| l.split(' ').next() == Some(&format!("/dev/{dev}")));
            let sectors: u64 = read_trimmed(&sys.join("size"))?.parse().ok()?;
            let size = sectors.checked_mul(512).filter(|s| *s > 0)?;
            (right_kind && ro == "1" && !mounted).then(|| PartFound {
                name: name.clone(),
                size,
                checked: checked.contains(name),
                dev: dev.clone(),
            })
        };
        match good() {
            Some(p) => parts.push(p),
            None => bad.push(name.clone()),
        }
    }
    if bad.is_empty() {
        Ok(parts)
    } else {
        Err(bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_step_names_no_user() {
        assert_eq!(step_name("user owner"), "user");
        assert_eq!(step_name("user setup"), "user setup");
        assert_eq!(step_name("SSH keys"), "SSH keys");
        assert_eq!(step_name("login"), "login");
    }
}
