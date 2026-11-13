// SPDX-License-Identifier: MIT
//! Builds small signed release bundles for the tests: the published layout
//! (docs/RELEASES.md §6) with tiny images, signed by a key made on the spot.
//! A test changes the manifest or the descriptor before signing, so every
//! refusal is tested on a bundle whose signature and checksums are valid.
#![allow(dead_code)]
use omamobile_installer_core::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use omamobile_installer_core::hash::sha256_hex;
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_core::Result;
use pgp::composed::{DetachedSignature, KeyType, SecretKeyParamsBuilder};
use pgp::crypto::hash::HashAlgorithm;
use pgp::ser::Serialize;
use pgp::types::{KeyDetails, Password};
use serde_json::{json, Value};

pub const IMAGES: [(&str, usize, Option<usize>); 5] = [
    ("boot", 16 << 10, Some(16 << 10)),
    ("init_boot", 8 << 10, Some(8 << 10)),
    ("vendor_boot", 4 << 10, None),
    ("vendor_kernel_boot", 12 << 10, None),
    ("dtbo", 4 << 10, None),
];
pub const ROOT_NAME: &str = "omamobile-rootfs-cheetah-2026.01.02-aarch64.img.zst";
/// The build the fake phone runs: its bootloader and baseband are the fake's.
pub const PHONE_BUILD: &str = "TEST.260101.001";

pub struct TestBundle {
    pub sums: Vec<u8>,
    pub sig: Vec<u8>,
    pub keyring: Vec<u8>,
    pub manifest_json: Vec<u8>,
    pub manifest_txt: Vec<u8>,
    pub device_conf: Vec<u8>,
    pub images: Vec<(String, Vec<u8>)>,
    pub root_raw: Vec<u8>,
    pub root_zst: Vec<u8>,
    pub fingerprint: String,
    /// `factory-builds.json`, or empty when the bundle carries none.
    pub index_json: Vec<u8>,
    /// A stand-in for Google's factory zip for [`PHONE_BUILD`]: noise, with
    /// the size and SHA-256 the index records. Nothing ever opens it.
    pub factory_zip: Vec<u8>,
    pub factory_zip_name: String,
    /// Files beyond the published layout's, under the signed checksums
    /// like every other: the tools a bundle ships (`tools/...`).
    pub extra: Vec<(String, Vec<u8>)>,
}

/// The image that carries slot B's initramfs, as `bin/omamobile images`
/// lays it out and the owner's SSH key is added to it
/// (`core/src/ramdisk_key.rs`): a v4 vendor boot image of 4 KiB pages with
/// one vendor ramdisk, a legacy LZ4 stream of a small cpio archive (an
/// `init`), a device tree section of noise that makes it `size` bytes, and
/// the ramdisk table.
pub fn vendor_kernel_boot(size: usize) -> Vec<u8> {
    const PAGE: usize = 4096;
    let put = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let mut archive = Vec::new();
    for (ino, mode, name, data) in [
        (
            1u32,
            0o100755u32,
            "init",
            &b"#!/bin/sh\nexec /sbin/init\n"[..],
        ),
        (0, 0, "TRAILER!!!", &b""[..]),
    ] {
        let fields = [
            ino,
            mode,
            0,
            0,
            1,
            0,
            data.len() as u32,
            0,
            0,
            0,
            0,
            name.len() as u32 + 1,
            0,
        ];
        archive.extend_from_slice(b"070701");
        for f in fields {
            archive.extend_from_slice(format!("{f:08X}").as_bytes());
        }
        archive.extend_from_slice(name.as_bytes());
        archive.push(0);
        archive.resize(archive.len().div_ceil(4) * 4, 0);
        archive.extend_from_slice(data);
        archive.resize(archive.len().div_ceil(4) * 4, 0);
    }
    // Literals only, in one block (the format's own rule for a short run)
    let mut block = vec![0xF0, (archive.len() - 15) as u8];
    assert!(archive.len() - 15 < 255);
    block.extend_from_slice(&archive);
    let mut ramdisk = 0x184C_2102u32.to_le_bytes().to_vec();
    ramdisk.extend_from_slice(&(block.len() as u32).to_le_bytes());
    ramdisk.extend_from_slice(&block);
    let mut img = vec![0u8; PAGE];
    img[..8].copy_from_slice(b"VNDRBOOT");
    put(&mut img, 8, 4);
    put(&mut img, 12, PAGE as u32);
    put(&mut img, 24, ramdisk.len() as u32);
    put(&mut img, 2096, 2128);
    put(&mut img, 2112, 108);
    put(&mut img, 2116, 1);
    put(&mut img, 2120, 108);
    img.extend_from_slice(&ramdisk);
    img.resize(2 * PAGE, 0);
    let table_at = size - PAGE;
    put(&mut img, 2100, (table_at - 2 * PAGE) as u32);
    img.extend_from_slice(&noise(7, table_at - 2 * PAGE));
    let mut table = vec![0u8; PAGE];
    put(&mut table, 0, ramdisk.len() as u32);
    put(&mut table, 8, 1);
    img.extend_from_slice(&table);
    img
}

