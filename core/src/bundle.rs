// SPDX-License-Identifier: MIT
//! A release bundle's chain of trust (docs/RELEASES.md §6), in the order
//! `tools/install-phone` walks it:
//!
//! 1. `SHA256SUMS.sig` is the release key's signature on `SHA256SUMS`;
//! 2. `SHA256SUMS` covers `manifest.json`, `MANIFEST` and `device.conf`;
//! 3. `manifest.json` passes its rules and agrees with `device.conf`;
//! 4. `manifest.json` lists exactly the files `SHA256SUMS` covers, with the
//!    same digests;
//! 5. the signed `MANIFEST` names the same root image and kernel.
//!
//! The result, [`VerifiedRelease`], is the only thing that can turn bytes
//! into a [`VerifiedImage`] or a [`VerifiedRootImage`], and those are the
//! only things [`crate::fastboot::Session`] accepts to write. There is no
//! unsigned path in this crate.
use crate::build_index::{self, BuildIndex, IndexData};
use crate::device_conf::DeviceConf;
use crate::hash::{is_sha256_hex, sha256_hex, StreamHasher};
use crate::manifest::{is_calendar_version, BootWrite, Manifest};
use crate::signature::{verify_detached, Signer, TrustedKeys};
use crate::{Error, Result};

/// The small files of a bundle, as bytes.
pub struct ReleaseFiles<'a> {
    pub sha256sums: &'a [u8],
    pub sha256sums_sig: &'a [u8],
    /// The certificate(s) to check the signature with (`omamobile.gpg`, or a
    /// copy built into the installer). Trust comes from [`TrustedKeys`].
    pub keyring: &'a [u8],
    pub manifest_json: &'a [u8],
    /// The text `MANIFEST`.
    pub manifest_txt: &'a [u8],
    pub device_conf: &'a [u8],
}

/// What the release's signed descriptor says about installing on this
/// device, beyond the manifest's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRules {
    /// `BOOTLOADER_SLOT_PROBE=yes`: the installer may read the other slot's
    /// bootloader version by switching slots. Parsed and carried; nothing in
    /// this version acts on it.
    pub bootloader_slot_probe: bool,
    /// `INSTALL_KEYS_FASTBOOT`: how this phone's keys reach Fastboot Mode, as
    /// a phrase that ends a sentence.
    pub keys_fastboot: Option<String>,
    /// `BOOT_UPDATE_AUTO_WRITE` (default yes): the `AUTO_WRITE` the
    /// installer puts in the root image's `boot-update.conf` placeholder.
    pub boot_update_auto_write: bool,
}

#[derive(Debug, Clone)]
pub struct VerifiedRelease {
    manifest: Manifest,
    rules: DeviceRules,
    signer: Signer,
    /// SHA-256 of `manifest.json`: ties tokens to this release.
    id: String,
}

/// A boot image whose bytes match the signed manifest.
#[derive(Debug)]
pub struct VerifiedImage<'a> {
    pub(crate) release: String,
    pub(crate) index: usize,
    pub(crate) bytes: &'a [u8],
}

/// Proof that the root image's bytes were hashed and match the signed
/// manifest. The caller streams those same bytes afterwards (from the
/// cached download it just hashed). The proof can't bind that later
/// stream, so the session hashes the stream again as it is sent
/// (`Session::userdata_streamed`) and refuses to finish `userdata` with
/// any other bytes.
#[derive(Debug, Clone)]
pub struct VerifiedRootImage {
    pub(crate) release: String,
}

/// Hashes the compressed root image as it arrives.
pub struct RootImageCheck {
    release: String,
    name: String,
    sha256: String,
    size: Option<u64>,
    hasher: StreamHasher,
}

fn parse_sums(text: &[u8]) -> Result<Vec<(String, String)>> {
    let text = core::str::from_utf8(text)
        .map_err(|_| Error::Checksum("SHA256SUMS is not UTF-8".into()))?;
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let bad = || Error::Checksum(format!("SHA256SUMS: not a checksum line: {line}"));
        let (digest, rest) = line.split_at_checked(64).ok_or_else(bad)?;
        let path = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))
            .ok_or_else(bad)?;
        let path = path.strip_prefix("./").unwrap_or(path);
        if !is_sha256_hex(digest) || path.is_empty() {
            return Err(bad());
        }
        if out.iter().any(|(p, _)| p == path) {
            return Err(Error::Checksum(format!("SHA256SUMS covers {path} twice")));
        }
        out.push((path.to_string(), digest.to_string()));
    }
    Ok(out)
}

fn manifest_txt_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(": "))
}

