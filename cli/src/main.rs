// SPDX-License-Identifier: MIT
//! Spike: a command-line front end of the installer core.
//!
//!   omamobile-install verify BUNDLE_DIR          no phone: signature, checksums, rules, every file
//!   omamobile-install sparse ROOT.img.zst OUTDIR [MAX_BYTES]
//!                                                no phone: the sparse images a flash would send
//!   omamobile-install devices                    USB: phones in fastboot mode
//!   omamobile-install install BUNDLE_DIR --serial SERIAL --dry-run
//!                     [--reinstall] [--bootloader-slots-match VERSION]
//!                                                USB: every check, read-only
//!
//! `install` runs only as a dry run here. The core's write path is complete
//! and tested against a fake phone, but this spike has no way to put the
//! owner's first-boot answers on the phone (INSTALL-EXPERIENCE-PLAN §4.4), so
//! a real write would install a system nobody can log in to.
use omamobile_installer_core::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use omamobile_installer_core::fastboot::{PhoneOptions, Session, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS};
use omamobile_installer_core::install::{flash_userdata_from_reader, UserdataStream};
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_core::Error;
use omamobile_installer_usb::UsbTransport;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn read(dir: &Path, name: &str) -> Res<Vec<u8>> {
    fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()).into())
}

/// The bundle's signature, checksums and rules; then every file it lists.
fn verify_bundle(dir: &Path) -> Res<VerifiedRelease> {
    let (sums, sig, keyring) = (read(dir, "SHA256SUMS")?, read(dir, "SHA256SUMS.sig")?, read(dir, "omamobile.gpg")?);
    let (manifest_json, manifest_txt, device_conf) = (read(dir, "manifest.json")?, read(dir, "MANIFEST")?, read(dir, "device.conf")?);
    let files = ReleaseFiles { sha256sums: &sums, sha256sums_sig: &sig, keyring: &keyring, manifest_json: &manifest_json, manifest_txt: &manifest_txt, device_conf: &device_conf };
    let release = verify_release(&files, &TrustedKeys::omamobile())?;
    println!("signed by {} (key {})", release.signer().primary, release.signer().signing_key);
    for f in &release.manifest().files {
        release.check_file(&f.path, &read(dir, &f.path)?)?;
    }
    println!("{} files match the signed manifest", release.manifest().files.len());
    Ok(release)
}

/// The root image sits beside the bundle directory, or inside it.
fn find_root(dir: &Path, name: &str) -> Res<PathBuf> {
    [dir.join("..").join(name), dir.join(name)].into_iter().find(|p| p.is_file()).ok_or_else(|| format!("no root image {name} beside {}", dir.display()).into())
}

fn hash_root(release: &VerifiedRelease, path: &Path) -> Res<omamobile_installer_core::bundle::VerifiedRootImage> {
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
    println!("Omarchy Mobile {} ({}) for {}", m.version, m.label, m.codename);
    match find_root(dir, &m.root_image.name) {
        Ok(path) => {
            hash_root(&release, &path)?;
            println!("root image matches the signed manifest");
        }
        Err(e) => println!("root image not checked: {e}"),
    }
    for w in &m.boot_writes {
        println!("  would flash {} from {} ({} bytes)", w.partition.name(), w.source, w.size);
    }
    println!("  would flash {} from {} (erases userdata)", m.root_image.partition.name(), m.root_image.name);
    Ok(())
}

fn cmd_sparse(image: &Path, out_dir: &Path, max: u64) -> Res<()> {
    fs::create_dir_all(out_dir)?;
    let mut file = File::open(image)?;
    let mut stream = UserdataStream::new(max);
    let (mut buf, mut count, mut bytes_in, mut bytes_out) = (vec![0u8; 256 << 10], 0usize, 0u64, 0u64);
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
    info.interfaces().any(|i| (i.class(), i.subclass(), i.protocol()) == (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL))
}