/// Deterministic noise.
pub fn noise(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).max(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

/// Where the stand-in root image keeps its placeholders for the owner's
/// answers (rootfs/placeholders): one block each, inside its run of zeros.
pub const PLACEHOLDERS: [(&str, usize); 2] = [
    ("/etc/omamobile/boot-update.conf", 300),
    ("/etc/omamobile/firstboot.conf", 302),
];

/// A placeholder's bytes as the release builds them: comments, padded to
/// one block.
pub fn placeholder(path: &str) -> Vec<u8> {
    omamobile_installer_core::firstboot::pad_placeholder(
        format!("# Omarchy Mobile: the place for {path}, kept by the release image.\n").as_bytes(),
        4096,
    )
    .unwrap()
}

/// A 2 MiB "root image": zero runs, a repeated word, noise, blocks that
/// only look uniform, and the two placeholders.
pub fn root_image() -> Vec<u8> {
    let b = 4096;
    let mut img = vec![0u8; 512 * b];
    img[64 * b..96 * b].copy_from_slice(&noise(1, 32 * b));
    for w in img[96 * b..100 * b].as_chunks_mut::<4>().0 {
        w.copy_from_slice(&0xdead_beef_u32.to_le_bytes());
    }
    img[100 * b..300 * b].copy_from_slice(&noise(2, 200 * b));
    // 300..400 stay zero; then blocks that are zero but for their last byte
    for i in 400..420 {
        img[(i + 1) * b - 1] = 1;
    }
    img[420 * b..511 * b].copy_from_slice(&noise(3, 91 * b));
    img[511 * b..].fill(0x11);
    for (path, block) in PLACEHOLDERS {
        img[block * b..(block + 1) * b].copy_from_slice(&placeholder(path));
    }
    img
}

/// `root_image.placeholders`, as tools/bundle-manifest writes it.
pub fn placeholders_json() -> Value {
    json!(PLACEHOLDERS
        .iter()
        .map(|(path, block)| json!({"path": path, "offset": block * 4096, "length": 4096, "sha256": sha256_hex(&placeholder(path))}))
        .collect::<Vec<_>>())
}

/// A valid zstd frame of raw and RLE blocks, with the content size in its
/// header (RFC 8878 §3.1.1): enough to drive the decoder's framing without
/// a compressor. Compressed blocks are covered by the zstd-made fixture.
pub fn zstd_store(data: &[u8], with_size: bool) -> Vec<u8> {
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd];
    if with_size {
        out.push(0xc0); // 8-byte content size, a window descriptor, no checksum
        out.push(0x50); // window 1 MiB
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    } else {
        out.extend_from_slice(&[0x00, 0x50]);
    }
    let pieces: Vec<&[u8]> = data.chunks(100_000).collect();
    for (i, piece) in pieces.iter().enumerate() {
        let last = u32::from(i + 1 == pieces.len());
        let rle = piece.iter().all(|b| *b == piece[0]);
        let header = ((piece.len() as u32) << 3) | (u32::from(rle) << 1) | last;
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        if rle {
            out.push(piece[0]);
        } else {
            out.extend_from_slice(piece);
        }
    }
    out
}

