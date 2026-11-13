// SPDX-License-Identifier: MIT
//! The owner's SSH key in slot B's initramfs, as Omarchy Mobile's
//! `tools/install-phone` puts it there (its "The owner's key in slot B's
//! initramfs"): `/root/.ssh/authorized_keys` in a cpio archive after the
//! shipped one, inside the vendor ramdisk of the image that carries it
//! (`vendor_kernel_boot`, or `vendor_boot` on a phone without one). The
//! initramfs's rescue SSH then lets this computer in, before the phone's
//! first kernel update rebuilds the image with the same file
//! (`omamobile-boot-update`).
//!
//! What changes in the image, and nothing else:
//!
//! - the vendor ramdisk: the shipped bytes, unchanged, followed by a second
//!   LZ4 stream in the legacy format that holds the new archive. The
//!   kernel's LZ4 reader takes a second stream's magic number as the start
//!   of more of the same input, and the initramfs unpacker takes one
//!   archive after another, a later file over an earlier one. This differs
//!   from install-phone, which decompresses the shipped archive and
//!   compresses both again as one stream: here the release's bytes stay
//!   byte for byte where they were, and what was added is the tail;
//! - the ramdisk's size, in the v4 header and in its one ramdisk table
//!   entry, and the padding after the ramdisk to the page size.
//!
//! The image must be what `bin/omamobile images` builds and install-phone
//! reads: a v4 vendor boot image with one vendor ramdisk and nothing after
//! its last section (no AVB footer). Anything else is refused, and so is a
//! key that is not one OpenSSH public key line.
//!
//! The new archive is install-phone's: `root` (0755), `root/.ssh` (0700)
//! and `root/.ssh/authorized_keys` (0600, the key and a line feed), owned by
//! root, dated 1970-01-01. The LZ4 block carries the archive as literals
//! only, which every LZ4 reader takes: it is a few hundred bytes.
use crate::firstboot::is_ssh_public_key;
use crate::{Error, Result};

const HEADER_SIZE: usize = 2128;
const ENTRY_SIZE: usize = 108;
/// The legacy LZ4 frame's magic number, as the kernel's `unlz4` reads it.
const LZ4_LEGACY_MAGIC: u32 = 0x184C_2102;

