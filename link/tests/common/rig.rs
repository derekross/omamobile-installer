// SPDX-License-Identifier: MIT
//! A pretend phone with small stand-in partitions, an installer with a
//! small stand-in factory image, and the loopback between them. No USB, no
//! files.
use omamobile_link::digest::sha256;
use omamobile_link::msg::FactoryBegin;
use omamobile_link::msg::{Code, StepResult, StepState};
use omamobile_link::phone::{PartFound, Persistent, PhoneConfig};
use omamobile_link::sim::{SimPhone, TestRng};

/// A placeholder, as everywhere in this repository: no real serial.
pub const SERIAL: &[u8] = b"<SERIAL>";
/// Small frames, so a small image is many of them.
pub const MAX_DATA: u32 = 1024;
pub const TEST_ACK_EVERY: u64 = 4096;
/// Twice that, as on the phone: eight frames may be under way unacknowledged.
pub const TEST_WINDOW: u64 = 2 * TEST_ACK_EVERY;
/// 10 frames, the last one short; acknowledged at 4096, 8192 and the end.
pub const IMAGE_LEN: usize = 9 * 1024 + 100;

pub fn phone_config() -> PhoneConfig {
    let mut c = PhoneConfig::new("0.0.1", "cheetah", SERIAL);
    c.max_data = MAX_DATA;
    c.ack_every = TEST_ACK_EVERY;
    c.window = TEST_WINDOW;
    c
}

pub fn image() -> Vec<u8> {
    TestRng(7).bytes(IMAGE_LEN)
}

pub fn factory_begin(image: &[u8]) -> FactoryBegin {
    FactoryBegin {
        size: image.len() as u64,
        sha256: sha256(image),
        build: "test.260101.001".into(),
        name: "cheetah-test.260101.001-factory.zip".into(),
    }
}

/// Stand-in radio partitions: one of several frames with a short last one,
/// one of exactly one frame, one that is not in NV_PARTS, one tiny.
pub fn parts() -> Vec<(PartFound, Vec<u8>)> {
    let mut rng = TestRng(11);
    [
        ("efs", 3000usize, true),
        ("persist", 1024, true),
        ("modem_a", 2048, true),
        ("devinfo", 10, false),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, len, checked))| {
        let found = PartFound {
            name: name.into(),
            size: len as u64,
            checked,
            dev: format!("sda{}", i + 1),
        };
        (found, rng.bytes(len))
    })
    .collect()
}

/// A phone whose files are what a link at `state` left, the daemon started.
pub fn sim_phone(state: Persistent, new_boot: bool, seed: u64) -> SimPhone {
    let mut p = SimPhone::new(phone_config(), state, new_boot, 0, seed);
    p.parts = parts();
    p
}

/// A freshly installed phone at its first start.
pub fn new_phone(seed: u64) -> SimPhone {
    let mut p = SimPhone::first_start(phone_config(), 0, seed);
    p.parts = parts();
    p
}

/// The owner's session is up and its agent is connected to the daemon.
pub fn session_up(p: &mut SimPhone, now: u64) {
    p.feed(now, PhoneInput::SessionUp);
    p.feed(now, PhoneInput::AgentUp);
}

/// The owner taps the notice in the unlocked session: the code in the window.
pub fn tap(p: &mut SimPhone, now: u64) -> Code {
    p.feed(now, PhoneInput::AgentShow);
    p.phone.code().expect("the window shows a code")
}

/// A worker's result file: every step `ok`, or one of them `failed`.
pub fn result(failed: Option<&str>) -> Vec<StepResult> {
    [
        "firmware",
        "sensors",
        "gnss",
        "modem",
        "fingerprint",
        "camera",
    ]
    .into_iter()
    .map(|step| StepResult {
        step: step.into(),
        state: if failed == Some(step) {
            StepState::Failed
        } else {
            StepState::Ok
        },
    })
    .collect()
}

// --- The installer's side, and the loopback ---------------------------------

use super::answers;
use omamobile_link::host::{Command, Event, HostConfig};
use omamobile_link::msg::Stage;
use omamobile_link::phone::PhoneInput;
use omamobile_link::sim::{Loopback, SimHost};

pub fn host_config() -> HostConfig {
    HostConfig::new("omamobile-install", "0.0.1", SERIAL)
}

pub fn rig() -> Loopback {
    let mut host = SimHost::new(host_config());
    host.factory = image();
    Loopback::new(new_phone(1), host)
}

/// Connect and pair with the code on the phone's screen.
pub fn paired() -> Loopback {
    let mut lb = rig();
    lb.connect();
    assert!(lb.saw(|e| *e == Event::NeedCode), "{:?}", lb.host.events);
    let code = lb.phone.phone.code().expect("a code is shown");
    lb.command(Command::Pair(code));
    assert!(lb.host.host.ready(), "{:?}", lb.host.events);
    lb
}

/// Paired, the answers in, first boot done.
pub fn set_up() -> Loopback {
    let mut lb = paired();
    lb.command(Command::Answers(answers()));
    assert!(
        lb.saw(|e| *e == Event::AnswersAccepted),
        "{:?}",
        lb.host.events
    );
    lb.phone_input(PhoneInput::FirstbootEnded { failed: vec![] });
    assert_eq!(lb.phone.phone.stage(), Stage::FirstbootDone);
    lb.host.take_events();
    lb
}

/// The phone restarts: the daemon starts again from what is on the phone.
pub fn reboot(lb: &mut Loopback) {
    lb.cut();
    lb.phone.start(true, lb.now);
}