pub fn default_conf() -> String {
    [
        "# test descriptor",
        "CODENAME=cheetah",
        "NAME=\"Pixel 7 Pro\"",
        "INSTALL_KEYS_FASTBOOT=\"with the phone off, hold Volume Down, then also hold Power, until Fastboot Mode appears\"",
        "SOC_IDS=\"b0:0x9855001 a0:0x9855000\"",
        "AVB_FOOTERS=\"boot:0x4000 init_boot:0x2000\"",
        "BOOT_IMAGES=\"boot init_boot vendor_boot vendor_kernel_boot dtbo\"",
        "UFS_RW=\"boot_b init_boot_b vendor_boot_b vendor_kernel_boot_b dtbo_b userdata\"",
        "",
    ]
    .join("\n")
}

/// The stand-in factory zip's bytes.
pub fn factory_zip() -> Vec<u8> {
    noise(77, 48 << 10)
}

fn index_entry(
    build: &str,
    versions: &str,
    zip: Option<&[u8]>,
    tested_with: Option<&str>,
) -> Value {
    // A build that isn't the phone's has no file here: a digest and a size
    // that only look the part
    let (sha256, size) = match zip {
        Some(data) => (sha256_hex(data), data.len() as u64),
        None => (
            sha256_hex(build.as_bytes()),
            3_900_000_000 + build.len() as u64,
        ),
    };
    let name = format!(
        "cheetah-{}-factory-{}.zip",
        build.to_ascii_lowercase(),
        &sha256[..8]
    );
    let mut entry = json!({
        "build": build,
        "bootloader": format!("fake-{versions}"),
        "baseband": format!("fake-radio-{versions}"),
        "zip": name,
        "url": format!("https://dl.google.com/dl/android/aosp/{name}"),
        "page": "https://developers.google.com/android/images#cheetah",
        "sha256": sha256,
        "size": size,
        "tested": tested_with.is_some(),
    });
    if let Some(release) = tested_with {
        entry["tested_with"] = json!(release);
    }
    entry
}

/// A build index around the fake phone (bootloader `fake-1.0`, baseband
/// `fake-radio-1.0`), newest first: an untested build newer than every
/// tested one; a tested one; the phone's, tested, and a carrier variant
/// with the same two versions; and an old untested one.
pub fn default_index() -> Value {
    let zip = factory_zip();
    json!({
        "format": 1,
        "codename": "cheetah",
        "updated": "2026-01-02",
        "builds": [
            index_entry("TEST.260301.001", "1.2", None, None),
            index_entry("TEST.260201.001", "1.1", None, Some("2026.01.02")),
            index_entry(PHONE_BUILD, "1.0", Some(&zip), Some("2026.01.02")),
            index_entry("TEST.260101.001.A1", "1.0", None, None),
            index_entry("TEST.251201.001", "0.9", None, None),
        ],
    })
}

impl TestBundle {
    pub fn new() -> Self {
        Self::build(|_| {}, |c| c)
    }

    /// `edit` changes manifest.json and `conf` the descriptor, both before
    /// the checksums are taken and signed.
    pub fn build(edit: impl FnOnce(&mut Value), conf: impl FnOnce(String) -> String) -> Self {
        Self::build_all(edit, conf, |_| {})
    }

    /// A bundle whose build index was changed before it was signed. Setting
    /// it to `null` leaves the index out of the bundle.
    pub fn with_index(index: impl FnOnce(&mut Value)) -> Self {
        Self::build_all(|_| {}, |c| c, index)
    }

    pub fn build_all(
        edit: impl FnOnce(&mut Value),
        conf: impl FnOnce(String) -> String,
        index: impl FnOnce(&mut Value),
    ) -> Self {
        Self::build_for("cheetah", edit, conf, index)
    }

