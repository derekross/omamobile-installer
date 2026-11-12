// SPDX-License-Identifier: MIT
//! The command-line front end of the installer core.
//!
//!   omamobile-install verify BUNDLE_DIR          no phone: signature, checksums, rules, every file
//!   omamobile-install sparse ROOT.img.zst OUTDIR [MAX_BYTES]
//!                                                no phone: the sparse images a flash would send
//!   omamobile-install devices                    USB: phones in fastboot mode
//!   omamobile-install install BUNDLE_DIR --serial SERIAL --dry-run [--reinstall]
//!                             [--factory-zip FILE] [--work-dir DIR] [--proxy http://HOST:PORT]
//!                                                USB: the install flow up to the dry run, read-only
//!   omamobile-install install BUNDLE_DIR --serial SERIAL --write [--reinstall]
//!                             [--bootloader-slots-match VERSION]
//!                             [--factory-zip FILE] [--work-dir DIR] [--proxy http://HOST:PORT]
//!                                                USB: the install flow, and the writes
//!   omamobile-install flow BUNDLE_DIR --fake-phone STATE [--write] [--reinstall]
//!                             [--factory-zip FILE] [--work-dir DIR] [--fake-google 127.0.0.1:PORT]
//!                                                no phone: the same flow against a pretend phone
//!                                                (only in a build with the `fake-phone` feature)
//!   omamobile-install json [--release BUNDLE_DIR] [--work-dir DIR]
//!                                                USB: the same flow as JSON lines on standard input
//!                                                and output, for the desktop app, agents and scripts
//!                                                (json.rs, docs/APP.md)
//!   omamobile-install help                       what each command does, and what --write erases
//!
//! `install`, `flow` and `json` drive the core's one install flow
//! (core/src/flow.rs): this file prints the screens the flow gives it and
//! hands in what was typed, and json.rs sends them as they are. Neither
//! decides anything or words anything of its own about the phone.
//!
//! `install` takes `--dry-run` or `--write`, one of them, and has no default.
//! With `--dry-run` all that reaches a phone is `getvar` and, for a locked
//! phone, the one question `flashing get_unlock_ability`. With `--write` the
//! flow does the same dry run first, then lists what it will write and what
//! is erased, and waits for the phone's codename to be typed; only then does
//! this file send the release's images through the one session the flow
//! opens for it (`Flow::begin_write`). It opens no session of its own, and
//! which partitions are written is the core's: slot B's boot images and
//! `userdata`, then `set_active:b` and `reboot`.
//!
//! Google's factory image: once the owner has accepted Google's terms on the
//! flow's terms step, this tool downloads the file from the address the core
//! hands it (`https://dl.google.com/`, from the release's signed build
//! index), into `DIR/factory/` under `--work-dir` (by default the user's
//! cache directory). A download that broke off goes on from what is there,
//! in the same run or a later one. The core decides every answer of the
//! server, the size and the SHA-256 (core/src/download.rs); this file only
//! carries the requests out and keeps the bytes. `--factory-zip FILE` hands
//! in a file the owner already saved from Google's page: nothing is
//! downloaded then.
//!
//! The download uses no proxy unless the owner names one with `--proxy`: a
//! proxy named in the environment (`http_proxy`, `https_proxy`, `all_proxy`)
//! is not used.
//!
//! `flow` never reaches Google: its downloads go to the stand-in server
//! named with `--fake-google`, on this computer, or nowhere.
//!
//! Not in the usage text, and for the maintainer only: `install` and `flow`
//! take `--maintainer-skip-bootloader-slot-check`, which lets a dry run go on
//! past a phone that reports one bootloader slot's version (docs/DESIGN.md
//! §6). The core says on every screen that the check was skipped; without
//! `--dry-run` it is refused here, before any USB device is looked for, and
//! the core refuses it again in a flow that writes.
use omamobile_installer_core::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use omamobile_installer_core::download::{
    FactoryDownload, FetchFailure, FetchNext, FetchRequest, FetchResponse,
};
use omamobile_installer_core::fastboot::{
    PhoneId, PieceReport, Session, Transport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS,
};
use omamobile_installer_core::flow::{
    AnswersInput, DownloadEnd, Flow, FlowOptions, OwnerAnswers, Screen, WRITE_HELP,
};
use omamobile_installer_core::install::{flash_userdata_with_progress, UserdataStream};
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_core::text::plain;
use omamobile_installer_core::{build_index, Error};
use omamobile_installer_usb::UsbTransport;
use pollster::block_on;
use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::io::{self, BufRead, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod json;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[cfg(feature = "fake-phone")]
#[path = "../../core/tests/support/fake.rs"]
mod fake;

fn read(dir: &Path, name: &str) -> Res<Vec<u8>> {
    fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()).into())
}

/// A bundle's small files, as read from a directory.
struct Bundle {
    dir: PathBuf,
    sums: Vec<u8>,
    sig: Vec<u8>,
    keyring: Vec<u8>,
    manifest_json: Vec<u8>,
    manifest_txt: Vec<u8>,
    device_conf: Vec<u8>,
    /// `factory-builds.json`, when the bundle has one.
    index: Option<Vec<u8>>,
}

impl Bundle {
    fn load(dir: &Path, keyring: &str) -> Res<Self> {
        Ok(Bundle {
            dir: dir.to_path_buf(),
            sums: read(dir, "SHA256SUMS")?,
            sig: read(dir, "SHA256SUMS.sig")?,
            keyring: read(dir, keyring)?,
            manifest_json: read(dir, "manifest.json")?,
            manifest_txt: read(dir, "MANIFEST")?,
            device_conf: read(dir, "device.conf")?,
            index: fs::read(dir.join(build_index::FILE)).ok(),
        })
    }
    fn files(&self) -> ReleaseFiles<'_> {
        ReleaseFiles {
            sha256sums: &self.sums,
            sha256sums_sig: &self.sig,
            keyring: &self.keyring,
            manifest_json: &self.manifest_json,
            manifest_txt: &self.manifest_txt,
            device_conf: &self.device_conf,
        }
    }
}

