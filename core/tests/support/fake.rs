// SPDX-License-Identifier: MIT
//! A fake phone in fastboot mode: a transport that speaks the protocol
//! (AOSP system/core/fastboot/README.md) against partitions held in memory,
//! with switches for the ways a real one can refuse. Test code only.
//!
//! It applies what is flashed, sparse images included (AOSP libsparse's
//! format, parsed here independently of the core's writer), so a test can
//! compare the partitions afterwards with what should be on them.
#![allow(dead_code)]
use omamobile_installer_core::fastboot::Transport;
use omamobile_installer_core::{Error, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

pub const SENTINEL: u8 = 0xa5;
/// A placeholder, never a real serial number.
pub const SERIAL: &str = "FAKESERIAL0001";

pub struct FakeState {
    pub vars: BTreeMap<String, String>,
    pub partitions: BTreeMap<String, Vec<u8>>,
    /// Every command received, in order.
    pub log: Vec<String>,
    pub active_slot: String,
    pub rebooted: bool,
    pub max_download: usize,
    // Faults
    /// Answer FAIL to a command starting with any of these.
    pub fail_on: Vec<String>,
    /// Answer `download:` with a DATA size smaller than asked.
    pub short_data: bool,
    /// Take at most this many bytes of each data write.
    pub take_at_most: Option<usize>,
    /// After this many commands, report another serial number.
    pub swap_serial_after: Option<usize>,
    // Protocol state
    staged: Option<Vec<u8>>,
    expecting: usize,
    incoming: Vec<u8>,
    replies: VecDeque<Vec<u8>>,
}

#[derive(Clone)]
pub struct FakePhone(pub Rc<RefCell<FakeState>>);

const SLOTTED: [(&str, usize); 8] = [
    ("boot", 16 << 10),
    ("init_boot", 8 << 10),
    ("vendor_boot", 16 << 10),
    ("vendor_kernel_boot", 32 << 10),
    ("dtbo", 8 << 10),
    ("vbmeta", 4 << 10),
    ("bootloader", 8 << 10),
    ("modem", 8 << 10),
];
const PLAIN: [(&str, usize); 6] = [
    ("userdata", 8 << 20),
    ("persist", 8 << 10),
    ("efs", 8 << 10),
    ("efs_backup", 8 << 10),
    ("devinfo", 4 << 10),
    ("misc", 4 << 10),
];

impl FakePhone {
    /// An unlocked phone of this product on slot a.
    pub fn new(product: &str) -> Self {
        let mut partitions = BTreeMap::new();
        for (name, size) in SLOTTED {
            partitions.insert(format!("{name}_a"), vec![SENTINEL; size]);
            partitions.insert(format!("{name}_b"), vec![SENTINEL; size]);
        }
        for (name, size) in PLAIN {
            partitions.insert(name.to_string(), vec![SENTINEL; size]);
        }
        let vars = [
            ("product", product),
            ("serialno", SERIAL),
            ("unlocked", "yes"),
            ("current-slot", "a"),
            ("version-bootloader", "fake-1.0"),
            ("version-baseband", "fake-radio-1.0"),
            ("slot-count", "2"),
        ];
        FakePhone(Rc::new(RefCell::new(FakeState {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            partitions,
            log: Vec::new(),
            active_slot: "a".into(),
            rebooted: false,
            max_download: 256 << 10,
            fail_on: Vec::new(),
            short_data: false,
            take_at_most: None,
            swap_serial_after: None,
            staged: None,
            expecting: 0,
            incoming: Vec::new(),
            replies: VecDeque::new(),
        })))
    }

    pub fn edit(&self, f: impl FnOnce(&mut FakeState)) -> &Self {
        f(&mut self.0.borrow_mut());
        self
    }

    pub fn partition(&self, name: &str) -> Vec<u8> {
        self.0.borrow().partitions[name].clone()
    }

    pub fn log(&self) -> Vec<String> {
        self.0.borrow().log.clone()
    }

    /// Names of the partitions that no longer hold only the sentinel.
    pub fn written(&self) -> Vec<String> {
        self.0
            .borrow()
            .partitions
            .iter()
            .filter(|(_, d)| d.iter().any(|b| *b != SENTINEL))
            .map(|(n, _)| n.clone())
            .collect()
    }
}

fn le32(b: &[u8]) -> usize {
    u32::from_le_bytes(b[..4].try_into().unwrap()) as usize
}
fn le16(b: &[u8]) -> usize {
    u16::from_le_bytes(b[..2].try_into().unwrap()) as usize
}

/// Apply an Android sparse image to `target`, as a bootloader does.
pub fn unsparse(image: &[u8], target: &mut [u8]) -> core::result::Result<(), String> {
    if image.len() < 28 || le32(image) != 0xed26_ff3a {
        return Err("not a sparse image".into());
    }
    let (major, file_hdr, chunk_hdr) = (le16(&image[4..]), le16(&image[8..]), le16(&image[10..]));
    let (blk, total_blks, chunks) = (le32(&image[12..]), le32(&image[16..]), le32(&image[20..]));
    if major != 1 || file_hdr != 28 || chunk_hdr != 12 || blk == 0 || blk % 4 != 0 {
        return Err("unsupported sparse header".into());
    }
    if total_blks * blk > target.len() {
        return Err("sparse image larger than the partition".into());
    }
    let (mut at, mut block) = (file_hdr, 0usize);
    for _ in 0..chunks {
        if image.len() < at + 12 {
            return Err("truncated chunk header".into());
        }
        let (kind, n, total_sz) = (
            le16(&image[at..]),
            le32(&image[at + 4..]),
            le32(&image[at + 8..]),
        );
        let data = image.get(at + 12..at + total_sz).ok_or("truncated chunk")?;
        if block + n > total_blks {
            return Err("chunks run past total_blks".into());
        }
        let out = &mut target[block * blk..(block + n) * blk];
        match kind {
            0xcac1 if data.len() == n * blk => out.copy_from_slice(data),
            0xcac2 if data.len() == 4 => out
                .chunks_exact_mut(4)
                .for_each(|w| w.copy_from_slice(data)),
            0xcac3 if data.is_empty() => {}
            _ => return Err(format!("bad chunk {kind:#x}")),
        }
        block += n;
        at += total_sz;
    }
    if block != total_blks || at != image.len() {
        return Err("chunks don't add up to total_blks".into());
    }
    Ok(())
}

impl FakeState {
    fn reply(&mut self, kind: &str, text: &str) {
        self.replies.push_back(format!("{kind}{text}").into_bytes());
    }

    fn getvar(&self, name: &str) -> Option<String> {
        if name == "serialno" && self.swap_serial_after.is_some_and(|n| self.log.len() > n) {
            return Some("ANOTHERPHONE0002".into());
        }
        if name == "max-download-size" {
            return Some(format!("0x{:x}", self.max_download));
        }
        if let Some(p) = name.strip_prefix("partition-size:") {
            return self.partitions.get(p).map(|d| format!("0x{:x}", d.len()));
        }
        self.vars.get(name).cloned()
    }

    fn command(&mut self, cmd: &str) {
        self.log.push(cmd.to_string());
        if self.fail_on.iter().any(|p| cmd.starts_with(p.as_str())) {
            return self.reply("FAIL", "injected failure");
        }
        if cmd == "getvar:all" {
            let lines: Vec<String> = self.vars.iter().map(|(k, v)| format!("{k}:{v}")).collect();
            for l in lines {
                self.reply("INFO", &l);
            }
            self.reply("OKAY", "");
        } else if let Some(name) = cmd.strip_prefix("getvar:") {
            match self.getvar(name) {
                Some(v) => self.reply("OKAY", &v),
                None => self.reply("FAIL", "unknown variable"),
            }
        } else if let Some(hex) = cmd.strip_prefix("download:") {
            match usize::from_str_radix(hex, 16) {
                Ok(n) if hex.len() == 8 && n <= self.max_download => {
                    let n = if self.short_data { n / 2 } else { n };
                    self.expecting = n;
                    self.incoming.clear();
                    self.staged = None;
                    self.reply("DATA", &format!("{n:08x}"));
                }
                Ok(_) => self.reply("FAIL", "data too large"),
                Err(_) => self.reply("FAIL", "bad download size"),
            }
        } else if let Some(part) = cmd.strip_prefix("flash:") {
            let Some(data) = self.staged.take() else {
                return self.reply("FAIL", "nothing downloaded");
            };
            if self.vars.get("unlocked").map(String::as_str) != Some("yes") {
                return self.reply("FAIL", "device is locked");
            }
            let Some(target) = self.partitions.get_mut(part) else {
                return self.reply("FAIL", "partition does not exist");
            };
            let result = if data.len() >= 4 && le32(&data) == 0xed26_ff3a {
                unsparse(&data, target)
            } else if data.len() <= target.len() {
                target[..data.len()].copy_from_slice(&data);
                Ok(())
            } else {
                Err("image larger than the partition".into())
            };
            match result {
                Ok(()) => {
                    self.reply("INFO", "writing");
                    self.reply("OKAY", "");
                }
                Err(why) => self.reply("FAIL", &why),
            }
        } else if let Some(slot) = cmd.strip_prefix("set_active:") {
            self.active_slot = slot.to_string();
            self.reply("OKAY", "");
        } else if cmd == "reboot" {
            self.rebooted = true;
            self.reply("OKAY", "");
        } else {
            // erase:, format:, oem, flashing, ...: a test that sees one fails
            self.reply("FAIL", "unknown command");
        }
    }
}

impl Transport for FakePhone {
    async fn send(&mut self, data: &[u8]) -> Result<usize> {
        let mut s = self.0.borrow_mut();
        if s.rebooted {
            return Err(Error::Transport("the phone left the bus".into()));
        }
        if s.expecting > 0 {
            let take = data
                .len()
                .min(s.take_at_most.unwrap_or(usize::MAX))
                .min(s.expecting - s.incoming.len());
            s.incoming.extend_from_slice(&data[..take]);
            if s.incoming.len() == s.expecting {
                s.expecting = 0;
                s.staged = Some(core::mem::take(&mut s.incoming));
                s.reply("OKAY", "");
            }
            return Ok(take);
        }
        match core::str::from_utf8(data) {
            Ok(cmd) if data.len() <= 64 => s.command(cmd),
            _ => s.reply("FAIL", "not a command"),
        }
        Ok(data.len())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        self.0
            .borrow_mut()
            .replies
            .pop_front()
            .ok_or_else(|| Error::Transport("timed out waiting for the phone".into()))
    }
}
