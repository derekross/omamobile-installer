// SPDX-License-Identifier: MIT
//! The command-line front end of the installer core.
//!
//!   omamobile-install verify BUNDLE_DIR          no phone: signature, checksums, rules, every file
//!   omamobile-install sparse ROOT.img.zst OUTDIR [MAX_BYTES]
//!                                                no phone: the sparse images a flash would send
//!   omamobile-install devices                    USB: phones in fastboot mode
//!   omamobile-install install BUNDLE_DIR --serial SERIAL --dry-run [--reinstall]
//!                             [--factory-zip FILE] [--work-dir DIR]
//!                                                USB: the install flow up to the dry run, read-only
//!   omamobile-install flow BUNDLE_DIR --fake-phone STATE [--reinstall] [--factory-zip FILE]
//!                             [--work-dir DIR] [--fake-google 127.0.0.1:PORT]
//!                                                no phone: the same flow against a pretend phone
//!                                                (only in a build with the `fake-phone` feature)
//!   omamobile-install json [--release BUNDLE_DIR] [--work-dir DIR]
//!                                                USB: the same flow as JSON lines on standard input
//!                                                and output, for the desktop app, agents and scripts
//!                                                (json.rs, docs/APP.md)
//!
//! `install`, `flow` and `json` drive the core's one install flow
//! (core/src/flow.rs): this file prints the screens the flow gives it and
//! hands in what was typed, and json.rs sends them as they are. Neither
//! decides anything or words anything of its own about the phone.
//!
//! The flow ends at the dry run, and `install` runs only with `--dry-run`:
//! nothing here can write to a phone. All that reaches one is `getvar` and,
//! for a locked phone, the one question `flashing get_unlock_ability`.
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
//! `flow` never reaches Google: its downloads go to the stand-in server
//! named with `--fake-google`, on this computer, or nowhere.
//!
//! Not in the usage text, and for the maintainer only: `install` and `flow`
//! take `--maintainer-skip-bootloader-slot-check`, which lets a dry run go on
//! past a phone that reports one bootloader slot's version (docs/DESIGN.md
//! §6). The core says on every screen that the check was skipped; without
//! `--dry-run` it is refused here, before any USB device is looked for.
use omamobile_installer_core::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use omamobile_installer_core::download::{
    FactoryDownload, FetchFailure, FetchNext, FetchRequest, FetchResponse,
};
use omamobile_installer_core::fastboot::{
    PhoneId, Session, Transport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS,
};
use omamobile_installer_core::flow::{
    AnswersInput, DownloadEnd, Flow, FlowOptions, OwnerAnswers, Screen,
};
use omamobile_installer_core::install::{flash_userdata_from_reader, UserdataStream};
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_core::{build_index, Error};
use omamobile_installer_usb::UsbTransport;
use pollster::block_on;
use std::fs::{self, File};
use std::io::{self, BufRead, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
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

fn hash_root(
    release: &VerifiedRelease,
    path: &Path,
) -> Res<omamobile_installer_core::bundle::VerifiedRootImage> {
    hash_root_from(release, File::open(path)?)
}

fn hash_root_from(
    release: &VerifiedRelease,
    mut file: impl Read,
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
            hash_root(&release, &path)?;
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
        println!(
            "{:04x}:{:04x} {} serial {}",
            info.vendor_id(),
            info.product_id(),
            info.product_string().unwrap_or("?"),
            info.serial_number().unwrap_or("?")
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

/// Walk the release's images through the dry run's session, when they are
/// on disk: each is checked against the signed manifest first, and the
/// session holds every write back. `Ok(false)`: the files aren't here.
async fn walk_images<T: Transport>(
    session: &mut Session<T>,
    release: &VerifiedRelease,
    dir: &Path,
    watch: &mut dyn Watch,
) -> Result<bool, Error> {
    let m = release.manifest();
    let Some(root_path) = find_root(dir, &m.root_image.name) else {
        return Ok(false);
    };
    if !m.boot_writes.iter().all(|w| dir.join(&w.source).is_file()) {
        return Ok(false);
    }
    let io = |e: io::Error| Error::Transport(format!("reading the release's files: {e}"));
    // The bytes to get through: each boot image once, and the root image
    // twice, to check it and then to cut it
    let root_size = fs::metadata(&root_path).map_err(io)?.len();
    let total = m.boot_writes.iter().map(|w| w.size).sum::<u64>() + 2 * root_size;
    let mut done = 0;
    watch.progress("dry-run", done, total);
    for w in &m.boot_writes {
        let bytes = fs::read(dir.join(&w.source)).map_err(io)?;
        session
            .flash_boot(&release.verify_boot_image(w, &bytes)?)
            .await?;
        done += bytes.len() as u64;
        watch.progress("dry-run", done, total);
    }
    let root = {
        let file = File::open(&root_path).map_err(io)?;
        let reader = Counted::new(file, done, total, &mut *watch);
        hash_root_from(release, reader).map_err(|e| Error::Checksum(e.to_string()))?
    };
    let reader = Counted::new(
        File::open(&root_path).map_err(io)?,
        done + root_size,
        total,
        watch,
    );
    flash_userdata_from_reader(session, &root, reader, Some(64 << 20)).await?;
    session.activate_and_reboot().await?;
    Ok(true)
}

/// A file of the release as the dry run reads it, counted for the progress.
struct Counted<'a, R> {
    inner: R,
    done: u64,
    total: u64,
    watch: &'a mut dyn Watch,
}

impl<'a, R> Counted<'a, R> {
    fn new(inner: R, done: u64, total: u64, watch: &'a mut dyn Watch) -> Self {
        Counted {
            inner,
            done,
            total,
            watch,
        }
    }
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.done += n as u64;
        self.watch.progress("dry-run", self.done, self.total);
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

fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
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
    fn google() -> Self {
        Fetcher::Google(agent(true))
    }

    /// For a pretend run, which never reaches Google: a stand-in on this
    /// computer answers in its place, or nothing does.
    #[cfg(feature = "fake-phone")]
    fn pretend(fake_google: Option<&str>) -> Res<Self> {
        let Some(at) = fake_google else {
            return Ok(Fetcher::Nowhere);
        };
        let at: std::net::SocketAddr = at
            .parse()
            .map_err(|_| "--fake-google takes an address like 127.0.0.1:8080")?;
        if !at.ip().is_loopback() {
            return Err("--fake-google takes an address on this computer only".into());
        }
        Ok(Fetcher::StandIn(agent(false), at))
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

/// Carry out what one try at the download asks: the kept bytes first, then
/// the requests, the body into `file`. Every decision is `download`'s.
/// `watch` is told how far it is, and may end the transfer between pieces:
/// the core then finds fewer bytes than the file has, as when a transfer
/// broke off.
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
                // The body is read on a thread of its own, so that a
                // transfer that stands still can still be ended from here
                let mut body = response.into_body().into_reader();
                let (pieces, arriving) = mpsc::sync_channel::<io::Result<Vec<u8>>>(4);
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; 1 << 20];
                    loop {
                        let piece = match body.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => Ok(buf[..n].to_vec()),
                            Err(e) => Err(e),
                        };
                        let failed = piece.is_err();
                        if pieces.send(piece).is_err() || failed {
                            break;
                        }
                    }
                });
                while watch.go_on("download") {
                    let piece = match arriving.recv_timeout(Duration::from_millis(200)) {
                        Ok(piece) => piece.map_err(|e| FetchFailure::Network(e.to_string()))?,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        // The body's end
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
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
    watch.note(&format!("downloading {name} into {}", dir.display()));
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
                    let mut quiet = Terminal::default();
                    match block_on(walk_images(&mut session, &release, &bundle.dir, &mut quiet)) {
                        Ok(_) => {
                            flow.dry_run_done(session)?;
                        }
                        Err(e) => flow.dry_run_failed(&e)?,
                    }
                }
            }
            _ => return Ok(screen.state == "done"),
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
) -> Res<bool> {
    use fake::{FakePhone, SERIAL};
    factory.fetcher = Fetcher::pretend(fake_google)?;
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
    })?;
    let log = phone.log();
    let (asked, other) = (phone.unlock_questions(), phone.not_reads().len());
    println!(
        "\nThe pretend phone received {} commands: {} getvar, {asked} flashing get_unlock_ability, {other} others; partitions written: {}",
        log.len(),
        log.len() - asked - other,
        phone.written().len()
    );
    Ok(done)
}

