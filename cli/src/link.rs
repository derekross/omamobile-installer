// SPDX-License-Identifier: MIT
//! The install link on this computer: the shell the core's
//! [`LinkFlow`](omamobile_installer_core::link::LinkFlow) needs (the clock,
//! randomness, the radio backup's directory, the owner's factory image, the
//! record in the work directory), and the terminal's way through its
//! screens. The JSON mode uses the same shell with its own way of telling
//! the app (json.rs).
use crate::{Res, Typed};
use omamobile_installer_core::flow::Screen;
use omamobile_installer_core::link::{BackupFiles, LinkFlow, LinkRecord, LinkShell, LinkTransport};
use omamobile_installer_core::{Error, Result};
use pollster::block_on;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The core's error for a file of this computer's.
fn io(what: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |e| Error::Transport(format!("{}: {e}", what.display()))
}

/// Whose clock the run goes by: this computer's, or the pretend phone's.
pub enum Clock {
    Real(Instant),
    #[cfg(feature = "fake-phone")]
    Pretend(omamobile_installer_core::link::pretend::Clock),
}

/// What a front end does with what the run tells it.
pub trait LinkUi {
    fn screen(&mut self, screen: &Screen);
    fn progress(&mut self, what: &'static str, done: u64, total: u64);
    fn go_on(&mut self) -> bool;
}

/// The record and the two paths a second run needs, as the work directory
/// keeps them (`link/record.json`, mode 600).
#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    record: serde_json::Value,
    factory: PathBuf,
    backup_dir: Option<PathBuf>,
}

pub struct FileShell<U: LinkUi> {
    pub ui: U,
    clock: Clock,
    work_dir: Option<PathBuf>,
    backup_dir: Option<PathBuf>,
    factory: PathBuf,
    open: Option<File>,
    /// The record as last kept, to keep again with a new backup directory.
    last: Option<LinkRecord>,
}

/// Where the record is kept.
fn record_path(work_dir: &Path) -> PathBuf {
    work_dir.join("link").join("record.json")
}

/// Write `data` to `path` whole or not at all: a temporary file beside it,
/// mode 600, synced, renamed, the directory synced.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.new",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    let mut f = private_file(&tmp)?;
    f.write_all(data).map_err(io(&tmp))?;
    f.sync_all().map_err(io(&tmp))?;
    fs::rename(&tmp, path).map_err(io(path))?;
    sync_dir(dir)
}

fn private_file(path: &Path) -> Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path).map_err(io(path))
}

fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io(dir))?;
    let _ = dir;
    Ok(())
}

fn private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(io(dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io(dir))?;
    }
    Ok(())
}

/// No one to tell: a shell kept between two runs of the link.
impl LinkUi for () {
    fn screen(&mut self, _: &Screen) {}
    fn progress(&mut self, _: &'static str, _: u64, _: u64) {}
    fn go_on(&mut self) -> bool {
        true
    }
}

impl<U: LinkUi> FileShell<U> {
    /// The same shell, telling another front end.
    pub fn with_ui<V: LinkUi>(self, ui: V) -> FileShell<V> {
        FileShell {
            ui,
            clock: self.clock,
            work_dir: self.work_dir,
            backup_dir: self.backup_dir,
            factory: self.factory,
            open: self.open,
            last: self.last,
        }
    }

    pub fn new(ui: U, clock: Clock, work_dir: Option<PathBuf>, factory: PathBuf) -> Self {
        FileShell {
            ui,
            clock,
            work_dir,
            backup_dir: None,
            factory,
            open: None,
            last: None,
        }
    }

    /// The radio backup goes into `dir`: a new directory, or an empty one
    /// (as `tools/nv-backup` asks), made mode 700. A directory a run before
    /// this one began for the same phone is taken as it is: what it holds
    /// is read on from, and checked against the phone.
    pub fn backup_into(&mut self, dir: &Path, going_on: bool) -> Res<()> {
        if dir.exists() {
            let empty = fs::read_dir(dir)?.next().is_none();
            if !empty && !going_on {
                return Err(format!(
                    "{} is not empty: the radio backup goes into a new, empty folder",
                    dir.display()
                )
                .into());
            }
        }
        private_dir(dir)?;
        self.backup_dir = Some(dir.to_path_buf());
        // A second run must find the folder too
        if let Some(record) = self.last.clone() {
            self.keep(&record)?;
        }
        Ok(())
    }

