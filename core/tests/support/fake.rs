// SPDX-License-Identifier: MIT
//! A fake phone in fastboot mode: a transport that speaks the protocol
//! (AOSP system/core/fastboot/README.md) against partitions held in memory,
//! with switches for the ways a real one can refuse. Test code only.
//!
//! It applies what is flashed, sparse images included (AOSP libsparse's
//! format, parsed here independently of the core's writer), so a test can
//! compare the partitions afterwards with what should be on them.
#![allow(dead_code)]
use omamobile_installer_core::fastboot::{Transport, GET_UNLOCK_ABILITY};
use omamobile_installer_core::{Error, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

pub const SENTINEL: u8 = 0xa5;
/// A placeholder, never a real serial number.
pub const SERIAL: &str = "FAKESERIAL0001";

/// What `reboot-bootloader` does to the pretend phone: how its restart into
/// the bootloader ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeRestart {
    /// It leaves the bus and comes back in Fastboot Mode, running the
    /// bootloader of the active slot.
    Normal,
    /// It confirms and never leaves the bus: nothing restarts.
    NeverLeaves,
    /// Restarted into this slot, it starts an operating system instead of
    /// staying in Fastboot Mode: gone until its owner's buttons
    /// ([`FakePhone::owner_buttons`]).
    BootsOsOn(char),
    /// Restarted into slot b, its bootloader does not start: the phone
    /// falls back to slot a by itself, which it marks active, and marks b
    /// unbootable.
    FallsBack,
    /// It leaves and never comes back by itself.
    NeverBack,
}