#[cfg(not(feature = "fake-phone"))]
fn cmd_fake(_: &Path, _: &str, _: FlowOptions, _: FactoryImage, _: Option<&str>) -> Res<bool> {
    Err("this build has no pretend phone: build with --features fake-phone".into())
}

const USAGE: &str = "usage: omamobile-install verify DIR | sparse IMG.zst OUTDIR [MAX] | devices | install DIR --serial S --dry-run [--reinstall] [--factory-zip FILE] [--work-dir DIR] | flow DIR --fake-phone STATE [--reinstall] [--factory-zip FILE] [--work-dir DIR] [--fake-google 127.0.0.1:PORT] | json [--release DIR] [--work-dir DIR]";

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
        [command @ ("install" | "flow"), dir, rest @ ..] => {
            let (mut serial, mut fake, mut dry_run) = (None, None, false);
            let (mut options, mut fake_google) = (FlowOptions::default(), None);
            let mut factory = FactoryImage {
                own_file: None,
                work_dir: None,
                fetcher: Fetcher::google(),
            };
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match *arg {
                    "--serial" => serial = it.next().copied(),
                    "--fake-phone" => fake = it.next().copied(),
                    "--dry-run" => dry_run = true,
                    "--reinstall" => options.reinstall = true,
                    SLOT_OVERRIDE => options.maintainer_skip_bootloader_slot_check = true,
                    "--factory-zip" => factory.own_file = it.next().map(PathBuf::from),
                    "--work-dir" => factory.work_dir = it.next().map(PathBuf::from),
                    "--fake-google" => fake_google = it.next().copied(),
                    other => return Err(format!("unknown argument {other}").into()),
                }
            }
            let dir = Path::new(dir);
            // `flow` is always a dry run on a pretend phone. `install` is
            // one only when told so, and the override is for nothing else:
            // refused from the arguments alone, with no USB device opened.
            if options.maintainer_skip_bootloader_slot_check && *command == "install" && !dry_run {
                return Err(Box::new(Error::OverrideOutsideDryRun));
            }
            return match (*command, serial, fake) {
                ("flow", None, Some(scenario)) => {
                    cmd_fake(dir, scenario, options, factory, fake_google)
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
                (_, Some(_), _) if !dry_run => Err(Box::new(Error::Refused(
                    "this version installs only with --dry-run: its flow ends at the dry run, and it has no step that writes to a phone".into(),
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
