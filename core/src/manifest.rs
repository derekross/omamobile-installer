// SPDX-License-Identifier: MIT
//! `manifest.json`, format 1 (docs/RELEASES.md §6): the release and its
//! install plan as data. The rules here are the ones `tools/install-phone`
//! applies: a manifest can confirm what is written, never widen it.
//!
//! The partitions an install may write are fixed in this file
//! ([`SLOT_B_IMAGES`] with `_b`, and `userdata`). A manifest naming anything
//! else is refused when it is parsed, and [`Partition`] can't be built any
//! other way, so the fastboot layer has no name to send for another one.
use crate::device_conf::DeviceConf;
use crate::hash::is_sha256_hex;
use crate::{Error, Result};
use serde::Deserialize;

/// The boot images a descriptor may name (docs/RELEASES.md §3.3).
pub const SLOT_B_IMAGES: [&str; 5] = [
    "boot",
    "init_boot",
    "vendor_boot",
    "vendor_kernel_boot",
    "dtbo",
];
/// The images every device has.
const REQUIRED_IMAGES: [&str; 3] = ["boot", "vendor_boot", "dtbo"];
pub const FORMAT: u64 = 1;
/// The files a release's root image keeps a place for, for the owner's
/// answers (Omarchy Mobile's `rootfs/placeholders`, RELEASES §6.1): the
/// first-boot answers and the boot hook's switch. Each is one block.
pub const PLACEHOLDER_PATHS: [&str; 2] = [
    "/etc/omamobile/firstboot.conf",
    "/etc/omamobile/boot-update.conf",
];
/// A placeholder's size: one 4096-byte block of the ext4 image, at a block
/// boundary, so it is one whole block of the sparse images too.
pub const PLACEHOLDER_LEN: u64 = 4096;

/// A partition this installer may write. Only this module makes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Partition(String);

impl Partition {
    fn slot_b(image: &str) -> Option<Self> {
        SLOT_B_IMAGES
            .contains(&image)
            .then(|| Partition(format!("{image}_b")))
    }
    fn userdata() -> Self {
        Partition("userdata".into())
    }
    pub fn name(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
struct Raw {
    format: u64,
    version: String,
    label: String,
    device: RawDevice,
    kernel: RawKernel,
    root_image: RawRoot,
    install: RawInstall,
    files: Vec<RawFile>,
    #[serde(default)]
    base_url: Option<String>,
}
#[derive(Debug, Deserialize)]
struct RawDevice {
    codename: String,
    #[serde(default)]
    name: Option<String>,
}
#[derive(Debug, Deserialize)]
struct RawKernel {
    release: String,
}
#[derive(Debug, Deserialize)]
struct RawRoot {
    name: String,
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    compression: Option<String>,
    /// Absent in releases made before the placeholders: none.
    #[serde(default)]
    placeholders: Vec<RawPlaceholder>,
}
// What bytes an installer may change in the root image: closed, as the
// install plan is. A key this installer doesn't know could be a rule about
// those bytes that it would ignore.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlaceholder {
    path: String,
    offset: u64,
    length: u64,
    sha256: String,
}
// The install plan is closed: a key this installer doesn't know could be a
// rule it would ignore, so an unknown one is refused (stricter than
// install-phone, which ignores them).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstall {
    product: String,
    require_unlocked_bootloader: bool,
    require_matching_bootloader_slots: bool,
    storage_guard_allowlist: Vec<String>,
    after: Vec<String>,
    writes: Vec<RawWrite>,
    #[serde(default)]
    tool: Option<String>,
    // Not written by any release yet: read so that a release which states
    // the rule can be installed by this version (docs/DESIGN.md).
    #[serde(default)]
    bootloader_slot_probe: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)] // documentation for people; the rules are in code
    never: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWrite {
    partition: String,
    source: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    partition_size: Option<u64>,
    #[serde(default)]
    erases_userdata: bool,
}
#[derive(Debug, Deserialize)]
struct RawFile {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// One slot B boot image write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootWrite {
    pub image: String,
    pub partition: Partition,
    /// Path inside the bundle, `images/<image>.img`.
    pub source: String,
    pub size: u64,
    pub sha256: String,
    /// The partition's exact size when the image carries an AVB footer.
    pub partition_size: Option<u64>,
}