/// The bundle's signature, checksums and rules; then every file it lists.
fn verify_bundle(dir: &Path) -> Res<VerifiedRelease> {
    let bundle = Bundle::load(dir, "omamobile.gpg")?;
    let release = verify_release(&bundle.files(), &TrustedKeys::omamobile())?;
    println!(
        "signed by {} (key {})",
        release.signer().primary,
        release.signer().signing_key
    );
    for f in &release.manifest().files {
        release.check_file(&f.path, &read(dir, &f.path)?)?;
    }
    println!(
        "{} files match the signed manifest",
        release.manifest().files.len()
    );
    if release.has_build_index() {
        let index = release.build_index(&read(dir, build_index::FILE)?)?;
        let data = index.data();
        println!(
            "the list of Google's builds passes its checks: {} builds, {} tested, updated {}",
            data.builds.len(),
            data.builds.iter().filter(|b| b.tested()).count(),
            data.updated
        );
    } else {
        println!("this release carries no list of Google's builds (factory-builds.json)");
    }
    Ok(release)
}

/// The root image sits beside the bundle directory, or inside it.
fn find_root(dir: &Path, name: &str) -> Option<PathBuf> {
    // `root.img.zst` is the name fixtures/selftest gives it
    [
        dir.join("..").join(name),
        dir.join(name),
        dir.join("root.img.zst"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Hash the root image from an open file, from where it stands (its
/// start). The caller keeps the handle and streams from it afterwards, so
/// that what is hashed and what is sent are one file, not two openings of
/// a name.
fn hash_root(
    release: &VerifiedRelease,
    file: &mut impl Read,
) -> Res<omamobile_installer_core::bundle::VerifiedRootImage> {
    let mut check = release.root_image_check();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        check.update(&buf[..n]);
    }
    Ok(check.finish()?)
}

fn cmd_verify(dir: &Path) -> Res<()> {
    let release = verify_bundle(dir)?;
    let m = release.manifest();
    println!(
        "Omarchy Mobile {} ({}) for {}",
        m.version, m.label, m.codename
    );
    match find_root(dir, &m.root_image.name) {
        Some(path) => {
            hash_root(&release, &mut File::open(&path)?)?;
            println!("root image matches the signed manifest");
        }
        None => println!(
            "root image not checked: no {} beside {}",
            m.root_image.name,
            dir.display()
        ),
    }
    for w in &m.boot_writes {
        println!(
            "  would flash {} from {} ({} bytes)",
            w.partition.name(),
            w.source,
            w.size
        );
    }
    println!(
        "  would flash {} from {} (erases userdata)",
        m.root_image.partition.name(),
        m.root_image.name
    );
    Ok(())
}

fn cmd_sparse(image: &Path, out_dir: &Path, max: u64) -> Res<()> {
    fs::create_dir_all(out_dir)?;
    let mut file = File::open(image)?;
    let mut stream = UserdataStream::new(max);
    let (mut buf, mut count, mut bytes_in, mut bytes_out) =
        (vec![0u8; 256 << 10], 0usize, 0u64, 0u64);
    let start = Instant::now();
    let mut save = |stream: &mut UserdataStream| -> Res<()> {
        while let Some(seg) = stream.next_segment()? {
            fs::write(out_dir.join(format!("userdata.{count:04}.simg")), &seg)?;
            count += 1;
            bytes_out += seg.len() as u64;
        }
        Ok(())
    };
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes_in += n as u64;
        stream.feed(&buf[..n])?;
        save(&mut stream)?;
    }
    let raw = stream.end()?;
    save(&mut stream)?;
    let secs = start.elapsed().as_secs_f64();
    println!("{bytes_in} compressed bytes -> {raw} raw bytes -> {count} sparse images, {bytes_out} bytes, in {secs:.2} s ({:.0} MiB/s of raw image)", raw as f64 / secs / 1048576.0);
    Ok(())
}

fn is_fastboot(info: &nusb::DeviceInfo) -> bool {
    info.interfaces()
        .any(|i| (i.class(), i.subclass(), i.protocol()) == (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL))
}

async fn cmd_devices() -> Res<()> {
    let mut n = 0;
    for info in nusb::list_devices().await?.filter(is_fastboot) {
        n += 1;
        // Both strings are the device's own: nothing of them that a
        // terminal acts on is printed
        println!(
            "{:04x}:{:04x} {} serial {}",
            info.vendor_id(),
            info.product_id(),
            plain(info.product_string().unwrap_or("?")),
            plain(info.serial_number().unwrap_or("?"))
        );
    }
    println!("{n} phone(s) in fastboot mode");
    Ok(())
}

/// The one phone in fastboot mode with this serial number, opened; `None`
/// when there is none.
async fn open_usb(serial: &str) -> Res<Option<(UsbTransport, PhoneId)>> {
    let mut found = nusb::list_devices()
        .await?
        .filter(|d| is_fastboot(d) && d.serial_number() == Some(serial));
    let Some(info) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err("more than one phone in fastboot mode has that serial number".into());
    }
    let id = PhoneId::of_serial(serial).ok_or("--serial is empty")?;
    let device = info.open().await?;
    Ok(Some((UsbTransport::open(&device).await?, id)))
}

// ---- The flow, on a terminal -------------------------------------------

/// Lines typed by the person. The end of input stops the install.
struct Typed {
    lines: io::Lines<io::StdinLock<'static>>,
    terminal: bool,
}

impl Typed {
    fn new() -> Self {
        Typed {
            terminal: io::stdin().is_terminal(),
            lines: io::stdin().lock().lines(),
        }
    }
    fn ask(&mut self, prompt: &str) -> Res<String> {
        print!("    {prompt}");
        io::stdout().flush()?;
        match self.lines.next() {
            Some(line) => {
                if !self.terminal {
                    println!();
                }
                Ok(line?.trim_end_matches(['\r', '\n']).to_string())
            }
            None => {
                println!();
                Err("no answer (the input ended), so the install stops here".into())
            }
        }
    }
    /// As `ask`, without showing what is typed, where the terminal can.
    fn ask_secret(&mut self, prompt: &str) -> Res<String> {
        let quiet = self.terminal && cfg!(unix) && stty("-echo");
        let answer = self.ask(prompt);
        if quiet {
            stty("echo");
            println!();
        }
        answer
    }
}

fn stty(arg: &str) -> bool {
    std::process::Command::new("stty")
        .arg(arg)
        .status()
        .is_ok_and(|s| s.success())
}

/// Print `text` wrapped at 78 columns: `first` before the first line,
/// `indent` before the others.
fn wrap(text: &str, indent: &str, first: &str) {
    let mut line = String::from(first);
    let mut bare = true;
    for word in text.split(' ') {
        if !bare && line.chars().count() + 1 + word.chars().count() > 78 {
            println!("{line}");
            line = indent.to_string();
            bare = true;
        }
        if !bare {
            line.push(' ');
        }
        line.push_str(word);
        bare = false;
    }
    println!("{line}");
}

/// One screen of the flow, as text. Every word is the core's.
fn show(screen: &Screen) {
    println!("\n==> {}", screen.title);
    if let Some(notice) = &screen.notice {
        println!();
        wrap(notice, "        ", "    !!  ");
    }
    for p in &screen.body {
        println!();
        wrap(p, "    ", "    ");
    }
    // Someone else's words: set in, and in quotation marks
    if let Some(quote) = &screen.quote {
        println!();
        wrap(&quote.intro, "    ", "    ");
        for line in &quote.lines {
            println!();
            wrap(&format!("\"{line}\""), "         ", "        ");
        }
        for p in &quote.after {
            println!();
            wrap(p, "    ", "    ");
        }
        println!();
        for link in &quote.links {
            println!("    {}: {}", link.label, link.url);
        }
    }
    if !screen.steps.is_empty() {
        println!();
    }
    for (n, step) in screen.steps.iter().enumerate() {
        wrap(&step.text, "       ", &format!("    {}. ", n + 1));
        if let Some(command) = &step.command {
            println!("\n           {command}\n");
        }
    }
    if let Some(link) = &screen.link {
        println!("\n    {link}");
    }
    if !screen.plan.is_empty() {
        println!();
        for line in &screen.plan {
            println!("        {line}");
        }
    }
    if let Some(problem) = &screen.problem {
        println!();
        wrap(problem, "      ", "    ! ");
    }
    if !screen.footer.is_empty() {
        println!();
    }
    for line in &screen.footer {
        wrap(line, "    ", "    ");
    }
}

/// What a long step says on the way, and how it is ended early. The
/// terminal prints; the JSON mode sends `progress` and listens for a cancel.
trait Watch {
    /// A line for a person at a terminal.
    fn note(&mut self, _text: &str) {}
    /// Bytes done and bytes in all. `what`: `download`, `check` or `dry-run`.
    fn progress(&mut self, what: &'static str, done: u64, total: u64);
    /// Asked between the pieces of a step: `false` ends it here.
    fn go_on(&mut self, _what: &'static str) -> bool {
        true
    }
}

/// The terminal: a download's bytes at each tenth, and nothing ends early.
#[derive(Default)]
struct Terminal {
    tenth: Option<u64>,
}

impl Watch for Terminal {
    fn note(&mut self, text: &str) {
        println!("    {text}");
    }
    fn progress(&mut self, what: &'static str, done: u64, total: u64) {
        let tenth = done * 10 / total.max(1);
        if what == "download" && self.tenth != Some(tenth) {
            self.tenth = Some(tenth);
            println!("    {done} of {total} bytes");
        }
    }
}

fn local_timezone() -> String {
    fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| {
            p.to_str()?
                .split_once("zoneinfo/")
                .map(|(_, z)| z.to_string())
        })
        .unwrap_or_else(|| "UTC".into())
}

fn ask_answers(typed: &mut Typed) -> Res<OwnerAnswers> {
    let zone = local_timezone();
    let zones = Path::new("/usr/share/zoneinfo");
    loop {
        let username = typed.ask("Your user name on the phone (lower-case, for example alex): ")?;
        let password = typed
            .ask_secret("A PIN or password to unlock the phone (digits only gives a PIN pad): ")?;
        let again = typed.ask_secret("The same again: ")?;
        let hostname = typed.ask(&format!("The phone's name [{username}-phone]: "))?;
        let timezone = typed.ask(&format!("Time zone [{zone}]: "))?;
        let timezone = if timezone.is_empty() {
            zone.clone()
        } else {
            timezone
        };
        let wifi_ssid = typed.ask("Wi-Fi network name (Enter to skip): ")?;
        let wifi_psk = if wifi_ssid.is_empty() {
            String::new()
        } else {
            typed.ask_secret("Wi-Fi password (Enter if it has none): ")?
        };
        let input = AnswersInput {
            username: &username,
            password: &password,
            password_again: &again,
            hostname: &hostname,
            timezone: &timezone,
            wifi_ssid: &wifi_ssid,
            wifi_psk: &wifi_psk,
        };
        match OwnerAnswers::check(&input) {
            // The core checks the zone's shape; this computer knows the zones
            Ok(_) if zones.is_dir() && !zones.join(&timezone).is_file() => {
                println!(
                    "    That isn't a time zone this computer knows (they look like Europe/Paris)."
                );
            }
            Ok(answers) => return Ok(answers),
            Err(problems) => {
                for p in problems {
                    println!("    {}", p.message);
                }
            }
        }
        println!();
    }
}

/// Print what the session sent since `seen` that is not a question: the
/// commands themselves, as the phone received them.
fn show_sent<T: Transport>(session: &Session<T>, seen: &mut usize) {
    for command in &session.log()[*seen..] {
        if !command.starts_with("getvar:") {
            println!("    {command}");
        }
    }
    *seen = session.log().len();
}

/// One piece of userdata in words: what its flash covered on the phone, and
/// how long the phone took to answer it.
fn piece_line(number: u32, piece: &PieceReport) -> String {
    let took = match piece.flash {
        Some(took) => format!("answered in {:.3} s", took.as_secs_f64()),
        None => "not answered".into(),
    };
    format!(
        "piece {number}: {} blocks, {} KiB written on the phone, {} bytes sent, {took}",
        piece.blocks,
        piece.blocks as u64 * 4,
        piece.bytes
    )
}

/// What the run's log keeps of the userdata pieces: the one that covered
/// the most blocks (the most work one flash asked of the phone), the flash
/// the phone took longest over, and a piece that was sent and not answered.
fn userdata_account(pieces: &[PieceReport]) -> Vec<String> {
    let numbered = || pieces.iter().enumerate().map(|(i, p)| (i as u32 + 1, p));
    let mut out = Vec::new();
    // The first of equals, so the line names one piece
    if let Some((n, piece)) = numbered().rev().max_by_key(|(_, p)| p.blocks) {
        out.push(format!(
            "userdata, the piece that covered the most blocks: {}",
            piece_line(n, piece)
        ));
    }
    if let Some((n, piece)) = numbered()
        .rev()
        .filter(|(_, p)| p.flash.is_some())
        .max_by_key(|(_, p)| p.flash)
    {
        out.push(format!(
            "userdata, the flash the phone took longest over: {}",
            piece_line(n, piece)
        ));
    }
    if let Some((n, piece)) = numbered().next_back().filter(|(_, p)| p.flash.is_none()) {
        out.push(format!(
            "userdata, sent and not confirmed: {}",
            piece_line(n, piece)
        ));
    }
    out
}

/// Send the release's images through a session the flow opened, when they
/// are on disk: each is checked against the signed manifest first. In the
/// dry run's session every write is held back; in the one `Flow::begin_write`
/// opened they are written, and each command is printed as it is sent.
/// `Ok(false)`: the files aren't here, and nothing was sent.
async fn walk_images<T: Transport>(
    session: &mut Session<T>,
    release: &VerifiedRelease,
    dir: &Path,
    watch: &mut dyn Watch,
) -> Result<bool, Error> {
    let writing = !session.is_dry_run();
    let mut seen = session.log().len();
    let m = release.manifest();
    let Some(root_path) = find_root(dir, &m.root_image.name) else {
        return Ok(false);
    };
    if !m.boot_writes.iter().all(|w| dir.join(&w.source).is_file()) {
        return Ok(false);
    }
    let io = |e: io::Error| Error::Transport(format!("reading the release's files: {e}"));
    // Every file is checked before the first write, the root image among
    // them: a file that is not the release's is found while the phone is
    // still as it was, not after slot B's boot partitions are written.
    // The bytes to get through: every file twice, once to check it and
    // once to send it (or, in a dry run, to walk it through the session)
    let root_size = fs::metadata(&root_path).map_err(io)?.len();
    let total = 2 * (m.boot_writes.iter().map(|w| w.size).sum::<u64>() + root_size);
    let what = "dry-run";
    let watch = RefCell::new(watch);
    let done = Cell::new(0u64);
    let add = |bytes: u64| {
        done.set(done.get() + bytes);
        watch.borrow_mut().progress(what, done.get(), total);
    };
    add(0);
    for w in &m.boot_writes {
        let bytes = fs::read(dir.join(&w.source)).map_err(io)?;
        release.verify_boot_image(w, &bytes)?;
        add(bytes.len() as u64);
    }
    // The root image is opened once. It is hashed from this handle, and
    // the same handle is rewound and streamed, so the file that is sent is
    // the file that was checked even if the name is given to another file
    // meanwhile. A file whose bytes change in place between the two is
    // caught by the session, which hashes what it is sent again and
    // refuses to make slot B active for anything else.
    let mut root_file = File::open(&root_path).map_err(io)?;
    let root = hash_root(release, &mut Counted(&mut root_file, &add))
        .map_err(|e| Error::Checksum(e.to_string()))?;
    root_file.seek(SeekFrom::Start(0)).map_err(io)?;
    // Each boot image is read and checked again as it is written: the
    // bytes that are sent are the bytes that were just checked
    for w in &m.boot_writes {
        let bytes = fs::read(dir.join(&w.source)).map_err(io)?;
        let result = session
            .flash_boot(&release.verify_boot_image(w, &bytes)?)
            .await;
        if writing {
            show_sent(session, &mut seen);
        }
        result?;
        add(bytes.len() as u64);
    }
    let reader = Counted(&mut root_file, &add);
    if writing {
        // The core has no clock: this one times each flash of userdata
        let begun = std::time::Instant::now();
        session.time_flashes_with(Box::new(move || begun.elapsed()));
    }
    let mut progress = |pieces: u32, piece: &PieceReport| {
        if writing {
            println!("    flash:userdata ({})", piece_line(pieces, piece));
        }
    };
    let result =
        flash_userdata_with_progress(session, &root, reader, Some(64 << 20), &mut progress).await;
    if writing {
        for line in userdata_account(session.pieces()) {
            println!("    {line}");
        }
    }
    result?;
    seen = session.log().len();
    let result = session.activate_and_reboot().await;
    if writing {
        show_sent(session, &mut seen);
    }
    result?;
    Ok(true)
}

/// A file of the release as a session reads it, counted for the progress.
struct Counted<'a, R>(R, &'a dyn Fn(u64));

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.0.read(buf)?;
        (self.1)(n as u64);
        Ok(n)
    }
}

