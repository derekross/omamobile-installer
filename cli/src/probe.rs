// SPDX-License-Identifier: MIT
//! The other-slot measurement on this computer (docs/DESIGN.md §6): the
//! restarts it asks for, watched on the USB bus by the phone's serial
//! number, and its record in the work directory, so that the next run puts
//! slot A back when this one could not. The core decides everything; this
//! file lends transports, watches and keeps a file.
use omamobile_installer_core::fastboot::{PhoneId, Transport};
use omamobile_installer_core::slot_probe::{ProbeNext, ProbeRecord, Restart, SlotProbe};
use omamobile_installer_core::{Error, Result as CoreResult};
use pollster::block_on;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The record's file in the work directory.
pub(crate) const RECORD: &str = "slot-probe";

/// How often the bus is looked at while the phone restarts.
const LOOK_EVERY: Duration = Duration::from_millis(200);

pub(crate) fn record_path(work_dir: Option<&Path>) -> Option<PathBuf> {
    work_dir
        .map(Path::to_path_buf)
        .or_else(crate::default_work_dir)
        .map(|d| d.join(RECORD))
}

/// What an earlier run kept, as text, if anything.
pub(crate) fn load_record(path: Option<&Path>) -> Option<String> {
    std::fs::read_to_string(path?).ok()
}

/// Keep what the measurement says to keep, or delete what was kept.
pub(crate) fn keep_record(path: Option<&Path>, record: Option<ProbeRecord>) {
    let Some(path) = path else { return };
    let done = match record {
        Some(record) => path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, record.to_text())),
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    };
    if let Err(e) = done {
        eprintln!(
            "(the measurement's record could not be kept in {}: {e})",
            path.display()
        );
    }
}

/// A transport to no phone, for a stretch the phone could not be opened
/// for: every command fails, and the measurement ends as a lost link.
pub(crate) struct NoLink(pub String);

impl Transport for NoLink {
    async fn send(&mut self, _: &[u8]) -> CoreResult<usize> {
        Err(Error::Transport(self.0.clone()))
    }
    async fn recv(&mut self) -> CoreResult<Vec<u8>> {
        Err(Error::Transport(self.0.clone()))
    }
}

/// What a front end lends the measurement: the phone, opened anew for each
/// stretch, and eyes on the bus.
pub(crate) trait ProbeSeams {
    type Link: Transport;
    /// The pinned phone in Fastboot Mode, opened; `None` if it is not there.
    fn open(&mut self) -> Result<Option<(Self::Link, PhoneId)>, String>;
    /// Is the phone with the pinned serial number on the bus in Fastboot
    /// Mode now?
    fn listed(&mut self) -> bool;
    /// Wait before the next look: real time on USB, a tick for a pretend
    /// phone.
    fn pause(&mut self);
    /// How many pauses make a second.
    fn per_second(&self) -> u32;
}

/// Watch a restart within the core's limits: first for the phone to leave
/// the bus, then for it to come back. "Back" is only ever what was seen.
pub(crate) fn watch<S: ProbeSeams>(seams: &mut S, leave_within: u32, back_within: u32) -> Restart {
    let per = seams.per_second().max(1);
    let mut gone = false;
    for _ in 0..leave_within * per {
        if !seams.listed() {
            gone = true;
            break;
        }
        seams.pause();
    }
    if !gone {
        return Restart::NeverLeft;
    }
    for _ in 0..back_within * per {
        seams.pause();
        if seams.listed() {
            return Restart::LeftAndBack;
        }
    }
    Restart::NotBack
}

/// Run a measurement (or a putting back) to its end. `say` gets each line
/// for the person; `keep` the record after every step.
pub(crate) fn run<S: ProbeSeams>(
    probe: &mut SlotProbe,
    seams: &mut S,
    say: &mut dyn FnMut(&str),
    keep: &mut dyn FnMut(Option<ProbeRecord>),
) {
    let mut shown = 0;
    loop {
        let next = match seams.open() {
            Ok(Some((mut link, id))) if &id == probe.phone() => block_on(probe.step(&mut link)),
            Ok(Some(_)) => block_on(probe.step(&mut NoLink(
                "another phone is connected in Fastboot Mode".into(),
            ))),
            Ok(None) => block_on(probe.step(&mut NoLink(
                "the phone is not connected in Fastboot Mode".into(),
            ))),
            Err(e) => block_on(probe.step(&mut NoLink(e))),
        };
        keep(probe.record());
        for line in &probe.log()[shown..] {
            say(line);
        }
        shown = probe.log().len();
        match next {
            ProbeNext::Finished => return,
            ProbeNext::Restart {
                leave_within,
                back_within,
                note,
                ..
            } => {
                say(&note);
                let started = Instant::now();
                let seen = watch(seams, leave_within, back_within);
                say(&format!(
                    "{} ({:.1} s)",
                    match seen {
                        Restart::LeftAndBack =>
                            "the phone left the bus and came back in Fastboot Mode",
                        Restart::NeverLeft => "the phone did not leave the bus",
                        Restart::NotBack => "the phone left the bus and did not come back in time",
                    },
                    started.elapsed().as_secs_f64()
                ));
                if let Err(e) = probe.restart_seen(seen) {
                    say(&format!("{e}"));
                    return;
                }
                keep(probe.record());
            }
        }
    }
}

/// The phone on USB, by its serial number.
pub(crate) struct Usb<'a> {
    pub serial: &'a str,
}

impl ProbeSeams for Usb<'_> {
    type Link = omamobile_installer_usb::UsbTransport;
    fn open(&mut self) -> Result<Option<(Self::Link, PhoneId)>, String> {
        block_on(crate::open_usb(self.serial)).map_err(|e| e.to_string())
    }
    fn listed(&mut self) -> bool {
        nusb::MaybeFuture::wait(nusb::list_devices()).is_ok_and(|mut devices| {
            devices.any(|d| crate::is_fastboot(&d) && d.serial_number() == Some(self.serial))
        })
    }
    fn pause(&mut self) {
        std::thread::sleep(LOOK_EVERY);
    }
    fn per_second(&self) -> u32 {
        (1000 / LOOK_EVERY.as_millis()) as u32
    }
}

/// The tests' pretend phone: its restarts advance by ticks, not seconds.
#[cfg(feature = "fake-phone")]
pub(crate) struct Pretend {
    pub phone: crate::fake::FakePhone,
}

#[cfg(feature = "fake-phone")]
impl ProbeSeams for Pretend {
    type Link = crate::fake::FakePhone;
    fn open(&mut self) -> Result<Option<(Self::Link, PhoneId)>, String> {
        if !self.phone.listed() {
            return Ok(None);
        }
        let serial = self
            .phone
            .0
            .borrow()
            .vars
            .get("serialno")
            .cloned()
            .unwrap_or_default();
        Ok(PhoneId::of_serial(&serial).map(|id| (self.phone.clone(), id)))
    }
    fn listed(&mut self) -> bool {
        self.phone.listed()
    }
    fn pause(&mut self) {
        self.phone.tick();
    }
    fn per_second(&self) -> u32 {
        1
    }
}