/// One place the root image keeps for the owner's answers: a file whose
/// data is `length` bytes at `offset` of the raw (unpacked) image, holding
/// bytes with this SHA-256 as the release built it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// One of [`PLACEHOLDER_PATHS`].
    pub path: String,
    pub offset: u64,
    pub length: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootImage {
    pub name: String,
    pub sha256: String,
    /// Compressed size, when the manifest gives one.
    pub size: Option<u64>,
    pub partition: Partition,
    /// In the order of their offsets. Empty for a release made before the
    /// placeholders: such a release has no place for the owner's answers.
    pub placeholders: Vec<Placeholder>,
}

impl RootImage {
    /// The placeholder for `path`, if the release has one.
    pub fn placeholder(&self, path: &str) -> Option<&Placeholder> {
        self.placeholders.iter().find(|p| p.path == path)
    }
}

/// A manifest that passed every rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub label: String,
    pub codename: String,
    pub device_name: Option<String>,
    pub kernel_release: String,
    pub base_url: Option<String>,
    pub files: Vec<FileEntry>,
    pub allowlist: Vec<String>,
    pub boot_writes: Vec<BootWrite>,
    pub root_image: RootImage,
    /// `install.bootloader_slot_probe`, when the manifest states it. It must
    /// agree with the descriptor's `BOOTLOADER_SLOT_PROBE`.
    pub bootloader_slot_probe: Option<bool>,
}

fn err<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Manifest(why.into()))
}