async fn cmd_devices() -> Res<()> {
    let mut n = 0;
    for info in nusb::list_devices().await?.filter(is_fastboot) {
        n += 1;
        println!("{:04x}:{:04x} {} serial {}", info.vendor_id(), info.product_id(), info.product_string().unwrap_or("?"), info.serial_number().unwrap_or("?"));
    }
    println!("{n} phone(s) in fastboot mode");
    Ok(())
}

async fn cmd_install(dir: &Path, serial: &str, options: PhoneOptions) -> Res<()> {
    let release = verify_bundle(dir)?;
    let m = release.manifest();
    // Everything is read and checked before the phone is opened
    let mut images = Vec::new();
    for w in &m.boot_writes {
        images.push(read(dir, &w.source)?);
    }
    let root_path = find_root(dir, &m.root_image.name)?;
    let root = hash_root(&release, &root_path)?;
    println!("root image matches the signed manifest");

    // Exactly one device with this serial number, in fastboot mode
    let mut found = nusb::list_devices().await?.filter(|d| is_fastboot(d) && d.serial_number() == Some(serial));
    let (Some(info), None) = (found.next(), found.next()) else {
        return Err("expected exactly one phone in fastboot mode with that serial number".into());
    };
    let device = info.open().await?;
    let transport = UsbTransport::open(&device).await?;
    let mut session = Session::connect(transport, &release, serial, options).await?;
    let facts = session.facts().clone();
    println!("{} unlocked, slot {}, bootloader {}", facts.product, facts.current_slot, facts.bootloader);
    println!("These commands, and only these, would run:");
    session.plan().iter().for_each(|l| println!("    {l}"));

    for (w, bytes) in m.boot_writes.iter().zip(&images) {
        session.flash_boot(&release.verify_boot_image(w, bytes)?).await?;
    }
    flash_userdata_from_reader(&mut session, &root, File::open(&root_path)?, Some(64 << 20)).await?;
    session.activate_and_reboot().await?;
    session.log().iter().filter(|l| l.starts_with("would:")).for_each(|l| println!("    {l}"));
    println!("Dry run done: nothing was written to the phone");
    Ok(())
}

fn run() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["verify", dir] => cmd_verify(Path::new(dir)),
        ["sparse", image, out] => cmd_sparse(Path::new(image), Path::new(out), 256 << 20),
        ["sparse", image, out, max] => cmd_sparse(Path::new(image), Path::new(out), max.parse()?),
        ["bench-zstd", file, rounds] => {
            // Decode a small .zst repeatedly: the same loop as the browser self-test
            let (data, mut bytes, start) = (fs::read(file)?, 0u64, Instant::now());
            for _ in 0..rounds.parse::<u32>()? {
                omamobile_installer_core::zstd_push::ZstdPush::new().push(&data, &mut |d| {
                    bytes += d.len() as u64;
                    Ok(())
                })?;
            }
            println!("{} MiB in {:.0} ms: {:.0} MiB/s", bytes >> 20, start.elapsed().as_secs_f64() * 1e3, bytes as f64 / 1048576.0 / start.elapsed().as_secs_f64());
            Ok(())
        }
        ["devices"] => pollster::block_on(cmd_devices()),
        ["install", dir, rest @ ..] => {
            let (mut serial, mut options, mut it) = (None, PhoneOptions::default(), rest.iter());
            while let Some(arg) = it.next() {
                match *arg {
                    "--serial" => serial = it.next().copied(),
                    "--dry-run" => options.dry_run = true,
                    "--reinstall" => options.reinstall = true,
                    "--bootloader-slots-match" => options.bootloader_slots_confirmed = it.next().map(|s| s.to_string()),
                    other => return Err(format!("unknown argument {other}").into()),
                }
            }
            let serial = serial.ok_or("--serial SERIAL is required: the install is pinned to one phone")?;
            if !options.dry_run {
                return Err(Box::new(Error::Refused("this spike installs only with --dry-run: it can't deliver the owner's first-boot answers yet".into())));
            }
            pollster::block_on(cmd_install(Path::new(dir), serial, options))
        }
        _ => Err("usage: omamobile-install verify DIR | sparse IMG.zst OUTDIR [MAX] | devices | install DIR --serial S --dry-run [--reinstall] [--bootloader-slots-match V]".into()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