pub fn verify_release(files: &ReleaseFiles<'_>, trusted: &TrustedKeys) -> Result<VerifiedRelease> {
    // 1. The signature, before anything in the bundle is believed
    let signer = verify_detached(
        files.keyring,
        files.sha256sums_sig,
        files.sha256sums,
        trusted,
    )?;

    // 2. The signed checksums cover the three files the rules come from
    let sums = parse_sums(files.sha256sums)?;
    let covered = |path: &str, bytes: &[u8]| -> Result<()> {
        let want = sums
            .iter()
            .find(|(p, _)| p == path)
            .ok_or_else(|| Error::Checksum(format!("the signed checksums don't cover {path}")))?;
        if sha256_hex(bytes) != want.1 {
            return Err(Error::Checksum(format!("{path} doesn't match SHA256SUMS")));
        }
        Ok(())
    };
    covered("manifest.json", files.manifest_json)?;
    covered("MANIFEST", files.manifest_txt)?;
    covered("device.conf", files.device_conf)?;

    // 3. The manifest's own rules, and the descriptor
    let manifest = Manifest::parse(files.manifest_json)?;
    let conf = DeviceConf::parse(files.device_conf)?;
    manifest.check_descriptor(&conf)?;
    let rules = DeviceRules {
        bootloader_slot_probe: conf.bootloader_slot_probe()?,
        keys_fastboot: conf
            .get("INSTALL_KEYS_FASTBOOT")
            .filter(|k| !k.is_empty())
            .map(str::to_string),
        boot_update_auto_write: conf.boot_update_auto_write()?,
    };
    if !is_calendar_version(&manifest.version) {
        return Err(Error::Manifest(format!(
            "a signed release is versioned by the calendar (YYYY.MM.DD), not {}",
            manifest.version
        )));
    }

    // 4. manifest.json lists exactly the files SHA256SUMS covers
    for f in &manifest.files {
        match sums.iter().find(|(p, _)| *p == f.path) {
            Some((_, d)) if *d == f.sha256 => {}
            Some(_) => {
                return Err(Error::Checksum(format!(
                    "manifest.json and SHA256SUMS disagree on {}",
                    f.path
                )))
            }
            None => {
                return Err(Error::Checksum(format!(
                    "manifest.json lists {}, which SHA256SUMS doesn't cover",
                    f.path
                )))
            }
        }
    }
    for (p, _) in &sums {
        if p != "manifest.json" && !manifest.files.iter().any(|f| f.path == *p) {
            return Err(Error::Checksum(format!(
                "SHA256SUMS covers {p}, which manifest.json doesn't list"
            )));
        }
    }

    // 5. The signed MANIFEST names the same root image and kernel
    let txt = core::str::from_utf8(files.manifest_txt)
        .map_err(|_| Error::Manifest("MANIFEST is not UTF-8".into()))?;
    let want_root = format!(
        "{} ({})",
        manifest.root_image.name, manifest.root_image.sha256
    );
    if manifest_txt_value(txt, "root image") != Some(want_root.as_str()) {
        return Err(Error::Manifest(
            "manifest.json and MANIFEST name different root images".into(),
        ));
    }
    if manifest_txt_value(txt, "kernel release") != Some(manifest.kernel_release.as_str()) {
        return Err(Error::Manifest(
            "manifest.json and MANIFEST name different kernels".into(),
        ));
    }
    if manifest_txt_value(txt, "device").is_some_and(|d| d != manifest.codename) {
        return Err(Error::Manifest("MANIFEST is for another device".into()));
    }

    Ok(VerifiedRelease {
        manifest,
        rules,
        signer,
        id: sha256_hex(files.manifest_json),
    })
}

impl VerifiedRelease {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn signer(&self) -> &Signer {
        &self.signer
    }
    pub fn rules(&self) -> &DeviceRules {
        &self.rules
    }

    /// Whether this release carries a build index at all.
    pub fn has_build_index(&self) -> bool {
        self.manifest
            .files
            .iter()
            .any(|f| f.path == build_index::FILE)
    }

    /// The release's build index: `bytes` must be the `factory-builds.json`
    /// the signed checksums cover, and pass every rule of
    /// [`build_index`]. This is the only way to a [`BuildIndex`], so a
    /// factory image is never checked against a digest the release key
    /// didn't sign.
    pub fn build_index(&self, bytes: &[u8]) -> Result<BuildIndex> {
        self.check_file(build_index::FILE, bytes)?;
        let data = IndexData::parse(bytes, &self.manifest.codename)?;
        Ok(BuildIndex::new(data, self.id.clone()))
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Any bundle file against the signed manifest (size, then SHA-256).
    pub fn check_file(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let f = self
            .manifest
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| Error::Checksum(format!("{path} is not a file of this release")))?;
        if bytes.len() as u64 != f.size {
            return Err(Error::Checksum(format!(
                "{path} is {} bytes, not {}",
                bytes.len(),
                f.size
            )));
        }
        if sha256_hex(bytes) != f.sha256 {
            return Err(Error::Checksum(format!(
                "{path} doesn't match the signed manifest"
            )));
        }
        Ok(())
    }

    /// The bytes of one of the plan's boot images, checked.
    pub fn verify_boot_image<'a>(
        &self,
        write: &BootWrite,
        bytes: &'a [u8],
    ) -> Result<VerifiedImage<'a>> {
        let index = self
            .manifest
            .boot_writes
            .iter()
            .position(|w| w == write)
            .ok_or_else(|| {
                Error::Refused(format!(
                    "{} is not a write of this release",
                    write.partition.name()
                ))
            })?;
        self.check_file(&write.source, bytes)?;
        Ok(VerifiedImage {
            release: self.id.clone(),
            index,
            bytes,
        })
    }

    /// Start hashing the compressed root image.
    pub fn root_image_check(&self) -> RootImageCheck {
        let r = &self.manifest.root_image;
        RootImageCheck {
            release: self.id.clone(),
            name: r.name.clone(),
            sha256: r.sha256.clone(),
            size: r.size,
            hasher: StreamHasher::new(),
        }
    }
}

impl RootImageCheck {
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }
    pub fn finish(self) -> Result<VerifiedRootImage> {
        let (digest, len) = self.hasher.finish();
        if self.size.is_some_and(|s| s != len) {
            return Err(Error::Checksum(format!(
                "{} is {len} bytes, not {}",
                self.name,
                self.size.unwrap_or(0)
            )));
        }
        if digest != self.sha256 {
            return Err(Error::Checksum(format!(
                "{} doesn't match the signed manifest",
                self.name
            )));
        }
        Ok(VerifiedRootImage {
            release: self.release,
        })
    }
}