// ---- Google's factory image ----------------------------------------------

/// Where a request for Google's factory image is carried out. The address
/// is always the core's ([`FetchRequest`]); this says who answers it.
enum Fetcher {
    /// Google's server, over HTTPS.
    Google(ureq::Agent),
    /// A stand-in for Google's server on this computer, for `flow`.
    #[cfg(feature = "fake-phone")]
    StandIn(ureq::Agent, std::net::SocketAddr),
    /// `flow` without a stand-in: nothing is fetched from anywhere.
    #[cfg(feature = "fake-phone")]
    Nowhere,
}

/// How long the tool waits for the next bytes of Google's file before it
/// ends a try. The connect and response timeouts below cover the time
/// before the body; ureq's own body timeout is for the whole transfer,
/// which no number fits for a file of several gigabytes on any connection.
const BODY_IDLE: Duration = Duration::from_secs(60);
/// The same for the stand-in of a pretend run, which is on this computer.
#[cfg(feature = "fake-phone")]
const STAND_IN_BODY_IDLE: Duration = Duration::from_secs(2);

/// The HTTP client. `proxy`: the one the owner named with `--proxy`, or
/// none. A proxy named in the environment (`http_proxy`, `https_proxy`,
/// `all_proxy`, in either case) is never used: the client would take one
/// from there by default, and whoever sets such a variable for a shell
/// would learn, unasked, that this computer fetches a Pixel factory image.
fn agent(https_only: bool, proxy: Option<ureq::Proxy>) -> ureq::Agent {
    ureq::Agent::config_builder()
        // Said even when it is None: that is what switches the
        // environment's proxy off
        .proxy(proxy)
        // The core is shown every redirect and every status, and decides
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .https_only(https_only)
        .user_agent(concat!("omamobile-install/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
}

impl Fetcher {
    fn google(proxy: Option<ureq::Proxy>) -> Self {
        Fetcher::Google(agent(true, proxy))
    }

    /// For a pretend run, which never reaches Google: a stand-in on this
    /// computer answers in its place, or nothing does.
    #[cfg(feature = "fake-phone")]
    fn pretend(fake_google: Option<&str>, proxy: Option<ureq::Proxy>) -> Res<Self> {
        let Some(at) = fake_google else {
            return Ok(Fetcher::Nowhere);
        };
        let at: std::net::SocketAddr = at
            .parse()
            .map_err(|_| "--fake-google takes an address like 127.0.0.1:8080")?;
        if !at.ip().is_loopback() {
            return Err("--fake-google takes an address on this computer only".into());
        }
        Ok(Fetcher::StandIn(agent(false, proxy), at))
    }

    /// How long a body may stay silent before the try is ended.
    fn body_idle(&self) -> Duration {
        match self {
            Fetcher::Google(_) => BODY_IDLE,
            #[cfg(feature = "fake-phone")]
            _ => STAND_IN_BODY_IDLE,
        }
    }

    /// One GET, as the core asked for it. Redirects are not followed here.
    fn get(&self, request: &FetchRequest) -> Result<ureq::http::Response<ureq::Body>, String> {
        let (agent, url) = match self {
            Fetcher::Google(agent) => (agent, request.url().to_string()),
            // The same request, answered by the stand-in in Google's place
            #[cfg(feature = "fake-phone")]
            Fetcher::StandIn(agent, at) => (
                agent,
                request
                    .url()
                    .replacen("https://dl.google.com/", &format!("http://{at}/"), 1),
            ),
            #[cfg(feature = "fake-phone")]
            Fetcher::Nowhere => {
                return Err(
                    "a pretend run fetches nothing from Google, and no stand-in was named with --fake-google"
                        .into(),
                )
            }
        };
        let mut call = agent.get(&url);
        if let Some(range) = request.range() {
            call = call.header("Range", range);
        }
        call.call().map_err(|e| e.to_string())
    }
}

/// `--proxy`'s value: an HTTP proxy the download goes through, as
/// `http://HOST:PORT` (with `USER:PASSWORD@` before the host if it asks for
/// one). The value is never printed back.
fn parse_proxy(value: Option<&str>) -> Res<ureq::Proxy> {
    let bad = "--proxy takes an address like http://HOST:PORT";
    let value = value.ok_or(bad)?;
    match value.strip_prefix("http://") {
        Some(rest) if !rest.is_empty() => Ok(ureq::Proxy::new(value).map_err(|_| bad)?),
        _ => Err(bad.into()),
    }
}

/// The user's cache directory, for what a download leaves between runs.
fn default_work_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    var("XDG_CACHE_HOME")
        .or_else(|| var("LOCALAPPDATA"))
        .or_else(|| var("HOME").map(|home| home.join(".cache")))
        .map(|dir| dir.join("omamobile-installer"))
}

/// What became of a wait for the next piece of a body.
enum Piece {
    Bytes(Vec<u8>),
    /// The body is over.
    End,
    /// Nothing arrived in time.
    Silent,
    Broke(String),
}

/// A response's body, read on a thread of its own, so that the wait for
/// each piece can be given up: a server that stops sending without closing
/// the connection would otherwise hold a blocking read, and the tool, for
/// good. After a silence the thread is left behind, blocked on a connection
/// nobody listens to; it touches no file and ends with the connection or
/// the tool.
struct Pieces(std::sync::mpsc::Receiver<io::Result<Vec<u8>>>);

impl Pieces {
    fn of(mut body: impl Read + Send + 'static) -> Self {
        // A couple of pieces ahead at most: the reader waits for the file
        let (pieces, taken) = std::sync::mpsc::sync_channel(2);
        std::thread::spawn(move || loop {
            let mut buf = vec![0u8; 64 << 10];
            let piece = body.read(&mut buf).map(|n| {
                buf.truncate(n);
                buf
            });
            let over = !matches!(&piece, Ok(bytes) if !bytes.is_empty());
            if pieces.send(piece).is_err() || over {
                break;
            }
        });
        Pieces(taken)
    }

    /// The next piece, waiting at most `idle` for it.
    fn next(&self, idle: Duration) -> Piece {
        use std::sync::mpsc::RecvTimeoutError;
        match self.0.recv_timeout(idle) {
            Ok(Ok(bytes)) if bytes.is_empty() => Piece::End,
            Ok(Ok(bytes)) => Piece::Bytes(bytes),
            Ok(Err(e)) => Piece::Broke(e.to_string()),
            Err(RecvTimeoutError::Timeout) => Piece::Silent,
            Err(RecvTimeoutError::Disconnected) => Piece::End,
        }
    }
}

/// Carry out what one try at the download asks: the kept bytes first, then
/// the requests, the body into `file`. Every decision is `download`'s.
/// `watch` is told how far it is, and may end the transfer between pieces,
/// also one that stands still: the core then finds fewer bytes than the
/// file has, as when a transfer broke off.
fn fetch(
    download: &mut FactoryDownload,
    fetcher: &Fetcher,
    path: &Path,
    watch: &mut dyn Watch,
) -> Result<(), FetchFailure> {
    let storage = |e: io::Error| FetchFailure::Storage(format!("{}: {e}", path.display()));
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(storage)?;
    let mut buf = vec![0u8; 1 << 20];
    // What an earlier try left is read back, from the first byte: the core
    // hashes it like the bytes that arrive now
    let mut left = download.resume_from();
    if left == 0 {
        file.set_len(0).map_err(storage)?;
    }
    while left > 0 {
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = file.read(&mut buf[..want]).map_err(storage)?;
        if n == 0 {
            return Err(FetchFailure::Storage(format!(
                "{} got shorter while it was read",
                path.display()
            )));
        }
        download
            .kept(&buf[..n])
            .map_err(|e| FetchFailure::Storage(e.to_string()))?;
        left -= n as u64;
    }
    let Some(mut request) = download.request() else {
        return Ok(());
    };
    loop {
        let response = fetcher.get(&request).map_err(FetchFailure::Network)?;
        let next = {
            let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok());
            let answer = FetchResponse {
                status: response.status().as_u16(),
                location: header("location"),
                final_url: None,
                content_length: header("content-length").and_then(|v| v.parse().ok()),
                content_range: header("content-range"),
            };
            download
                .response(&answer)
                .map_err(|e| FetchFailure::Network(e.to_string()))?
        };
        match next {
            FetchNext::Fetch(next) => request = next,
            FetchNext::End => return Ok(()),
            FetchNext::Body { at } => {
                file.set_len(at).map_err(storage)?;
                file.seek(SeekFrom::Start(at)).map_err(storage)?;
                let idle = fetcher.body_idle();
                let body = Pieces::of(response.into_body().into_reader());
                // Waited for in short turns, so that `watch` can end a
                // transfer that stands still before the silence runs out
                let turn = idle.min(Duration::from_millis(200));
                let mut heard = Instant::now();
                while watch.go_on("download") {
                    let piece = match body.next(turn) {
                        Piece::Bytes(piece) => piece,
                        Piece::End => break,
                        Piece::Silent if heard.elapsed() < idle => continue,
                        // What arrived so far is in the file, for the next try
                        Piece::Silent => {
                            return Err(FetchFailure::Stalled {
                                seconds: idle.as_secs(),
                            })
                        }
                        Piece::Broke(why) => return Err(FetchFailure::Network(why)),
                    };
                    heard = Instant::now();
                    // The core takes no byte past the file's size
                    if !download.body(&piece) {
                        break;
                    }
                    file.write_all(&piece).map_err(storage)?;
                    let (done, total) = download.progress();
                    watch.progress("download", done, total);
                }
                return file.flush().map_err(storage);
            }
        }
    }
}