    /// A bundle for another phone model: its manifest, descriptor and
    /// `MANIFEST` name `codename`. The build index stays cheetah's.
    pub fn for_device(codename: &str) -> Self {
        Self::build_for(codename, |_| {}, |c| c, |_| {})
    }

    /// A bundle around another root image: `raw`, the unpacked image (a
    /// multiple of 4096 bytes), and its `root_image.placeholders`.
    pub fn with_root_image(raw: Vec<u8>, placeholders: Value) -> Self {
        Self::build_with(
            "cheetah",
            Some((raw, placeholders)),
            |_| {},
            |c| c,
            |_| {},
            Vec::new(),
        )
    }

    /// A bundle that also ships these files (paths inside the bundle, such
    /// as `tools/nv-backup`), signed with the rest.
    pub fn with_files(extra: Vec<(String, Vec<u8>)>) -> Self {
        Self::build_with("cheetah", None, |_| {}, |c| c, |_| {}, extra)
    }

    fn build_for(
        codename: &str,
        edit: impl FnOnce(&mut Value),
        conf: impl FnOnce(String) -> String,
        index: impl FnOnce(&mut Value),
    ) -> Self {
        Self::build_with(codename, None, edit, conf, index, Vec::new())
    }

    fn build_with(
        codename: &str,
        root: Option<(Vec<u8>, Value)>,
        edit: impl FnOnce(&mut Value),
        conf: impl FnOnce(String) -> String,
        index: impl FnOnce(&mut Value),
        extra: Vec<(String, Vec<u8>)>,
    ) -> Self {
        let images: Vec<(String, Vec<u8>)> = IMAGES
            .iter()
            .enumerate()
            .map(|(i, (name, size, _))| {
                let image = if *name == "vendor_kernel_boot" {
                    vendor_kernel_boot(*size)
                } else {
                    noise(100 + i as u32, *size)
                };
                (name.to_string(), image)
            })
            .collect();
        let (root_raw, placeholders) = root.unwrap_or_else(|| (root_image(), placeholders_json()));
        let root_zst = zstd_store(&root_raw, true);
        let device_conf =
            conf(default_conf().replace("CODENAME=cheetah", &format!("CODENAME={codename}")))
                .into_bytes();
        let manifest_txt = format!("version: 2026.01.02\ndevice: {codename}\nkernel release: 7.3.0-test\nroot image: {ROOT_NAME} ({})\n", sha256_hex(&root_zst)).into_bytes();

        // The key: Ed25519 in the v4 format, as the release key is
        let mut rng = rand::thread_rng();
        let params = SecretKeyParamsBuilder::default()
            .key_type(KeyType::Ed25519Legacy)
            .can_sign(true)
            .can_certify(true)
            .primary_user_id("Installer spike test key".into())
            .build()
            .expect("key params");
        let secret = params.generate(&mut rng).expect("key generation");
        let public = secret.to_public_key();
        let keyring = public.to_bytes().expect("serialize the key");
        let fingerprint = format!("{:X}", public.primary_key.fingerprint());

        let mut files: Vec<(String, Vec<u8>)> = vec![
            ("MANIFEST".into(), manifest_txt.clone()),
            ("device.conf".into(), device_conf.clone()),
        ];
        files.extend(
            images
                .iter()
                .map(|(n, d)| (format!("images/{n}.img"), d.clone())),
        );
        files.push(("omamobile.gpg".into(), keyring.clone()));
        files.extend(extra.iter().cloned());
        let factory_zip = factory_zip();
        let mut index_value = default_index();
        index(&mut index_value);
        let index_json = if index_value.is_null() {
            Vec::new()
        } else {
            let mut text = serde_json::to_vec_pretty(&index_value).unwrap();
            text.push(b'\n');
            files.push(("factory-builds.json".into(), text.clone()));
            text
        };
        let factory_zip_name = default_index()["builds"][2]["zip"]
            .as_str()
            .unwrap()
            .to_string();

        let mut writes: Vec<Value> = IMAGES.iter().map(|(name, size, psize)| json!({"partition": format!("{name}_b"), "partition_size": psize, "size": size, "source": format!("images/{name}.img")})).collect();
        writes.push(
            json!({"erases_userdata": true, "partition": "userdata", "source": "root_image"}),
        );
        let mut manifest = json!({
            "format": 1, "version": "2026.01.02", "label": "test", "name": "Omarchy Mobile",
            "device": {"codename": codename, "name": "Pixel 7 Pro"},
            "kernel": {"commit": "000000000000", "release": "7.3.0-test"},
            "files": files.iter().map(|(p, d)| json!({"path": p, "sha256": sha256_hex(d), "size": d.len()})).collect::<Vec<_>>(),
            "root_image": {"compression": "zstd", "name": ROOT_NAME, "sha256": sha256_hex(&root_zst), "size": root_zst.len(), "placeholders": placeholders},
            "install": {
                "tool": "fastboot", "product": codename,
                "require_unlocked_bootloader": true, "require_matching_bootloader_slots": true,
                "storage_guard_allowlist": ["boot_b", "init_boot_b", "vendor_boot_b", "vendor_kernel_boot_b", "dtbo_b", "userdata"],
                "never": ["slot A (any *_a partition)"],
                "writes": writes,
                "after": ["set-active b", "reboot"],
            },
        });
        edit(&mut manifest);
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        files.push(("manifest.json".into(), manifest_json.clone()));
        files.sort();
        let sums: Vec<u8> = files
            .iter()
            .map(|(p, d)| format!("{}  {p}\n", sha256_hex(d)))
            .collect::<String>()
            .into_bytes();
        let sig = DetachedSignature::sign_binary_data(
            &mut rng,
            &secret.primary_key,
            &Password::empty(),
            HashAlgorithm::Sha512,
            &sums[..],
        )
        .expect("sign")
        .to_bytes()
        .expect("serialize the signature");

        TestBundle {
            sums,
            sig,
            keyring,
            manifest_json,
            manifest_txt,
            device_conf,
            images,
            root_raw,
            root_zst,
            fingerprint,
            index_json,
            factory_zip,
            factory_zip_name,
            extra,
        }
    }