fn bad(why: &str) -> Error {
    Error::Refused(format!("the image that carries slot B's initramfs: {why}"))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Where the sections of a v4 vendor boot image are.
struct Layout {
    page: usize,
    ramdisk_size: usize,
    /// Where the vendor ramdisk starts.
    ramdisk_at: usize,
    /// Where the section after it (the device tree) starts.
    after_ramdisk: usize,
    /// Where the ramdisk table starts.
    table_at: usize,
}

fn layout(img: &[u8]) -> Result<Layout> {
    if img.len() < HEADER_SIZE || &img[..8] != b"VNDRBOOT" {
        return Err(bad("it is not an Android vendor boot image"));
    }
    let version = u32_at(img, 8);
    let page = u32_at(img, 12) as usize;
    let ramdisk_size = u32_at(img, 24) as usize;
    let header_size = u32_at(img, 2096) as usize;
    let dtb_size = u32_at(img, 2100) as usize;
    let table_size = u32_at(img, 2112) as usize;
    let entries = u32_at(img, 2116);
    let entry_size = u32_at(img, 2120) as usize;
    let bootconfig_size = u32_at(img, 2124) as usize;
    if version != 4 || header_size != HEADER_SIZE {
        return Err(bad("it is not a v4 vendor boot image"));
    }
    if page < 2048 || !page.is_power_of_two() {
        return Err(bad("its page size is not one a bootloader takes"));
    }
    if (entries, entry_size, table_size) != (1, ENTRY_SIZE, ENTRY_SIZE) {
        return Err(bad("it does not hold exactly one vendor ramdisk"));
    }
    let pad = |n: usize| n.div_ceil(page) * page;
    let ramdisk_at = pad(HEADER_SIZE);
    let after_ramdisk = ramdisk_at + pad(ramdisk_size);
    let table_at = after_ramdisk + pad(dtb_size);
    if img.len() != table_at + pad(table_size) + pad(bootconfig_size) {
        return Err(bad(
            "its sections don't add up to its size (an AVB footer?)",
        ));
    }
    if (u32_at(img, table_at) as usize, u32_at(img, table_at + 4)) != (ramdisk_size, 0) {
        return Err(bad("its ramdisk table doesn't match its header"));
    }
    if ramdisk_size < 4 || u32_at(img, ramdisk_at) != LZ4_LEGACY_MAGIC {
        return Err(bad("its ramdisk is not LZ4 in the legacy format"));
    }
    Ok(Layout {
        page,
        ramdisk_size,
        ramdisk_at,
        after_ramdisk,
        table_at,
    })
}

/// One newc entry: header, name and its NUL padded to four bytes, data
/// padded to four bytes.
fn newc(out: &mut Vec<u8>, ino: u32, mode: u32, nlink: u32, name: &str, data: &[u8]) {
    let fields = [
        ino,
        mode,
        0, // uid
        0, // gid
        nlink,
        0, // mtime
        data.len() as u32,
        0, // devmajor
        0, // devminor
        0, // rdevmajor
        0, // rdevminor
        name.len() as u32 + 1,
        0, // check
    ];
    out.extend_from_slice(b"070701");
    for f in fields {
        out.extend_from_slice(format!("{f:08X}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// The archive install-phone packs: root's `authorized_keys` and the two
/// directories above it.
pub fn key_archive(key: &str) -> Vec<u8> {
    let mut out = Vec::new();
    newc(&mut out, 1, 0o040755, 2, "root", &[]);
    newc(&mut out, 2, 0o040700, 2, "root/.ssh", &[]);
    let line = format!("{key}\n");
    newc(
        &mut out,
        3,
        0o100600,
        1,
        "root/.ssh/authorized_keys",
        line.as_bytes(),
    );
    newc(&mut out, 0, 0, 1, "TRAILER!!!", &[]);
    out
}

/// `data` as a legacy LZ4 stream: the magic number, then one block of
/// literals only, prefixed with its size. `data` must be smaller than the
/// format's 8 MiB block.
fn lz4_legacy_literals(data: &[u8]) -> Vec<u8> {
    let mut block = Vec::with_capacity(data.len() + data.len() / 255 + 2);
    if data.len() < 15 {
        block.push((data.len() as u8) << 4);
    } else {
        block.push(0xF0);
        let mut rest = data.len() - 15;
        while rest >= 255 {
            block.push(255);
            rest -= 255;
        }
        block.push(rest as u8);
    }
    block.extend_from_slice(data);
    let mut out = Vec::with_capacity(block.len() + 8);
    out.extend_from_slice(&LZ4_LEGACY_MAGIC.to_le_bytes());
    out.extend_from_slice(&(block.len() as u32).to_le_bytes());
    out.extend_from_slice(&block);
    out
}

/// The image with the owner's key appended to its vendor ramdisk. `key` is
/// one OpenSSH public key line.
pub fn with_owner_key(img: &[u8], key: &str) -> Result<Vec<u8>> {
    if !is_ssh_public_key(key) {
        return Err(Error::Refused(
            "the SSH key for slot B's initramfs is not a single OpenSSH public key line".into(),
        ));
    }
    let l = layout(img)?;
    let tail = lz4_legacy_literals(&key_archive(key));
    let size = l.ramdisk_size + tail.len();
    let padded = size.div_ceil(l.page) * l.page;
    let mut out = Vec::with_capacity(img.len() + padded);
    out.extend_from_slice(&img[..l.ramdisk_at + l.ramdisk_size]);
    out.extend_from_slice(&tail);
    out.resize(l.ramdisk_at + padded, 0);
    let rest_at = out.len();
    out.extend_from_slice(&img[l.after_ramdisk..]);
    put_u32(&mut out, 24, size as u32);
    let table_at = rest_at + (l.table_at - l.after_ramdisk);
    put_u32(&mut out, table_at, size as u32);
    Ok(out)
}

/// The size [`with_owner_key`] gives, without making the image.
pub fn size_with_owner_key(img: &[u8], key: &str) -> Result<u64> {
    Ok(with_owner_key(img, key)?.len() as u64)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderPlaceholderPlaceholderPlacehol installer";

    /// A v4 vendor boot image as `bin/omamobile images` lays one out: one
    /// ramdisk (a legacy LZ4 stream of `ramdisk`), no device tree, the
    /// ramdisk table, `total` bytes in all.
    pub fn vendor_boot_v4(ramdisk_archive: &[u8], page: usize, total: usize) -> Vec<u8> {
        let ramdisk = lz4_legacy_literals(ramdisk_archive);
        let pad = |n: usize| n.div_ceil(page) * page;
        let mut img = vec![0u8; pad(HEADER_SIZE)];
        img[..8].copy_from_slice(b"VNDRBOOT");
        put_u32(&mut img, 8, 4);
        put_u32(&mut img, 12, page as u32);
        put_u32(&mut img, 24, ramdisk.len() as u32);
        put_u32(&mut img, 2096, HEADER_SIZE as u32);
        put_u32(&mut img, 2112, ENTRY_SIZE as u32);
        put_u32(&mut img, 2116, 1);
        put_u32(&mut img, 2120, ENTRY_SIZE as u32);
        img.extend_from_slice(&ramdisk);
        img.resize(pad(img.len()), 0);
        // The device tree section is what makes the image `total` bytes
        let table_at = total - page;
        let dtb = (table_at - img.len()) as u32;
        put_u32(&mut img, 2100, dtb);
        img.resize(table_at, 0);
        let mut table = vec![0u8; page];
        put_u32(&mut table, 0, ramdisk.len() as u32);
        put_u32(&mut table, 8, 1); // VENDOR_RAMDISK_TYPE_PLATFORM
        img.extend_from_slice(&table);
        img
    }

    /// Every legacy LZ4 stream of `data`, decoded, as the kernel reads one
    /// stream after another. Literals and matches both.
    pub fn unlz4(mut data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while data.len() >= 4 {
            let word = u32_at(data, 0);
            data = &data[4..];
            if word == LZ4_LEGACY_MAGIC {
                continue;
            }
            let (block, rest) = data.split_at(word as usize);
            data = rest;
            let mut i = 0;
            while i < block.len() {
                let token = block[i];
                i += 1;
                let mut lits = (token >> 4) as usize;
                if lits == 15 {
                    loop {
                        let b = block[i];
                        i += 1;
                        lits += b as usize;
                        if b != 255 {
                            break;
                        }
                    }
                }
                out.extend_from_slice(&block[i..i + lits]);
                i += lits;
                if i >= block.len() {
                    break;
                }
                let offset = u16::from_le_bytes([block[i], block[i + 1]]) as usize;
                i += 2;
                let mut len = (token & 15) as usize;
                if len == 15 {
                    loop {
                        let b = block[i];
                        i += 1;
                        len += b as usize;
                        if b != 255 {
                            break;
                        }
                    }
                }
                for _ in 0..len + 4 {
                    out.push(out[out.len() - offset]);
                }
            }
        }
        out
    }

    /// The entries of one or more newc archives, one after another: name,
    /// mode, data.
    pub fn entries(mut data: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
        let field = |h: &[u8], n: usize| {
            u32::from_str_radix(core::str::from_utf8(&h[6 + 8 * n..14 + 8 * n]).unwrap(), 16)
                .unwrap()
        };
        let mut out = Vec::new();
        while data.len() >= 110 {
            assert_eq!(&data[..6], b"070701");
            let (mode, size, namesize) = (field(data, 1), field(data, 6), field(data, 11));
            let name = core::str::from_utf8(&data[110..110 + namesize as usize - 1])
                .unwrap()
                .to_string();
            let at = (110 + namesize as usize).div_ceil(4) * 4;
            let body = data[at..at + size as usize].to_vec();
            data = &data[(at + size as usize).div_ceil(4) * 4..];
            if name != "TRAILER!!!" {
                out.push((name, mode, body));
            }
            // Padding between archives
            while data.first() == Some(&0) {
                data = &data[1..];
            }
        }
        out
    }

    fn shipped() -> Vec<u8> {
        let mut a = Vec::new();
        newc(&mut a, 1, 0o100755, 1, "init", b"#!/bin/sh\n");
        newc(&mut a, 0, 0, 1, "TRAILER!!!", &[]);
        a
    }

    #[test]
    fn the_key_is_appended_as_install_phone_appends_it_and_the_release_bytes_stay() {
        let img = vendor_boot_v4(&shipped(), 4096, 3 * 4096);
        let l = layout(&img).unwrap();
        let keyed = with_owner_key(&img, KEY).unwrap();
        // Same size here: the archive fits the ramdisk's last page
        assert_eq!(keyed.len(), img.len());
        let k = layout(&keyed).expect("the result is a v4 image with one ramdisk");
        // The shipped ramdisk is where it was, byte for byte, and the rest of
        // the image after the ramdisk is unchanged
        assert_eq!(
            &keyed[k.ramdisk_at..k.ramdisk_at + l.ramdisk_size],
            &img[l.ramdisk_at..l.ramdisk_at + l.ramdisk_size]
        );
        assert_eq!(
            &keyed[k.after_ramdisk..k.table_at],
            &img[l.after_ramdisk..l.table_at]
        );
        // The header changed in its ramdisk size and nowhere else
        let mut header = img[..HEADER_SIZE].to_vec();
        put_u32(&mut header, 24, k.ramdisk_size as u32);
        assert_eq!(&keyed[..HEADER_SIZE], &header[..]);
        // What the kernel unpacks: the shipped archive, then the key's
        let ramdisk = &keyed[k.ramdisk_at..k.ramdisk_at + k.ramdisk_size];
        let files = entries(&unlz4(ramdisk));
        let names: Vec<(&str, u32)> = files.iter().map(|(n, m, _)| (n.as_str(), *m)).collect();
        assert_eq!(
            names,
            [
                ("init", 0o100755),
                ("root", 0o040755),
                ("root/.ssh", 0o040700),
                ("root/.ssh/authorized_keys", 0o100600)
            ]
        );
        assert_eq!(files[3].2, format!("{KEY}\n").into_bytes());
        assert_eq!(size_with_owner_key(&img, KEY).unwrap(), keyed.len() as u64);
    }

    #[test]
    fn an_archive_that_needs_another_page_grows_the_image_by_it() {
        // A ramdisk that ends near the end of its page
        let mut big = Vec::new();
        newc(&mut big, 1, 0o100644, 1, "filler", &vec![7u8; 3600]);
        newc(&mut big, 0, 0, 1, "TRAILER!!!", &[]);
        let img = vendor_boot_v4(&big, 4096, 4 * 4096);
        let keyed = with_owner_key(&img, KEY).unwrap();
        assert_eq!(keyed.len(), img.len() + 4096);
        let k = layout(&keyed).unwrap();
        let ramdisk = &keyed[k.ramdisk_at..k.ramdisk_at + k.ramdisk_size];
        assert_eq!(entries(&unlz4(ramdisk)).len(), 4);
    }

    #[test]
    fn only_a_v4_image_with_one_ramdisk_and_one_key_line_is_changed() {
        let img = vendor_boot_v4(&shipped(), 4096, 3 * 4096);
        for key in [
            "",
            "not a key",
            "ssh-ed25519 AAAA\nssh-ed25519 BBBB",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
        ] {
            assert!(with_owner_key(&img, key).is_err(), "{key:?}");
        }
        let mut footer = img.clone();
        footer.extend_from_slice(&[0u8; 4096]);
        let mut v3 = img.clone();
        put_u32(&mut v3, 8, 3);
        let mut two = img.clone();
        put_u32(&mut two, 2116, 2);
        let mut gzip = img.clone();
        gzip[4096] = 0x1f;
        for (why, bad) in [
            ("noise", vec![9u8; 3 * 4096]),
            ("an AVB footer", footer),
            ("v3", v3),
            ("two ramdisks", two),
            ("not LZ4", gzip),
        ] {
            assert!(with_owner_key(&bad, KEY).is_err(), "{why}");
        }
    }
}
