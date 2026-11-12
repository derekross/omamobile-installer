// SPDX-License-Identifier: MIT
//! The command-line front end of the installer core.
//!
//!   omamobile-install verify BUNDLE_DIR          no phone: signature, checksums, rules, every file
//!   omamobile-install sparse ROOT.img.zst OUTDIR [MAX_BYTES]
//!                                                no phone: the sparse images a flash would send
//!   omamobile-install devices                    USB: phones in fastboot mode
//!   omamobile-install install BUNDLE_DIR --serial SERIAL --dry-run [--reinstall] [--factory-zip FILE]
//!                                                USB: the install flow up to the dry run, read-only
//!   omamobile-install flow BUNDLE_DIR --fake-phone STATE [--reinstall] [--factory-zip FILE]
//!                                                no phone: the same flow against a pretend phone
//!                                                (only in a build with the `fake-phone` feature)
//!
//! `install` and `flow` drive the core's one install flow (core/src/flow.rs):
//! this file prints the screens the flow gives it and hands in what was
//! typed. It decides nothing and words nothing of its own about the phone.
//!
//! The flow ends at the dry run, and `install` runs only with `--dry-run`:
//! nothing here can write to a phone. All that reaches one is `getvar` and,
//! for a locked phone, the one question `flashing get_unlock_ability`.
use omamobile_installer_core::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use omamobile_installer_core::fastboot::{
    PhoneId, Session, Transport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS,
};
use omamobile_installer_core::flow::{AnswersInput, Flow, FlowOptions, OwnerAnswers, Screen};
use omamobile_installer_core::install::{flash_userdata_from_reader, UserdataStream};
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_core::{build_index, Error};
use omamobile_installer_usb::UsbTransport;
use pollster::block_on;
use std::fs::{self, File};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

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
    let mut check = release.root_image_check();
    let mut file = File::open(path)?;
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
    for p in &screen.body {
        println!();
        wrap(p, "    ", "    ");
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
        wrap(problem, "    ", "    Not taken: ");
    }
    if !screen.footer.is_empty() {
        println!();
    }
    for line in &screen.footer {
        wrap(line, "    ", "    ");
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
) -> Result<bool, Error> {
    let m = release.manifest();
    let Some(root_path) = find_root(dir, &m.root_image.name) else {
        return Ok(false);
    };
    if !m.boot_writes.iter().all(|w| dir.join(&w.source).is_file()) {
        return Ok(false);
    }
    let io = |e: io::Error| Error::Transport(format!("reading the release's files: {e}"));
    for w in &m.boot_writes {
        let bytes = fs::read(dir.join(&w.source)).map_err(io)?;
        session
            .flash_boot(&release.verify_boot_image(w, &bytes)?)
            .await?;
    }
    let root = hash_root(release, &root_path).map_err(|e| Error::Checksum(e.to_string()))?;
    let reader = File::open(&root_path).map_err(io)?;
    flash_userdata_from_reader(session, &root, reader, Some(64 << 20)).await?;
    session.activate_and_reboot().await?;
    Ok(true)
}

/// Drive the core's flow to its end. `connect` finds the phone and opens
/// it, or says there is none; nothing else here touches a phone.
fn run_flow<T: Transport>(
    bundle: &Bundle,
    trusted: &TrustedKeys,
    options: FlowOptions,
    mut factory_zip: Option<PathBuf>,
    mut connect: impl FnMut() -> Res<Option<(T, PhoneId)>>,
) -> Res<bool> {
    let mut typed = Typed::new();
    let mut flow = Flow::begin(&bundle.files(), bundle.index.as_deref(), trusted, options);
    let mut looked = false;
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
            "take-file" => {
                let path = match factory_zip.take() {
                    Some(path) => path,
                    None => PathBuf::from(typed.ask("Where did you save the file? ")?),
                };
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let size = match fs::metadata(&path) {
                    Ok(meta) if meta.is_file() => meta.len(),
                    _ => {
                        println!("    There is no file {}", path.display());
                        continue;
                    }
                };
                if let Some(mut check) = flow.factory_zip(name, size)? {
                    println!("    checking {name}");
                    // Streamed: a megabyte at a time, never the whole file
                    let mut file = File::open(&path)?;
                    let mut buf = vec![0u8; 1 << 20];
                    loop {
                        let n = file.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        check.update(&buf[..n]);
                    }
                    flow.factory_zip_done(check)?;
                }
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
                    match block_on(walk_images(&mut session, &release, &bundle.dir)) {
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

fn cmd_install(
    dir: &Path,
    serial: &str,
    options: FlowOptions,
    factory_zip: Option<PathBuf>,
) -> Res<bool> {
    let bundle = Bundle::load(dir, "omamobile.gpg")?;
    run_flow(
        &bundle,
        &TrustedKeys::omamobile(),
        options,
        factory_zip,
        || block_on(open_usb(serial)),
    )
}

/// The same flow against the tests' pretend phone, held in memory. A bundle
/// for it is signed by a test key, which only this command trusts, and only
/// together with the pretend phone: there is no way from here to USB.
#[cfg(feature = "fake-phone")]
fn cmd_fake(
    dir: &Path,
    scenario: &str,
    options: FlowOptions,
    factory_zip: Option<PathBuf>,
) -> Res<bool> {
    use fake::{FakePhone, SERIAL};
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
    let done = run_flow(&bundle, &trusted, options, factory_zip, || {
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
fn cmd_fake(_: &Path, _: &str, _: FlowOptions, _: Option<PathBuf>) -> Res<bool> {
    Err("this build has no pretend phone: build with --features fake-phone".into())
}

const USAGE: &str = "usage: omamobile-install verify DIR | sparse IMG.zst OUTDIR [MAX] | devices | install DIR --serial S --dry-run [--reinstall] [--factory-zip FILE] | flow DIR --fake-phone STATE [--reinstall] [--factory-zip FILE]";

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
            let (mut options, mut zip) = (FlowOptions::default(), None);
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match *arg {
                    "--serial" => serial = it.next().copied(),
                    "--fake-phone" => fake = it.next().copied(),
                    "--dry-run" => dry_run = true,
                    "--reinstall" => options.reinstall = true,
                    "--factory-zip" => zip = it.next().map(PathBuf::from),
                    other => return Err(format!("unknown argument {other}").into()),
                }
            }
            let dir = Path::new(dir);
            return match (*command, serial, fake) {
                ("flow", None, Some(scenario)) => cmd_fake(dir, scenario, options, zip),
                ("flow", ..) => {
                    Err("flow runs against a pretend phone only: --fake-phone STATE".into())
                }
                (_, _, Some(_)) => Err(
                    "install is for a real phone; the pretend phone is 'flow DIR --fake-phone STATE'"
                        .into(),
                ),
                (_, None, _) => {
                    Err("--serial SERIAL is required: the install is pinned to one phone".into())
                }
                (_, Some(_), _) if !dry_run => Err(Box::new(Error::Refused(
                    "this version installs only with --dry-run: its flow ends at the dry run, and it has no step that writes to a phone".into(),
                ))),
                (_, Some(serial), _) => cmd_install(dir, serial, options, zip),
            };
        }
        _ => return Err(USAGE.into()),
    }
    Ok(true)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
