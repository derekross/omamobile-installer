// SPDX-License-Identifier: MIT
//! Every path the daemon knows, under one root. On a phone the root is `/`.
//! With `--root DIR` it is a scratch directory that stands in for the
//! phone's file system (as `OMAMOBILE_FIRSTBOOT_ROOT` and `NV_PHONE_ROOT`
//! do for the scripts): the daemon then opens nothing outside it, and the
//! fixed programs it runs are the ones under it.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Root {
    root: PathBuf,
}

/// The temporary name a durable write uses beside its target.
pub fn tmp_name(name: &str) -> String {
    format!(".{name}.linkd.tmp")
}

pub fn is_tmp_name(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".linkd.tmp")
}

impl Root {
    pub fn new(root: &Path) -> Root {
        Root {
            root: root.to_path_buf(),
        }
    }
    /// A scratch root: anything but `/`.
    pub fn scratch(&self) -> bool {
        self.root != Path::new("/")
    }
    pub fn dir(&self) -> &Path {
        &self.root
    }
    pub fn at(&self, abs: &str) -> PathBuf {
        self.root.join(abs.trim_start_matches('/'))
    }

    /// The release image's marker (§2.2).
    pub fn marker(&self) -> PathBuf {
        self.at("/etc/omamobile/install-link")
    }
    pub fn etc(&self) -> PathBuf {
        self.at("/etc/omamobile")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.at("/var/lib/omamobile/install-link")
    }
    pub fn state(&self) -> PathBuf {
        self.state_dir().join("state")
    }
    pub fn done(&self) -> PathBuf {
        self.state_dir().join("done")
    }
    /// The boot `omamobile-install-link reopen` ran in (§2.8, P6).
    pub fn reopened(&self) -> PathBuf {
        self.state_dir().join("reopened")
    }
    pub fn image(&self) -> PathBuf {
        self.state_dir().join("factory.zip")
    }
    /// The pending `modem-nv.sha256` (§5.5): it reaches `/etc` in `provision`.
    pub fn pending_sums(&self) -> PathBuf {
        self.state_dir().join("modem-nv.sha256")
    }
    pub fn result(&self) -> PathBuf {
        self.state_dir().join("provision.result")
    }
    pub fn firstboot_conf(&self) -> PathBuf {
        self.etc().join("firstboot.conf")
    }
    pub fn boot_update_conf(&self) -> PathBuf {
        self.etc().join("boot-update.conf")
    }
    pub fn firstboot_done(&self) -> PathBuf {
        self.at("/var/lib/omamobile/firstboot/done")
    }
    pub fn firstboot_user(&self) -> PathBuf {
        self.at("/var/lib/omamobile/firstboot/user")
    }
    pub fn firstboot_log(&self) -> PathBuf {
        self.at("/var/log/omamobile/firstboot.log")
    }
    /// On a tmpfs: gone at every boot.
    pub fn run_dir(&self) -> PathBuf {
        self.at("/run/omamobile-install-link")
    }
    pub fn candidate(&self) -> PathBuf {
        self.run_dir().join("firstboot.conf")
    }
    pub fn agent_socket(&self) -> PathBuf {
        self.run_dir().join("agent")
    }
    pub fn screen_socket(&self) -> PathBuf {
        self.run_dir().join("screen")
    }
    /// The link was closed for this boot (§2.2): on the tmpfs, so the next
    /// boot does not find it.
    pub fn closed(&self) -> PathBuf {
        self.run_dir().join("closed")
    }
    /// `omamobile-link-gadget add` ran in this boot and `remove` has not:
    /// on the tmpfs, for the start after a daemon that was killed.
    pub fn offered(&self) -> PathBuf {
        self.run_dir().join("offered")
    }
    /// Where the provisioning unit's progress lines go.
    pub fn progress(&self) -> PathBuf {
        self.run_dir().join("provision.progress")
    }
    /// The extraction's `work.*` directories are in here.
    pub fn work_parent(&self) -> PathBuf {
        self.at("/var/lib/omamobile/provision")
    }
    pub fn descriptor(&self) -> PathBuf {
        self.at("/usr/share/omamobile/device.conf")
    }
    pub fn manifest(&self) -> PathBuf {
        self.at("/usr/share/omamobile/rootfs/MANIFEST")
    }
    pub fn passwd(&self) -> PathBuf {
        self.at("/etc/passwd")
    }
    pub fn boot_id(&self) -> PathBuf {
        self.at("/proc/sys/kernel/random/boot_id")
    }
    /// A fixed program (§6.1): `/usr/bin/NAME`, under the root.
    pub fn program(&self, name: &str) -> PathBuf {
        self.at("/usr/bin").join(name)
    }
    /// Where `omamobile-link-gadget add` mounts the link's FunctionFS: on
    /// the tmpfs.
    pub fn ffs_dir(&self) -> PathBuf {
        self.run_dir().join("ffs")
    }
    pub fn gadget(&self) -> PathBuf {
        self.at("/usr/lib/omamobile-install-link/omamobile-link-gadget")
    }
}