    fn part(&self, name: &str) -> Result<PathBuf> {
        // Partition names are the protocol's (A-Z a-z 0-9 _ -): one component
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Error::Refused("not a partition's name".into()));
        }
        let dir = self
            .backup_dir
            .as_ref()
            .ok_or_else(|| Error::Refused("no place was chosen for the radio backup".into()))?;
        Ok(dir.join(format!("{name}.img")))
    }

    /// The record a run before this one kept in `work_dir`, with its paths.
    pub fn read_kept(work_dir: &Path) -> Res<(LinkRecord, PathBuf, Option<PathBuf>)> {
        let path = record_path(work_dir);
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let kept: Kept = serde_json::from_slice(&bytes)
            .map_err(|_| format!("{} is not a record of the install link", path.display()))?;
        let record = LinkRecord::parse(kept.record.to_string().as_bytes())
            .ok_or_else(|| format!("{} is not a record of the install link", path.display()))?;
        Ok((record, kept.factory, kept.backup_dir))
    }
}

impl<U: LinkUi> LinkShell for FileShell<U> {
    fn now_ms(&mut self) -> u64 {
        match &self.clock {
            Clock::Real(start) => start.elapsed().as_millis() as u64,
            #[cfg(feature = "fake-phone")]
            Clock::Pretend(c) => c.now(),
        }
    }
    fn wall_clock_s(&mut self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
    async fn sleep(&mut self, ms: u64) {
        match &self.clock {
            Clock::Real(_) => std::thread::sleep(Duration::from_millis(ms)),
            #[cfg(feature = "fake-phone")]
            Clock::Pretend(c) => c.pass(ms),
        }
    }
    fn random(&mut self, buf: &mut [u8]) {
        // The operating system's source. Without it no nonce can be drawn,
        // and a nonce that is not random is no nonce: the tool stops
        getrandom::getrandom(buf).expect("the operating system's random source");
    }
    fn go_on(&mut self) -> bool {
        self.ui.go_on()
    }
    fn screen(&mut self, screen: &Screen) {
        self.ui.screen(screen);
    }
    fn progress(&mut self, what: &'static str, done: u64, total: u64) {
        self.ui.progress(what, done, total);
    }
    fn keep(&mut self, record: &LinkRecord) -> Result<()> {
        let Some(work) = &self.work_dir else {
            return Ok(());
        };
        let dir = work.join("link");
        private_dir(&dir)?;
        let kept = Kept {
            record: serde_json::from_slice(&record.to_json())
                .map_err(|e| Error::Refused(e.to_string()))?,
            factory: self.factory.clone(),
            backup_dir: self.backup_dir.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&kept).map_err(|e| Error::Refused(e.to_string()))?;
        write_private(&record_path(work), &bytes)?;
        self.last = Some(record.clone());
        Ok(())
    }
    async fn backup_open(&mut self, name: &str, _size: u64) -> Result<u64> {
        let path = self.part(name)?;
        Ok(fs::metadata(&path).map_or(0, |m| m.len()))
    }
    async fn backup_write(&mut self, name: &str, offset: u64, data: &[u8]) -> Result<()> {
        let path = self.part(name)?;
        let mut o = OpenOptions::new();
        o.write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut f = o.open(&path).map_err(io(&path))?;
        f.set_len(offset).map_err(io(&path))?;
        f.seek(SeekFrom::Start(offset)).map_err(io(&path))?;
        f.write_all(data).map_err(io(&path))
    }
    async fn backup_digest(&mut self, name: &str) -> Result<[u8; 32]> {
        let path = self.part(name)?;
        // What is stored, synced and read back: not what passed by
        let mut f = File::open(&path).map_err(io(&path))?;
        f.sync_all().map_err(io(&path))?;
        let mut h = omamobile_link::digest::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(io(&path))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(h.finish())
    }
    async fn backup_discard(&mut self, name: &str) -> Result<()> {
        let path = self.part(name)?;
        match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(&path)(e)),
            _ => Ok(()),
        }
    }
    async fn backup_finish(&mut self, files: &BackupFiles) -> Result<()> {
        let dir = self
            .backup_dir
            .clone()
            .ok_or_else(|| Error::Refused("no place was chosen for the radio backup".into()))?;
        for entry in fs::read_dir(&dir).map_err(io(&dir))? {
            let path = entry.map_err(io(&dir))?.path();
            if path.extension().is_some_and(|e| e == "img") {
                File::open(&path)
                    .and_then(|f| f.sync_all())
                    .map_err(io(&path))?;
            }
        }
        write_private(&dir.join("SHA256SUMS"), files.sha256sums.as_bytes())?;
        write_private(&dir.join("partitions.txt"), files.partitions_txt.as_bytes())?;
        write_private(&dir.join("NV-BACKUP"), files.nv_backup.as_bytes())?;
        sync_dir(&dir)
    }
    async fn factory_read(&mut self, offset: u64, len: u32) -> Result<Vec<u8>> {
        if self.open.is_none() {
            self.open = Some(File::open(&self.factory).map_err(io(&self.factory))?);
        }
        let path = self.factory.clone();
        let f = self.open.as_mut().expect("opened above");
        f.seek(SeekFrom::Start(offset)).map_err(io(&path))?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).map_err(io(&path))?;
        Ok(buf)
    }
}