/// One try at downloading Google's factory image into the work directory.
/// What happens to the bytes afterwards is the core's answer.
fn download_factory_zip(
    flow: &mut Flow,
    fetcher: &Fetcher,
    proxied: bool,
    work_dir: Option<&Path>,
    name: &str,
    watch: &mut dyn Watch,
) -> Res<()> {
    let dir = match work_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_work_dir()
            .ok_or("no place to keep the download: name a directory with --work-dir DIR")?,
    }
    .join("factory");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // The name is the signed index's, checked there: one path component
    let (whole, part) = (dir.join(name), dir.join(format!("{name}.part")));
    // A file an earlier run finished is checked again like any kept bytes
    let path = if whole.is_file() { &whole } else { &part };
    let have = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut download = flow.factory_download(have)?;
    watch.note(&format!(
        "downloading {name} into {}{}",
        dir.display(),
        if proxied {
            ", through the proxy named with --proxy"
        } else {
            ""
        }
    ));
    let failure = fetch(&mut download, fetcher, path, watch).err();
    match flow.factory_download_done(download, failure)? {
        DownloadEnd::Verified => {
            if path != &whole {
                fs::rename(path, &whole)?;
            }
        }
        // Nothing arrived: no empty file is left behind
        DownloadEnd::Kept => {
            if fs::metadata(path).is_ok_and(|m| m.len() == 0) {
                fs::remove_file(path)?;
            }
        }
        DownloadEnd::Discard => {
            for file in [&whole, &part] {
                match fs::remove_file(file) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => {
                        return Err(format!("{}: {e}", file.display()).into())
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Hand the flow a file the owner saved, read a piece at a time. `false`:
/// there is no such file, and the flow was handed nothing. A check that
/// `watch` ends early is dropped, and the flow's step stays as it was.
fn take_factory_zip(flow: &mut Flow, path: &Path, watch: &mut dyn Watch) -> Res<bool> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let size = match fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return Ok(false),
    };
    if let Some(mut check) = flow.factory_zip(name, size)? {
        watch.note(&format!("checking {name}"));
        // Streamed: a megabyte at a time, never the whole file
        let mut file = File::open(path)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if !watch.go_on("check") {
                return Ok(true);
            }
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            check.update(&buf[..n]);
            let (done, total) = check.progress();
            watch.progress("check", done, total);
        }
        flow.factory_zip_done(check)?;
    }
    Ok(true)
}

/// As the terminal does it: a file that is not there is said, and the step
/// stays open.
fn take_typed_zip(flow: &mut Flow, path: &Path) -> Res<()> {
    if !take_factory_zip(flow, path, &mut Terminal::default())? {
        println!("    There is no file {}", path.display());
    }
    Ok(())
}

/// What the flow's factory-image step works with on this computer.
struct FactoryImage {
    /// A file the owner already saved from Google's page (`--factory-zip`).
    own_file: Option<PathBuf>,
    /// Where a download is kept (`--work-dir`).
    work_dir: Option<PathBuf>,
    fetcher: Fetcher,
    /// The owner named a proxy for the download (`--proxy`).
    proxied: bool,
}

/// Drive the core's flow to its end. `connect` finds the phone and opens
/// it, or says there is none; nothing else here touches a phone.
fn run_flow<T: Transport>(
    bundle: &Bundle,
    trusted: &TrustedKeys,
    mut options: FlowOptions,
    mut factory: FactoryImage,
    mut connect: impl FnMut() -> Res<Option<(T, PhoneId)>>,
) -> Res<bool> {
    let mut typed = Typed::new();
    // A file handed in up front: the installer downloads nothing
    options.owner_has_factory_zip = factory.own_file.is_some();
    let mut flow = Flow::begin(&bundle.files(), bundle.index.as_deref(), trusted, options);
    let (mut looked, mut downloaded) = (false, false);
    loop {
        let screen = flow.screen();
        show(&screen);
        println!();
        match screen.action {
            "acknowledge" => {
                if typed.ask("Type yes to go on, or press Enter to stop: ")? == "yes" {
                    flow.acknowledge_warning()?;
                } else {
                    flow.decline_warning()?;
                }
            }
            "read-phone" => {
                // The first look needs no question; after that, the person
                // says when the phone is ready to be looked at again
                if looked {
                    typed.ask("Press Enter when that is done, to look at the phone again: ")?;
                }
                looked = true;
                match connect()? {
                    Some((mut transport, id)) => block_on(flow.read_phone(&mut transport, &id))?,
                    None => flow.no_phone()?,
                }
            }
            "accept-terms" => {
                if typed
                    .ask("Type yes to accept, or press Enter to download the file yourself: ")?
                    == "yes"
                {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                    flow.accept_google_terms(now)?;
                } else {
                    flow.decline_google_terms()?;
                }
            }
            "download" => {
                // The first try needs no question. After one that ended
                // early the person says whether to go on, or hands in a file
                if downloaded {
                    let own = typed.ask(
                        "Press Enter to try the download again, or type where you saved the file yourself: ",
                    )?;
                    if !own.is_empty() {
                        take_typed_zip(&mut flow, Path::new(&own))?;
                        continue;
                    }
                }
                downloaded = true;
                let name = screen.file.as_ref().map(|f| f.name.as_str());
                download_factory_zip(
                    &mut flow,
                    &factory.fetcher,
                    factory.proxied,
                    factory.work_dir.as_deref(),
                    name.ok_or("the flow names no file to download")?,
                    &mut Terminal::default(),
                )?;
            }
            "take-file" => {
                let path = match factory.own_file.take() {
                    Some(path) => path,
                    None => PathBuf::from(typed.ask("Where did you save the file? ")?),
                };
                take_typed_zip(&mut flow, &path)?;
            }
            "answers" => flow.answers(&ask_answers(&mut typed)?)?,
            "dry-run" => {
                let Some((transport, _)) = connect()? else {
                    println!("    No phone in Fastboot Mode answered.");
                    typed.ask("Press Enter to look again: ")?;
                    continue;
                };
                let release = flow.release().ok_or("the flow has no release")?.clone();
                if let Some(mut session) = block_on(flow.dry_run(transport))? {
                    match block_on(walk_images(
                        &mut session,
                        &release,
                        &bundle.dir,
                        &mut Terminal::default(),
                    )) {
                        Ok(_) => {
                            flow.dry_run_done(session)?;
                        }
                        Err(e) => flow.dry_run_failed(&e)?,
                    }
                }
            }
            // What will be written was listed: the owner types the phone's
            // codename, and the core takes that or stops
            "confirm" => {
                let answer = typed.ask("Codename: ")?;
                flow.confirm_install(&answer)?;
            }
            "write" => {
                let Some((transport, _)) = connect()? else {
                    println!("    No phone in Fastboot Mode answered.");
                    typed.ask("Press Enter to look again: ")?;
                    continue;
                };
                let release = flow.release().ok_or("the flow has no release")?.clone();
                // The one session that can write is the flow's
                if let Some(mut session) = block_on(flow.begin_write(transport))? {
                    match block_on(walk_images(
                        &mut session,
                        &release,
                        &bundle.dir,
                        &mut Terminal::default(),
                    )) {
                        Ok(true) => {
                            flow.write_done(session)?;
                        }
                        Ok(false) => {
                            let gone = Error::Transport(
                                "the release's files are no longer where the dry run read them"
                                    .into(),
                            );
                            flow.write_failed(session, &gone)?;
                        }
                        Err(e) => {
                            flow.write_failed(session, &e)?;
                        }
                    }
                }
            }
            _ => return Ok(matches!(screen.state, "done" | "installed")),
        }
    }
}

fn cmd_install(dir: &Path, serial: &str, options: FlowOptions, factory: FactoryImage) -> Res<bool> {
    let bundle = Bundle::load(dir, "omamobile.gpg")?;
    run_flow(&bundle, &TrustedKeys::omamobile(), options, factory, || {
        block_on(open_usb(serial))
    })
}

/// The same flow against the tests' pretend phone, held in memory. A bundle
/// for it is signed by a test key, which only this command trusts, and only
/// together with the pretend phone: there is no way from here to USB.
#[cfg(feature = "fake-phone")]
fn cmd_fake(
    dir: &Path,
    scenario: &str,
    options: FlowOptions,
    mut factory: FactoryImage,
    fake_google: Option<&str>,
    proxy: Option<ureq::Proxy>,
) -> Res<bool> {
    use fake::{FakePhone, SERIAL};
    factory.fetcher = Fetcher::pretend(fake_google, proxy)?;
    let phone = FakePhone::scenario(scenario).ok_or_else(|| {
        format!(
            "no pretend phone '{scenario}'; there are: {}",
            FakePhone::SCENARIOS.join(", ")
        )
    })?;
    let bundle = Bundle::load(dir, "test-key.gpg")?;
    let fingerprint = String::from_utf8(read(dir, "test-key.fingerprint")?)?;
    let trusted = TrustedKeys::new(&[fingerprint.trim()])?;
    println!("A pretend phone ({scenario}), held in memory. No USB device is opened.");
    let mut looks = 0;
    // The record is printed however the run ended
    let done = run_flow(&bundle, &trusted, options, factory, || {
        looks += 1;
        // A locked pretend phone is unlocked by its pretend owner after the
        // unlock steps were shown once
        if scenario == "locked" && looks == 2 {
            println!("    (the pretend phone's owner unlocked it)");
            phone.edit(|s| {
                s.vars.insert("unlocked".into(), "yes".into());
            });
        }
        Ok(Some((phone.clone(), PhoneId::of_serial(SERIAL).unwrap())))
    });
    let log = phone.log();
    let (asked, other) = (phone.unlock_questions(), phone.not_reads().len());
    println!(
        "\nThe pretend phone received {} commands: {} getvar, {asked} flashing get_unlock_ability, {other} others; partitions written: {}",
        log.len(),
        log.len() - asked - other,
        phone.written().len()
    );
    if other > 0 {
        println!(
            "Those others, in the order it received them (partitions written: {}):",
            phone.written().join(", ")
        );
        for command in phone.not_reads() {
            println!("    {command}");
        }
    }
    done
}

#[cfg(not(feature = "fake-phone"))]
fn cmd_fake(
    _: &Path,
    _: &str,
    _: FlowOptions,
    _: FactoryImage,
    _: Option<&str>,
    _: Option<ureq::Proxy>,
) -> Res<bool> {
    Err("this build has no pretend phone: build with --features fake-phone".into())
}

const USAGE: &str = "usage: omamobile-install verify DIR | sparse IMG.zst OUTDIR [MAX] | devices | install DIR --serial S (--dry-run | --write) [--reinstall] [--bootloader-slots-match VERSION] [--factory-zip FILE] [--work-dir DIR] [--proxy http://HOST:PORT] | flow DIR --fake-phone STATE [--write] [--reinstall] [--factory-zip FILE] [--work-dir DIR] [--proxy http://HOST:PORT] [--fake-google 127.0.0.1:PORT] | json [--release DIR] [--work-dir DIR] | help";

/// What `help` says about [`SLOTS_MATCH`].
const SLOTS_MATCH_HELP: &str = "Only for a phone whose bootloader tells the version of one of its two copies (the Pixel 7 Pro): your statement that the other copy's version was read from the phone and is VERSION. The installer stops unless VERSION is the version the phone reports for the copy in use. It cannot check your statement and never switches the phone's slots to try: if the two copies differ, the phone may not start from slot B. Every screen of such a run says that it rests on this statement. Without it, such a phone stops before anything is written.";

/// `omamobile-install help`: the commands, and what `--write` does to a
/// phone, in the core's words.
fn cmd_help() {
    println!("{USAGE}\n");
    println!("  verify DIR           check a downloaded release: signature, rules, every file. No phone.");
    println!("  sparse IMG OUTDIR    cut a root image into the sparse images a flash would send. No phone.");
    println!("  devices              list phones in Fastboot Mode (USB; reads only).");
    println!("  install DIR --serial S --dry-run");
    println!(
        "                       the install flow up to its dry run. It asks the phone questions"
    );
    println!("                       and writes nothing.");
    println!("  install DIR --serial S --write");
    wrap(
        WRITE_HELP,
        "                       ",
        "                       ",
    );
    println!("\n  install needs --dry-run or --write, one of them: there is no default.");
    println!("\n  install ... {SLOTS_MATCH} VERSION");
    wrap(
        SLOTS_MATCH_HELP,
        "                       ",
        "                       ",
    );
}

fn bench_zstd(file: &str, rounds: &str) -> Res<()> {
    // Decode a small .zst repeatedly: the same loop as the browser self-test
    let (data, mut bytes, start) = (fs::read(file)?, 0u64, Instant::now());
    for _ in 0..rounds.parse::<u32>()? {
        omamobile_installer_core::zstd_push::ZstdPush::new().push(&data, &mut |d| {
            bytes += d.len() as u64;
            Ok(())
        })?;
    }
    println!(
        "{} MiB in {:.0} ms: {:.0} MiB/s",
        bytes >> 20,
        start.elapsed().as_secs_f64() * 1e3,
        bytes as f64 / 1048576.0 / start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// The maintainer's override for the bootloader-slot stop, in a dry run
/// only. Deliberately not in [`USAGE`].
const SLOT_OVERRIDE: &str = "--maintainer-skip-bootloader-slot-check";
/// The owner's statement for a phone that reports one bootloader slot's
/// version: the other slot's was read from the phone and is VERSION. The
/// core compares it with the phone ([`FlowOptions::bootloader_slots_match`]).
const SLOTS_MATCH: &str = "--bootloader-slots-match";

/// `Ok(false)`: the install flow stopped, and has said why.
fn run() -> Res<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["verify", dir] => cmd_verify(Path::new(dir))?,
        ["sparse", image, out] => cmd_sparse(Path::new(image), Path::new(out), 256 << 20)?,
        ["sparse", image, out, max] => cmd_sparse(Path::new(image), Path::new(out), max.parse()?)?,
        ["bench-zstd", file, rounds] => bench_zstd(file, rounds)?,
        ["devices"] => block_on(cmd_devices())?,
        ["help" | "--help" | "-h"] => cmd_help(),
        [command @ ("install" | "flow"), dir, rest @ ..] => {
            let (mut serial, mut fake, mut dry_run, mut write) = (None, None, false, false);
            let (mut options, mut fake_google) = (FlowOptions::default(), None);
            let (mut own_file, mut work_dir, mut proxy) = (None, None, None);
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match *arg {
                    "--serial" => serial = it.next().copied(),
                    "--fake-phone" => fake = it.next().copied(),
                    "--dry-run" => dry_run = true,
                    "--write" => write = true,
                    "--reinstall" => options.reinstall = true,
                    SLOT_OVERRIDE => options.maintainer_skip_bootloader_slot_check = true,
                    // The owner's statement about the other bootloader
                    // slot, as tools/install-phone takes it: said once,
                    // with the version that was read from the phone
                    SLOTS_MATCH => {
                        let version = it.next().copied().filter(|v| !v.is_empty());
                        let Some(version) = version else {
                            return Err(format!("{SLOTS_MATCH} takes the bootloader version that was read from the phone's other slot").into());
                        };
                        if options.bootloader_slots_match.is_some() {
                            return Err(format!("{SLOTS_MATCH} is given once").into());
                        }
                        options.bootloader_slots_match = Some(version.to_string());
                    }
                    "--factory-zip" => own_file = it.next().map(PathBuf::from),
                    "--work-dir" => work_dir = it.next().map(PathBuf::from),
                    // The one way a proxy is used: named here, by the owner
                    "--proxy" => proxy = Some(parse_proxy(it.next().copied())?),
                    "--fake-google" => fake_google = it.next().copied(),
                    other => return Err(format!("unknown argument {other}").into()),
                }
            }
            let factory = FactoryImage {
                own_file,
                work_dir,
                proxied: proxy.is_some(),
                fetcher: Fetcher::google(proxy.clone()),
            };
            let dir = Path::new(dir);
            // `flow` is always a dry run on a pretend phone. `install` is
            // one only when told so, and the override is for nothing else:
            // refused from the arguments alone, with no USB device opened.
            if options.maintainer_skip_bootloader_slot_check && *command == "install" && !dry_run {
                return Err(Box::new(Error::OverrideOutsideDryRun));
            }
            if dry_run && write {
                return Err(
                    "--dry-run and --write are two different runs: give one of them".into(),
                );
            }
            // The one switch that lets the flow write. It is never implied.
            options.write = write;
            return match (*command, serial, fake) {
                ("flow", None, Some(scenario)) => {
                    cmd_fake(dir, scenario, options, factory, fake_google, proxy)
                }
                ("flow", ..) => {
                    Err("flow runs against a pretend phone only: --fake-phone STATE".into())
                }
                (_, _, Some(_)) => Err(
                    "install is for a real phone; the pretend phone is 'flow DIR --fake-phone STATE'"
                        .into(),
                ),
                _ if fake_google.is_some() => Err(
                    "a stand-in for Google's server is for the pretend phone only: 'flow DIR --fake-phone STATE'"
                        .into(),
                ),
                (_, None, _) => {
                    Err("--serial SERIAL is required: the install is pinned to one phone".into())
                }
                (_, Some(_), _) if !dry_run && !write => Err(Box::new(Error::Refused(
                    "install needs --dry-run or --write, and has no default. --dry-run asks the phone questions and writes nothing. --write erases the phone's userdata and writes slot B: 'omamobile-install help' says exactly what".into(),
                ))),
                (_, Some(serial), _) => cmd_install(dir, serial, options, factory),
            };
        }
        _ => return Err(USAGE.into()),
    }
    Ok(true)
}

fn main() -> ExitCode {
    // The JSON mode owns standard output: nothing below may print there
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("json") {
        let rest: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        return json::run(&rest);
    }
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