pub struct FakeState {
    pub vars: BTreeMap<String, String>,
    pub partitions: BTreeMap<String, Vec<u8>>,
    /// Every command received, in order.
    pub log: Vec<String>,
    pub active_slot: String,
    pub rebooted: bool,
    pub max_download: usize,
    /// The INFO line that answers `flashing get_unlock_ability`. `None`: a
    /// bootloader that doesn't know the command, and answers FAIL.
    pub unlock_ability: Option<String>,
    // Faults
    /// Answer FAIL to a command starting with any of these.
    pub fail_on: Vec<String>,
    /// Answer `download:` with a DATA size smaller than asked.
    pub short_data: bool,
    /// Take at most this many bytes of each data write.
    pub take_at_most: Option<usize>,
    /// After this many commands, report another serial number.
    pub swap_serial_after: Option<usize>,
    /// The cable is pulled once this many commands have been received: the
    /// next one never arrives, and nothing is answered until [`FakePhone::replug`].
    pub unplug_at: Option<usize>,
    /// The cable is pulled while the phone works on command number N (from
    /// 0): the phone carries it out, and its answer never arrives.
    pub lose_answer_at: Option<usize>,
    /// The cable is pulled as the Nth command (from 1) that starts with
    /// this text is sent: it never arrives. For a front end's pretend
    /// phone, which cannot count the questions before a write.
    pub unplug_on: Option<(String, usize)>,
    // The two slots, for a phone that tells the version of the bootloader
    // that runs only (the Pixel 7 Pro)
    /// Each slot's bootloader version (a, b). With it, `version-bootloader`
    /// and `current-slot` are the running slot's, from each restart on.
    pub slot_versions: Option<(String, String)>,
    /// The slot whose bootloader runs.
    pub running: String,
    /// `current-slot` follows `set_active` at once, before any restart:
    /// the pretend phone of the other tests. A pretend phone with
    /// `slot_versions` says the running slot until it restarts.
    pub current_follows_flag: bool,
    /// What `reboot-bootloader` does.
    pub restart: FakeRestart,
    /// Restarting: `Some(n)`, back after n more ticks; `Some(usize::MAX)`,
    /// not back until the owner's buttons. Off the bus meanwhile.
    pub away: Option<usize>,
    /// How many restarts into the bootloader it made.
    pub restarts: usize,
    /// The phone moves to its other slot, as after a restart into it (a
    /// fallback, or a `set_active` by hand and a restart), when the Nth
    /// (from 1) `getvar:current-slot` arrives: from that question on,
    /// `current-slot` reads this slot.
    pub slot_moves_at: Option<(usize, String)>,
    unplugged: bool,
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
            unlock_ability: None,
            fail_on: Vec::new(),
            short_data: false,
            take_at_most: None,
            swap_serial_after: None,
            unplug_at: None,
            lose_answer_at: None,
            unplug_on: None,
            slot_versions: None,
            running: "a".into(),
            current_follows_flag: true,
            restart: FakeRestart::Normal,
            away: None,
            restarts: 0,
            slot_moves_at: None,
            unplugged: false,
            staged: None,
            expecting: 0,
            incoming: Vec::new(),
            replies: VecDeque::new(),
        })))
    }

    /// A phone in one of the states an install can meet, by name: for the
    /// flow's tests and for the front ends' pretend phone.
    pub fn scenario(name: &str) -> Option<Self> {
        let phone = FakePhone::new(if name == "other-model" {
            "panther"
        } else {
            "cheetah"
        });
        let set = |pairs: &[(&str, &str)]| {
            phone.edit(|s| {
                for (k, v) in pairs {
                    s.vars.insert(k.to_string(), v.to_string());
                }
            });
        };
        // A bootloader that reports both slots' versions, as the flow needs
        let slots = |v: &str| {
            set(&[
                ("version-bootloader", v),
                ("version-bootloader-a", v),
                ("version-bootloader-b", v),
            ])
        };
        slots("fake-1.0");
        match name {
            "ready" | "other-model" => {}
            // Locked, with "OEM unlocking" switched on in Developer options
            "locked" => {
                set(&[("unlocked", "no")]);
                phone.edit(|s| s.unlock_ability = Some("get_unlock_ability: 1".into()));
            }
            // Locked, and "OEM unlocking" still off
            "locked-oem-off" => {
                set(&[("unlocked", "no")]);
                phone.edit(|s| s.unlock_ability = Some("get_unlock_ability: 0".into()));
            }
            "slot-b" => set(&[("current-slot", "b")]),
            // The Pixel 7 Pro's bootloader on a reinstall: it runs from slot
            // B and tells the version of the copy in use only
            "slot-b-one-slot" => {
                set(&[("current-slot", "b")]);
                phone.edit(|s| {
                    s.vars.remove("version-bootloader-a");
                    s.vars.remove("version-bootloader-b");
                });
            }
            "one-slot" => {
                phone.edit(|s| {
                    s.vars.remove("version-bootloader-a");
                    s.vars.remove("version-bootloader-b");
                });
            }
            "slots-differ" => set(&[("version-bootloader-b", "fake-0.9")]),
            "unknown-build" => {
                slots("fake-7.7");
                set(&[("version-baseband", "fake-radio-7.7")]);
            }
            "untested-build" => {
                slots("fake-1.2");
                set(&[("version-baseband", "fake-radio-1.2")]);
            }
            "untested-old-build" => {
                slots("fake-0.9");
                set(&[("version-baseband", "fake-radio-0.9")]);
            }
            // A first install of the Pixel 7 Pro: stock Android on slot a,
            // one bootloader copy told, two slots with their versions and
            // flags, and a restart into the bootloader that works
            "first-install" => {
                phone.first_install("fake-1.0");
            }
            "first-install-locked" => {
                phone.first_install("fake-1.0");
                set(&[("unlocked", "no")]);
                phone.edit(|s| s.unlock_ability = Some("get_unlock_ability: 1".into()));
            }
            "first-install-differ" => {
                phone.first_install("fake-0.9");
            }
            "first-install-follows" => {
                phone.first_install("fake-1.0");
                phone.edit(|s| s.current_follows_flag = true);
            }
            "first-install-stays" => {
                phone.first_install("fake-1.0");
                phone.edit(|s| s.restart = FakeRestart::NeverLeaves);
            }
            "first-install-os" => {
                phone.first_install("fake-1.0");
                phone.edit(|s| s.restart = FakeRestart::BootsOsOn('b'));
            }
            "first-install-falls-back" => {
                phone.first_install("fake-1.0");
                phone.edit(|s| s.restart = FakeRestart::FallsBack);
            }
            "first-install-b-started" => {
                phone.first_install("fake-1.0");
                set(&[("slot-successful:b", "yes"), ("slot-unbootable:b", "no")]);
            }
            // For a release that installs on the running slot: a Pixel 7
            // Pro as stock leaves it, on slot a; the same on slot b (stock
            // after a system update); and one that moves to slot b between
            // the first look and the dry run
            "running-a" => {
                phone.first_install("fake-1.0");
            }
            "running-b" => {
                phone.first_install("fake-1.0");
                phone.edit(|s| {
                    s.active_slot = "b".into();
                    s.running = "b".into();
                    s.vars.insert("current-slot".into(), "b".into());
                });
            }
            "running-a-moves" => {
                phone.first_install("fake-1.0");
                phone.edit(|s| s.slot_moves_at = Some((2, "b".into())));
            }
            _ => return None,
        }
        Some(phone)
    }

    pub fn edit(&self, f: impl FnOnce(&mut FakeState)) -> &Self {
        f(&mut self.0.borrow_mut());
        self
    }

    /// A Pixel 7 Pro as it comes from stock Android: on slot a, telling the
    /// version of the running bootloader only, slot b's bootloader at
    /// `slot_b`, and the slot flags of a phone whose slot b never started.
    pub fn first_install(&self, slot_b: &str) -> &Self {
        self.edit(|s| {
            s.vars.remove("version-bootloader-a");
            s.vars.remove("version-bootloader-b");
            s.slot_versions = Some(("fake-1.0".into(), slot_b.into()));
            s.current_follows_flag = false;
            for (k, v) in [
                ("slot-successful:a", "yes"),
                ("slot-unbootable:a", "no"),
                ("slot-retry-count:a", "3"),
                ("slot-successful:b", "no"),
                ("slot-unbootable:b", "yes"),
                ("slot-retry-count:b", "0"),
            ] {
                s.vars.insert(k.into(), v.into());
            }
        })
    }

    /// Whether the phone is on the bus in Fastboot Mode now, as a front end
    /// that lists USB devices sees it.
    pub fn listed(&self) -> bool {
        let s = self.0.borrow();
        s.away.is_none() && !s.unplugged
    }

    /// A restart into the bootloader is under way (or never ended): off the
    /// bus, as a front end that lists USB devices sees it.
    pub fn restarting(&self) -> bool {
        self.0.borrow().away.is_some()
    }

    /// Time passes: a restart under way gets one step nearer its end.
    pub fn tick(&self) {
        let back = {
            let mut s = self.0.borrow_mut();
            match s.away {
                Some(usize::MAX) | None => false,
                Some(0) => true,
                Some(n) => {
                    s.away = Some(n - 1);
                    false
                }
            }
        };
        if back {
            self.edit(|s| s.come_back());
        }
    }

    /// The owner brings the phone to Fastboot Mode with its buttons: it runs
    /// the active slot's bootloader.
    pub fn owner_buttons(&self) -> &Self {
        self.replug();
        self.edit(|s| {
            s.away = Some(0);
            s.come_back()
        })
    }

    /// The active slot, as the bootloader keeps it.
    pub fn active(&self) -> String {
        self.0.borrow().active_slot.clone()
    }

    /// The names [`Self::scenario`] knows.
    pub const SCENARIOS: [&'static str; 22] = [
        "ready",
        "locked",
        "locked-oem-off",
        "other-model",
        "slot-b",
        "slot-b-one-slot",
        "one-slot",
        "slots-differ",
        "unknown-build",
        "untested-build",
        "untested-old-build",
        "first-install",
        "first-install-locked",
        "first-install-differ",
        "first-install-follows",
        "first-install-stays",
        "first-install-os",
        "first-install-falls-back",
        "first-install-b-started",
        "running-a",
        "running-b",
        "running-a-moves",
    ];

    /// The owner plugs the cable back in, with the phone still in Fastboot
    /// Mode: what was downloaded and not flashed is gone, as is any answer
    /// that was on its way.
    pub fn replug(&self) -> &Self {
        self.edit(|s| {
            s.unplug_at = None;
            s.lose_answer_at = None;
            s.unplug_on = None;
            s.unplugged = false;
            s.staged = None;
            s.expecting = 0;
            s.incoming.clear();
            s.replies.clear();
        })
    }

    /// The commands received that change a phone: everything but `getvar:`,
    /// `download:` (held in memory until a flash) and the unlock question.
    pub fn changes(&self) -> Vec<String> {
        self.not_reads()
            .into_iter()
            .filter(|c| !c.starts_with("download:"))
            .collect()
    }

    pub fn partition(&self, name: &str) -> Vec<u8> {
        self.0.borrow().partitions[name].clone()
    }

    pub fn log(&self) -> Vec<String> {
        self.0.borrow().log.clone()
    }

    /// What was received that is neither `getvar:` nor the one read-only
    /// question of the flashing family: for a flow that only reads, nothing.
    pub fn not_reads(&self) -> Vec<String> {
        self.0
            .borrow()
            .log
            .iter()
            .filter(|c| !c.starts_with("getvar:") && *c != GET_UNLOCK_ABILITY)
            .cloned()
            .collect()
    }

    /// How many times `flashing get_unlock_ability` was received.
    pub fn unlock_questions(&self) -> usize {
        let log = &self.0.borrow().log;
        log.iter().filter(|c| *c == GET_UNLOCK_ABILITY).count()
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
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
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
    /// Back in Fastboot Mode after a restart, running the active slot's
    /// bootloader.
    fn come_back(&mut self) {
        self.away = None;
        self.replies.clear();
        self.staged = None;
        self.running = self.active_slot.clone();
        self.vars
            .insert("current-slot".into(), self.running.clone());
        if let Some((a, b)) = &self.slot_versions {
            let v = if self.running == "a" { a } else { b }.clone();
            self.vars.insert("version-bootloader".into(), v);
        }
    }

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
        if cmd == "getvar:current-slot" {
            if let Some((nth, slot)) = self.slot_moves_at.clone() {
                let asked = self.log.iter().filter(|c| *c == cmd).count();
                if asked == nth {
                    self.vars.insert("current-slot".into(), slot.clone());
                    self.running = slot.clone();
                    self.active_slot = slot;
                }
            }
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
            if slot != "a" && slot != "b" {
                return self.reply("FAIL", "no such slot");
            }
            self.active_slot = slot.to_string();
            // The other tests' pretend phone reports the active slot as the
            // current one at once
            if self.current_follows_flag {
                self.vars.insert("current-slot".into(), slot.to_string());
            }
            // As the Pixel 9a's bootloader does (Omamobile's AGENTS.md):
            // the slot made active loses its successful mark and gets tries
            if self.vars.contains_key(&format!("slot-successful:{slot}")) {
                for (k, v) in [
                    ("successful", "no"),
                    ("unbootable", "no"),
                    ("retry-count", "7"),
                ] {
                    self.vars.insert(format!("slot-{k}:{slot}"), v.into());
                }
            }
            self.reply("OKAY", "");
        } else if cmd == "reboot-bootloader" {
            self.reply("OKAY", "");
            self.restarts += 1;
            match self.restart {
                FakeRestart::NeverLeaves => {}
                FakeRestart::Normal => self.away = Some(2),
                FakeRestart::NeverBack => self.away = Some(usize::MAX),
                FakeRestart::BootsOsOn(slot) if self.active_slot == slot.to_string() => {
                    self.away = Some(usize::MAX)
                }
                FakeRestart::BootsOsOn(_) => self.away = Some(2),
                FakeRestart::FallsBack if self.active_slot == "b" => {
                    self.active_slot = "a".into();
                    self.vars.insert("slot-unbootable:b".into(), "yes".into());
                    self.away = Some(2);
                }
                FakeRestart::FallsBack => self.away = Some(2),
            }
        } else if cmd == "reboot" {
            self.rebooted = true;
            self.reply("OKAY", "");
        } else if cmd == GET_UNLOCK_ABILITY && self.unlock_ability.is_some() {
            // The one question of the flashing family: it reads
            let line = self.unlock_ability.clone().unwrap_or_default();
            self.reply("INFO", &line);
            self.reply("OKAY", "");
        } else {
            // erase:, format:, oem, flashing unlock, ...: a test that sees one fails
            self.reply("FAIL", "unknown command");
        }
    }
}