/// The terminal's side: each screen while the phone works, and the
/// progress at each tenth.
#[derive(Default)]
pub struct TextUi {
    tenth: Option<(&'static str, u64)>,
    title: String,
}

impl LinkUi for TextUi {
    fn screen(&mut self, screen: &Screen) {
        // The same step again with a new line of progress: that line alone
        if screen.title == self.title {
            if let Some(last) = screen.body.last() {
                println!("    {last}");
            }
            return;
        }
        self.title = screen.title.clone();
        crate::show(screen);
    }
    fn progress(&mut self, what: &'static str, done: u64, total: u64) {
        if what == "link-wait" || total == 0 {
            return;
        }
        let tenth = done * 10 / total;
        if self.tenth != Some((what, tenth)) {
            self.tenth = Some((what, tenth));
            println!("    {what}: {done} of {total}");
        }
    }
    fn go_on(&mut self) -> bool {
        true
    }
}

/// Take the link's flow to its end on a terminal. `shows` is what the
/// pretend phone's screen shows, in a build with it; a real phone's code is
/// read off its screen by the owner. `true`: the phone is ready.
pub fn drive_text<T: LinkTransport>(
    link: &mut LinkFlow,
    transport: &mut T,
    shell: &mut FileShell<TextUi>,
    typed: &mut Typed,
    shows: &dyn Fn(&T) -> Option<String>,
) -> Res<bool> {
    loop {
        block_on(link.run(transport, shell))?;
        let screen = link.screen();
        crate::show(&screen);
        println!();
        match screen.action {
            "pair" => {
                if let Some(code) = shows(transport) {
                    println!("    (the pretend phone's screen shows the code {code})");
                }
                let mut code = typed.ask("The code on the phone's screen: ")?;
                // The pretend owner reads the pretend screen
                if code == "pretend" {
                    code = shows(transport).unwrap_or_default();
                }
                link.pair(&code)?;
            }
            "backup-dir" => {
                let dir = typed.ask("A new, empty folder for the radio backup: ")?;
                match shell.backup_into(Path::new(&dir), false) {
                    Ok(()) => link.backup_place_chosen()?,
                    Err(e) => println!("    {e}"),
                }
            }
            "answers" => {
                let answers = crate::ask_answers(typed)?;
                let salt = crate::new_salt();
                let extras = omamobile_installer_core::firstboot::Extras::default();
                match omamobile_installer_core::link::link_answers(&answers, &extras, &salt) {
                    Ok(valid) => link.answers(valid)?,
                    Err(e) => println!("    {e}"),
                }
            }
            "retry" => {
                typed.ask("Press Enter to try again: ")?;
                link.retry()?;
            }
            _ => return Ok(screen.state == "ready"),
        }
    }
}