fn all(s: &str, ok: impl Fn(u8) -> bool) -> bool {
    !s.is_empty() && s.bytes().all(ok)
}
fn is_name(s: &str) -> bool {
    all(s, |b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}
fn is_codename(s: &str) -> bool {
    all(s, |b| b.is_ascii_lowercase() || b.is_ascii_digit())
}
fn is_path(s: &str) -> bool {
    s.split('/').all(|part| {
        all(part, |b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && part != ".."
            && part != "."
    })
}

/// `YYYY.MM.DD` with an optional `.N`, as a signed release is versioned.
pub fn is_calendar_version(v: &str) -> bool {
    let mut parts = v.split('.');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let num = |s: &str, len: usize| {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    let ok = matches!(num(y, 4), Some(2000..=2099))
        && matches!(num(m, 2), Some(1..=12))
        && matches!(num(d, 2), Some(1..=31));
    let rev_ok = match parts.next() {
        None => true,
        Some(r) => !r.starts_with('0') && all(r, |b| b.is_ascii_digit()),
    };
    ok && rev_ok && parts.next().is_none()
}

impl Manifest {
    pub fn parse(json: &[u8]) -> Result<Self> {
        let raw: Raw = serde_json::from_slice(json)
            .map_err(|e| Error::Manifest(format!("not a release manifest ({e})")))?;
        if raw.format != FORMAT {
            return err(format!(
                "format {}; this installer reads format {FORMAT}",
                raw.format
            ));
        }
        if !is_name(&raw.version) {
            return err("version is not valid");
        }
        if !all(&raw.label, |b| {
            b.is_ascii_alphanumeric() || b" ._-".contains(&b)
        }) {
            return err("label is not valid");
        }
        if !is_codename(&raw.device.codename) {
            return err("device.codename is not valid");
        }
        if !is_name(&raw.kernel.release) {
            return err("kernel.release is not valid");
        }
        let codename = raw.device.codename;
        let i = raw.install;
        if i.product != codename {
            return err(format!(
                "is for {codename}, but install.product is {}",
                i.product
            ));
        }
        if !i.require_unlocked_bootloader {
            return err("must require an unlocked bootloader");
        }
        if !i.require_matching_bootloader_slots {
            return err("must require both bootloader slots to match");
        }
        if i.tool.as_deref().is_some_and(|t| t != "fastboot") {
            return err("install.tool must be fastboot");
        }
        if i.after != ["set-active b", "reboot"] {
            return err(format!(
                "must end with set-active b and reboot, nothing else (has: {})",
                i.after.join(",")
            ));
        }

        // Files: plain relative paths, each once.
        let mut files = Vec::with_capacity(raw.files.len());
        for f in raw.files {
            if !is_path(&f.path) {
                return err("a file's path is not valid");
            }
            if !is_sha256_hex(&f.sha256) {
                return err(format!("{}: not a SHA-256", f.path));
            }
            if files.iter().any(|e: &FileEntry| e.path == f.path) {
                return err(format!("{} is listed twice", f.path));
            }
            files.push(FileEntry {
                path: f.path,
                size: f.size,
                sha256: f.sha256,
            });
        }

        // The allowlist: only slot B's boot images and userdata, each once.
        let allow = i.storage_guard_allowlist;
        for (n, p) in allow.iter().enumerate() {
            let known = p == "userdata"
                || p.strip_suffix("_b")
                    .is_some_and(|img| SLOT_B_IMAGES.contains(&img));
            if !known {
                return err(format!(
                    "the allowlist may hold only slot B's boot images and userdata, not {p}"
                ));
            }
            if allow[..n].contains(p) {
                return err(format!("the allowlist names {p} twice"));
            }
        }
        if !allow.iter().any(|p| p == "userdata") {
            return err("userdata is not on the allowlist");
        }

        // The writes: slot B's images, then userdata with the root image, last.
        let Some((last, boots)) = i.writes.split_last() else {
            return err("no writes");
        };
        let mut boot_writes: Vec<BootWrite> = Vec::with_capacity(boots.len());
        for w in boots {
            let image = w.partition.strip_suffix("_b").unwrap_or("");
            let Some(partition) = Partition::slot_b(image) else {
                return err(format!(
                    "refusing a write to {}: not a slot B boot image",
                    w.partition
                ));
            };
            if w.source != format!("images/{image}.img") {
                return err(format!(
                    "{} must be written from images/{image}.img, not {}",
                    w.partition, w.source
                ));
            }
            if w.erases_userdata {
                return err(format!("{} can't erase userdata", w.partition));
            }
            if !allow.contains(&w.partition) {
                return err(format!("{} is not on the allowlist", w.partition));
            }
            if boot_writes.iter().any(|b| b.image == image) {
                return err(format!("{} is written twice", w.partition));
            }
            let Some(size) = w.size.filter(|s| *s > 0) else {
                return err(format!("{}: no size", w.partition));
            };
            if w.partition_size.is_some_and(|p| p != size) {
                // An AVB footer image is padded to the partition (RELEASES §3)
                return err(format!(
                    "{}: a footer image must be exactly its partition's size",
                    w.partition
                ));
            }
            let Some(file) = files.iter().find(|f| f.path == w.source) else {
                return err(format!("{} is not among the bundle's files", w.source));
            };
            if file.size != size {
                return err(format!("{}: the write's size is not the file's", w.source));
            }
            boot_writes.push(BootWrite {
                image: image.to_string(),
                partition,
                source: w.source.clone(),
                size,
                sha256: file.sha256.clone(),
                partition_size: w.partition_size,
            });
        }
        for need in REQUIRED_IMAGES {
            if !boot_writes.iter().any(|b| b.image == need) {
                return err(format!("{need}_b is not written"));
            }
        }
        // Every *_b name on the allowlist is written, so the two lists agree
        for p in allow.iter().filter(|p| *p != "userdata") {
            if !boot_writes.iter().any(|b| b.partition.name() == p) {
                return err(format!("{p} is on the allowlist but not written"));
            }
        }
        if last.partition != "userdata" || last.source != "root_image" || !last.erases_userdata {
            return err(
                "the last write must be userdata from the root image, marked as erasing userdata",
            );
        }
        if last.size.is_some() || last.partition_size.is_some() {
            return err("the userdata write carries no size");
        }

        let r = raw.root_image;
        let root_name_ok = r
            .name
            .strip_suffix(".img.zst")
            .is_some_and(|stem| all(stem, |b| b.is_ascii_alphanumeric() || b"._-".contains(&b)));
        if !root_name_ok {
            return err("root_image.name is not valid");
        }
        if !is_sha256_hex(&r.sha256) {
            return err("root_image.sha256 is not valid");
        }
        if r.compression.as_deref().is_some_and(|c| c != "zstd") {
            return err("root_image.compression must be zstd");
        }
        let mut placeholders: Vec<Placeholder> = Vec::with_capacity(r.placeholders.len());
        for p in r.placeholders {
            if !PLACEHOLDER_PATHS.contains(&p.path.as_str()) {
                return err(format!(
                    "root_image.placeholders names {}, which this installer doesn't fill",
                    p.path
                ));
            }
            if placeholders.iter().any(|q| q.path == p.path) {
                return err(format!("root_image.placeholders names {} twice", p.path));
            }
            if p.length != PLACEHOLDER_LEN || !p.offset.is_multiple_of(PLACEHOLDER_LEN) {
                return err(format!(
                    "{}'s placeholder is not one {PLACEHOLDER_LEN}-byte block at a block boundary",
                    p.path
                ));
            }
            if !is_sha256_hex(&p.sha256) {
                return err(format!("{}'s placeholder has no SHA-256", p.path));
            }
            if placeholders.iter().any(|q| q.offset == p.offset) {
                return err("two placeholders are at the same place");
            }
            placeholders.push(Placeholder {
                path: p.path,
                offset: p.offset,
                length: p.length,
                sha256: p.sha256,
            });
        }
        placeholders.sort_by_key(|p| p.offset);
        // The answers themselves go to firstboot.conf: a release that keeps
        // places for others but not for it has none for the answers
        if !placeholders.is_empty() && !placeholders.iter().any(|p| p.path == PLACEHOLDER_PATHS[0])
        {
            return err(format!(
                "root_image.placeholders has none for {}",
                PLACEHOLDER_PATHS[0]
            ));
        }

        Ok(Manifest {
            version: raw.version,
            label: raw.label,
            codename,
            device_name: raw.device.name,
            kernel_release: raw.kernel.release,
            base_url: raw.base_url,
            files,
            allowlist: allow,
            boot_writes,
            bootloader_slot_probe: i.bootloader_slot_probe,
            root_image: RootImage {
                name: r.name,
                sha256: r.sha256,
                size: r.size,
                partition: Partition::userdata(),
                placeholders,
            },
        })
    }

    /// The manifest must agree exactly with the descriptor the images were
    /// built from, as `install-phone` checks.
    pub fn check_descriptor(&self, conf: &DeviceConf) -> Result<()> {
        let bad = |why: String| Err(Error::DeviceConf(why));
        let codename = conf.get("CODENAME").unwrap_or("");
        if codename != self.codename {
            return bad(format!(
                "CODENAME={codename}, but manifest.json is for {}",
                self.codename
            ));
        }
        let method = conf.get("BOOT_METHOD").unwrap_or("android-v4");
        if method != "android-v4" {
            return bad(format!(
                "unsupported BOOT_METHOD={method}; only android-v4 can be installed"
            ));
        }
        match (
            conf.get("SOC_IDS").is_some_and(|v| !v.is_empty()),
            conf.get("SOC_COMPATIBLE").is_some_and(|v| !v.is_empty()),
        ) {
            (true, true) => return bad("SOC_IDS and SOC_COMPATIBLE are exclusive".into()),
            (false, false) => {
                return bad(
                    "SOC_IDS is missing (or SOC_COMPATIBLE); this device can't be installed yet"
                        .into(),
                )
            }
            _ => {}
        }
        // The slot probe changes what an install sends to the phone, so the
        // signed plan and the descriptor may not disagree about it.
        let probe = conf.bootloader_slot_probe()?;
        if self.bootloader_slot_probe.is_some_and(|m| m != probe) {
            return bad(format!(
                "BOOTLOADER_SLOT_PROBE ({}) is not manifest.json's install.bootloader_slot_probe",
                if probe { "yes" } else { "no or unset" }
            ));
        }
        let images = conf.list("BOOT_IMAGES");
        let ours: Vec<&str> = self.boot_writes.iter().map(|b| b.image.as_str()).collect();
        if images != ours {
            return bad(format!(
                "BOOT_IMAGES ({}) is not what manifest.json writes ({})",
                images.join(" "),
                ours.join(" ")
            ));
        }
        let ufs_rw = conf.list("UFS_RW");
        if ufs_rw
            != self
                .allowlist
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        {
            return bad(format!(
                "UFS_RW ({}) is not manifest.json's allowlist ({})",
                ufs_rw.join(" "),
                self.allowlist.join(" ")
            ));
        }
        let footers = conf.avb_footers()?;
        for (image, _) in &footers {
            if !ours.contains(&image.as_str()) {
                return bad(format!(
                    "AVB_FOOTERS names {image}, which is not in BOOT_IMAGES"
                ));
            }
        }
        for w in &self.boot_writes {
            let footer = footers.iter().find(|(i, _)| *i == w.image).map(|(_, s)| *s);
            if footer != w.partition_size {
                return bad(format!(
                    "AVB_FOOTERS and manifest.json disagree on {}'s partition size",
                    w.partition.name()
                ));
            }
        }
        Ok(())
    }
}