impl Transport for FakePhone {
    async fn send(&mut self, data: &[u8]) -> Result<usize> {
        let mut s = self.0.borrow_mut();
        if s.rebooted || s.unplugged || s.away.is_some() {
            return Err(Error::Transport("the phone left the bus".into()));
        }
        if s.expecting == 0 && s.unplug_at.is_some_and(|n| s.log.len() >= n) {
            s.unplugged = true;
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
        let lose_answer = s.lose_answer_at == Some(s.log.len());
        if let (Some((prefix, nth)), Ok(cmd)) = (s.unplug_on.clone(), core::str::from_utf8(data)) {
            let before = s.log.iter().filter(|c| c.starts_with(&prefix)).count();
            if cmd.starts_with(&prefix) && before + 1 == nth {
                s.unplugged = true;
                return Err(Error::Transport("the phone left the bus".into()));
            }
        }
        match core::str::from_utf8(data) {
            Ok(cmd) if data.len() <= 64 => s.command(cmd),
            _ => s.reply("FAIL", "not a command"),
        }
        if lose_answer {
            s.unplugged = true;
            s.expecting = 0;
            s.replies.clear();
        }
        Ok(data.len())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        let mut s = self.0.borrow_mut();
        if s.unplugged {
            return Err(Error::Transport("the phone left the bus".into()));
        }
        if s.away.is_some() && s.replies.is_empty() {
            return Err(Error::Transport("the phone left the bus".into()));
        }
        s.replies
            .pop_front()
            .ok_or_else(|| Error::Transport("timed out waiting for the phone".into()))
    }
}