    pub fn files(&self) -> ReleaseFiles<'_> {
        ReleaseFiles {
            sha256sums: &self.sums,
            sha256sums_sig: &self.sig,
            keyring: &self.keyring,
            manifest_json: &self.manifest_json,
            manifest_txt: &self.manifest_txt,
            device_conf: &self.device_conf,
        }
    }

    pub fn trusted(&self) -> TrustedKeys {
        TrustedKeys::new(&[&self.fingerprint]).unwrap()
    }

    pub fn verify(&self) -> Result<VerifiedRelease> {
        verify_release(&self.files(), &self.trusted())
    }

    /// The index as a front end hands it to the flow.
    pub fn index(&self) -> Option<&[u8]> {
        (!self.index_json.is_empty()).then_some(&self.index_json[..])
    }

    pub fn image(&self, name: &str) -> &[u8] {
        &self.images.iter().find(|(n, _)| n == name).unwrap().1
    }

    /// The root image as an install with the tests' answers
    /// ([`super::drive::answers`], [`super::drive::SALT`]) writes it: the
    /// placeholders' blocks hold the answers.
    pub fn root_as_installed(&self) -> Vec<u8> {
        self.root_as_installed_with(None)
    }

    /// The same, from a run that hands the flow this computer's SSH key:
    /// `firstboot.conf` carries it too.
    pub fn root_as_installed_with(&self, ssh_key: Option<&str>) -> Vec<u8> {
        let release = self.verify().unwrap();
        let answers = omamobile_installer_core::firstboot::root_answers(
            &release,
            &super::drive::answers(),
            ssh_key,
            super::drive::SALT,
        )
        .unwrap();
        let mut raw = self.root_raw.clone();
        for (_, offset, bytes) in answers.blocks() {
            let at = offset as usize;
            raw[at..at + bytes.len()].copy_from_slice(bytes);
        }
        raw
    }
}
