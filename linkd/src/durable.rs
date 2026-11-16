// SPDX-License-Identifier: MIT
//! Writes that hold across a power cut (INSTALL-LINK-PLAN §6.5): a
//! temporary file in the target's own directory, synced, then renamed, then
//! the directory synced. And the test's kill points.
use crate::paths::tmp_name;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(test)]
thread_local! {
    /// What a write did, in order: for the test that the order is §6.5's.
    pub static OPS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn op(_what: &str, _path: &Path) {
    #[cfg(test)]
    OPS.with(|o| {
        // (the directory's name and the file's: where it is matters)
        let name = |p: Option<&Path>| {
            p.and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        };
        o.borrow_mut().push(format!(
            "{_what} {}/{}",
            name(_path.parent()),
            name(Some(_path))
        ))
    });
}

fn sync_file(f: &File, path: &Path) -> io::Result<()> {
    op("fsync", path);
    f.sync_all()
}

/// Sync a directory: what makes a rename or a removal in it hold.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    op("fsync-dir", dir);
    File::open(dir)?.sync_all()
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no directory"))
}

/// Replace `path` with `bytes`, mode `mode`: whole or not at all, and on
/// disk when this returns.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = parent(path)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let tmp: PathBuf = dir.join(tmp_name(name));
    op("create", &tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp)?;
    // (a temporary file a kill left may have another mode)
    f.set_permissions(fs::Permissions::from_mode(mode))?;
    f.write_all(bytes)?;
    kill_point();
    sync_file(&f, &tmp)?;
    op("rename", path);
    fs::rename(&tmp, path)?;
    kill_point();
    sync_dir(dir)
}

/// Remove a file and sync its directory. False: there was none.
pub fn remove(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => {
            sync_dir(parent(path)?)?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

// --- The kill test's hook ---------------------------------------------------

static DIE: OnceLock<PathBuf> = OnceLock::new();

/// For the kill test, on a scratch root only: `file` holds a number. Each
/// kill point takes one off; the one that finds 0 removes the file and
/// kills the daemon (SIGKILL: no clean-up runs, as when power is lost or
/// the kernel's OOM killer comes).
pub fn arm_kill_points(file: PathBuf) {
    let _ = DIE.set(file);
}

/// One place a daemon may be killed: before each thing on a list the
/// machine returned, and between the single writes of an effect that makes
/// more than one.
pub fn kill_point() {
    let Some(file) = DIE.get() else { return };
    let Ok(text) = fs::read_to_string(file) else {
        return;
    };
    match text.trim().parse::<u64>() {
        Ok(0) => {
            let _ = fs::remove_file(file);
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
            // (not reached)
            std::process::exit(137);
        }
        Ok(n) => {
            let _ = fs::write(file, (n - 1).to_string());
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of the test's own, removed however the test ends.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    fn scratch(name: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("linkd-durable-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }

    #[test]
    fn a_write_is_a_temporary_file_synced_renamed_and_the_directory_synced() {
        let d = scratch("order");
        let target = d.join("state");
        fs::write(&target, b"old").unwrap();
        OPS.with(|o| o.borrow_mut().clear());
        write(&target, b"new", 0o600).unwrap();
        let ops = OPS.with(|o| o.borrow().clone());
        let dir = d.file_name().unwrap().to_str().unwrap();
        let up = d.parent().unwrap().file_name().unwrap().to_str().unwrap();
        assert_eq!(
            ops,
            [
                // In the target's own directory: a rename cannot cross file
                // systems, and must not be a copy
                format!("create {dir}/.state.linkd.tmp"),
                format!("fsync {dir}/.state.linkd.tmp"),
                format!("rename {dir}/state"),
                format!("fsync-dir {up}/{dir}"),
            ]
        );
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // The temporary file was in the target's own directory, and is gone
        let left: Vec<_> = fs::read_dir(&*d).unwrap().flatten().collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn a_removal_syncs_the_directory_and_a_missing_file_is_no_error() {
        let d = scratch("remove");
        let target = d.join("done");
        fs::write(&target, b"x").unwrap();
        OPS.with(|o| o.borrow_mut().clear());
        assert!(remove(&target).unwrap());
        assert_eq!(OPS.with(|o| o.borrow().len()), 1, "the directory, synced");
        assert!(!remove(&target).unwrap());
    }

    #[test]
    fn a_temporary_file_of_another_mode_does_not_lend_its_mode() {
        let d = scratch("mode");
        let target = d.join("firstboot.conf");
        fs::write(d.join(tmp_name("firstboot.conf")), b"left by a kill").unwrap();
        fs::set_permissions(
            d.join(tmp_name("firstboot.conf")),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        write(&target, b"USERNAME=owner\n", 0o600).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
