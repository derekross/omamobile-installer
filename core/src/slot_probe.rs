// SPDX-License-Identifier: MIT
//! Measuring the other bootloader slot: the version a first install needs
//! and a Pixel 7 Pro does not report (docs/DESIGN.md §6, "Measuring the
//! other slot").
//!
//! An install ends in `set_active:b`, which makes the phone run slot B's
//! bootloader, and that bootloader has to be a version the release was
//! tested with. A phone that runs from slot A and tells only the version of
//! the copy in use is measured here, with its owner's yes:
//!
//! ```text
//! on slot A:  getvar:all, set_active:b, getvar:current-slot (must still be a),
//!             reboot-bootloader
//!             ... the phone leaves the bus and comes back ...
//! on slot B:  getvar:serialno (the pinned phone), getvar:current-slot (b),
//!             getvar:version-bootloader (slot B's), getvar:all,
//!             set_active:a, reboot-bootloader
//!             ... the phone leaves the bus and comes back ...
//! on slot A:  getvar:serialno, getvar:current-slot (a),
//!             getvar:version-bootloader (what it was), getvar:all
//! ```
//!
//! Nothing is flashed, erased or booted: the only commands beyond questions
//! are `set_active:a`, `set_active:b` and `reboot-bootloader`, sent through a
//! client of their own that the gate lets send those three and nothing else
//! (`fastboot::Grant::SlotProbe`). The bootloader keeps its slot flags in its
//! own storage, so they are a write all the same; they are read before, on
//! the other slot and after, and the end says what changed.
//!
//! The core does no I/O: the front end lends a transport for each stretch
//! between two restarts ([`SlotProbe::step`]), carries out each restart it is
//! asked for ([`ProbeNext::Restart`]), and says what it saw
//! ([`SlotProbe::restart_seen`]): the phone left the bus and came back, never
//! left, or did not come back in time. "Back" is only ever what was seen:
//! a phone that never left the bus has not restarted, whatever it says.
//!
//! Every way it can end leads back to slot A as the active slot, as far as
//! the phone still answers: a step that fails sends `set_active:a` while the
//! phone is reachable. A phone that did not come back, or another phone that
//! answered, gets nothing more; the record ([`SlotProbe::record`], kept by
//! the front end) lets the next run put slot A back on the same phone
//! ([`SlotProbe::put_back`]), and the stop gives the owner the buttons and
//! the command.
use crate::fastboot::{Client, Grant, PhoneId, Transport};
use crate::{Error, Result};

/// How long a front end waits for the phone to leave the bus after the
/// restart was confirmed: a bootloader that restarts drops off USB at once.
pub const LEAVE_WITHIN_SECONDS: u32 = 15;
/// How long it waits, from the moment the phone left, for it to be back in
/// Fastboot Mode. Measured nowhere yet: the first attended run records it.
pub const BACK_WITHIN_SECONDS: u32 = 90;

/// The flags a bootloader reports for its slots, by name.
const FLAG_NAMES: [&str; 4] = [
    "slot-successful",
    "slot-unbootable",
    "slot-retry-count",
    "slot-fastboot-ok",
];

/// What the phone said of its slots at one point of the measurement.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlotReading {
    /// `current-slot`.
    pub current: String,
    /// `version-bootloader`: the version of the bootloader that answers.
    pub bootloader: String,
    /// Each slot flag the phone reported in `getvar:all`, as `name:slot`
    /// and its value, in the order of [`FLAG_NAMES`] and slot.
    pub flags: Vec<(String, String)>,
}

impl SlotReading {
    /// The slot flags among `getvar:all`'s lines (`slot-successful:a:yes`).
    pub(crate) fn flags_of(all: &[String]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for name in FLAG_NAMES {
            for slot in ['a', 'b'] {
                let key = format!("{name}:{slot}");
                let value = all.iter().find_map(|line| {
                    line.strip_prefix(&key)?
                        .strip_prefix(':')
                        .map(|v| v.trim().to_string())
                });
                if let Some(value) = value {
                    out.push((key, value));
                }
            }
        }
        out
    }

    pub fn flag(&self, key: &str) -> Option<&str> {
        self.flags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// One line for a person: `current slot a, bootloader X; slot-successful:a yes, ...`.
    pub fn line(&self) -> String {
        let flags: Vec<String> = self.flags.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let flags = if flags.is_empty() {
            "no slot flags reported".to_string()
        } else {
            flags.join(", ")
        };
        format!(
            "current slot {}, bootloader {}; {flags}",
            self.current, self.bootloader
        )
    }
}

/// Whether slot B reads as a slot that has started and can start again: what
/// an installed Omarchy Mobile sent back to slot A looks like, and stock
/// Android after a system update. The two can't be told apart from
/// fastboot, so neither is measured.
pub(crate) fn slot_b_reads_as_started(flags: &[(String, String)]) -> bool {
    let get = |k: &str| flags.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    get("slot-successful:b") == Some("yes") && get("slot-unbootable:b") == Some("no")
}

/// What the front end saw after a restart it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// The phone left the bus and came back in Fastboot Mode with the same
    /// serial number, in time.
    LeftAndBack,
    /// The phone stayed listed: it did not restart.
    NeverLeft,
    /// It left and was not back in Fastboot Mode in time.
    NotBack,
}

/// What the front end does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeNext {
    /// The phone was told to restart into the bootloader of slot `into`.
    /// Wait for it to leave the bus (at most `leave_within` seconds), then
    /// for it to be back in Fastboot Mode with the same serial number (at
    /// most `back_within` seconds from when it left); hand in what was seen
    /// ([`SlotProbe::restart_seen`]), then a transport to the phone that came
    /// back ([`SlotProbe::step`]), or to the one that stayed, if it never
    /// left. `note` says it to the person, in the core's words.
    Restart {
        into: char,
        leave_within: u32,
        back_within: u32,
        note: String,
    },
    /// The measurement is over: hand it back to the flow
    /// ([`crate::flow::Flow::slot_probe_done`]).
    Finished,
}

/// Which slot is the active one when the measurement ends, as far as the
/// phone confirmed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Active {
    /// The first slot is active, and its bootloader is the one that runs:
    /// read after a restart, or never left.
    First,
    /// `set_active` for the first slot was confirmed, but the other slot's
    /// bootloader still runs: one restart into the bootloader puts it right.
    FirstSet,
    /// The other slot may be the active one: the phone was not reachable to
    /// set the first slot back.
    NotKnown,
}

/// Why the other slot was not measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeTrouble {
    /// The phone is not the pinned one, or no longer on the first slot with
    /// the version it had, at the start: nothing was sent.
    NotAsLookedAt(String),
    /// Slot B reads as started and startable ([`slot_b_reads_as_started`]):
    /// nothing was sent.
    SlotBStarted,
    /// The phone refused to make the other slot active.
    SetActiveRefused(String),
    /// Right after `set_active`, before any restart, `current-slot` already
    /// named the other slot: on this bootloader it can't show which
    /// bootloader runs, so a restart could not be proven.
    CurrentFollowsFlag,
    /// The phone confirmed the restart and never left the bus.
    NeverLeft,
    /// It left the bus and was not back in Fastboot Mode in time: it may have
    /// started an operating system, or not started at all.
    NotBack { into: char },
    /// Another phone answered after a restart.
    OtherPhone,
    /// After the restart into the other slot it came back on the first slot.
    CameBackOnFirst,
    /// On the other slot, the phone refused to make the first slot active.
    PutBackRefused(String),
    /// After the restart back, the other slot still runs.
    BackOnOther,
    /// After the restart back, the first slot's bootloader is not the
    /// version it was.
    FirstChanged { was: String, now: String },
    /// After the restart back, the first slot's flags say it can't start.
    FirstNotStartable(String),
    /// The link to the phone failed.
    Lost(Error),
}

/// How the measurement ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeEnd {
    /// Measured: `other` is the other slot's bootloader version; the phone is
    /// back on the first slot with the version it had. `changed`: the slot
    /// flags that differ from before, each as `name: before -> after`.
    Measured { other: String, changed: Vec<String> },
    /// Not measured, and which slot is active as far as is known.
    NotMeasured { why: ProbeTrouble, active: Active },
    /// A run that put the first slot back after an earlier one ended with
    /// the other slot active ([`SlotProbe::put_back`]).
    PutBack,
}

/// What a run of the measurement leaves to remember, for the next run, while
/// the first slot may not be the active one: the phone (by its digest), and
/// which slot is the first. The front end keeps it as text in its work
/// directory ([`Self::to_text`]) and hands it to the next flow
/// ([`crate::flow::FlowOptions::slot_probe_record`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRecord {
    pub(crate) phone: PhoneId,
    pub(crate) first: char,
}

impl ProbeRecord {
    const HEAD: &'static str = "omamobile-slot-probe 1";

    pub fn to_text(&self) -> String {
        format!(
            "{}\nphone {}\nfirst {}\n",
            Self::HEAD,
            self.phone.digest(),
            self.first
        )
    }

    /// The record as [`Self::to_text`] wrote it, and nothing else.
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != Self::HEAD {
            return None;
        }
        let digest = lines.next()?.strip_prefix("phone ")?;
        let first = match lines.next()?.strip_prefix("first ")? {
            "a" => 'a',
            "b" => 'b',
            _ => return None,
        };
        if lines.next().is_some() {
            return None;
        }
        Some(ProbeRecord {
            phone: PhoneId::of_digest(digest)?,
            first,
        })
    }

    /// The first slot, which the record's run started from.
    pub fn first(&self) -> char {
        self.first
    }
}

/// The other slot's version, as a measurement read it on this phone, for
/// the checks that follow (the flow's later looks and each session's). Only
/// [`SlotProbe`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredSlot {
    pub(crate) phone: PhoneId,
    pub(crate) first: char,
    pub(crate) version: String,
}

impl MeasuredSlot {
    /// The other slot's bootloader version.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// The slot the phone ran from: the one whose version it tells.
    pub fn first(&self) -> char {
        self.first
    }
    /// The phone it was measured on.
    pub fn phone(&self) -> &PhoneId {
        &self.phone
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    /// On the first slot, nothing sent yet.
    Begin,
    /// Told to restart into the other slot.
    AwaitOther,
    /// Seen to restart: the next transport is the other slot's bootloader.
    OnOther,
    /// It never left: set the first slot back on the same bootloader.
    StayedOnFirst,
    /// Told to restart back into the first slot.
    AwaitFirst,
    /// Seen to restart back.
    BackOnFirst,
    /// Putting the first slot back after an earlier run (`put_back`).
    PutBackBegin,
    PutBackAwait,
    PutBackBack,
    Ended,
}

/// One measurement of the other bootloader slot, or one putting-back of the
/// first. Made by the flow only ([`crate::flow::Flow::slot_probe`]), for a
/// phone it judged and pinned, once the owner said yes.
pub struct SlotProbe {
    phone: PhoneId,
    first: char,
    other: char,
    /// The first slot's version, as the flow's look read it.
    first_version: String,
    stage: Stage,
    /// The three readings: before, on the other slot, after.
    readings: Vec<(&'static str, SlotReading)>,
    other_version: Option<String>,
    end: Option<ProbeEnd>,
    /// Every command sent, in order, and each step in words.
    log: Vec<String>,
    /// The other slot may be the active one: from `set_active` for it until
    /// the first slot was set and seen to run again.
    other_may_be_active: bool,
    /// `set_active` for the first slot was confirmed since.
    first_set: bool,
}

impl core::fmt::Debug for SlotProbe {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotProbe")
            .field("stage", &self.stage)
            .field("end", &self.end)
            .finish_non_exhaustive()
    }
}

fn slot_name(slot: char) -> String {
    slot.to_ascii_uppercase().to_string()
}

impl SlotProbe {
    /// A measurement from slot `first` of the phone `phone`, whose bootloader
    /// reported `first_version` at the flow's look.
    pub(crate) fn measure(phone: PhoneId, first: char, first_version: String) -> Self {
        let other = if first == 'a' { 'b' } else { 'a' };
        SlotProbe {
            phone,
            first,
            other,
            first_version,
            stage: Stage::Begin,
            readings: Vec::new(),
            other_version: None,
            end: None,
            log: Vec::new(),
            other_may_be_active: false,
            first_set: false,
        }
    }

    /// Put the first slot back as the active one, after a run that ended
    /// without it ([`ProbeRecord`]): the phone is told `set_active` for the
    /// first slot, and, if it runs the other slot's bootloader, to restart,
    /// and must be seen to come back on the first.
    pub(crate) fn put_back(record: &ProbeRecord) -> Self {
        let mut probe = Self::measure(record.phone.clone(), record.first, String::new());
        probe.stage = Stage::PutBackBegin;
        probe.other_may_be_active = true;
        probe
    }

    /// The phone it is for.
    pub fn phone(&self) -> &PhoneId {
        &self.phone
    }

    pub fn is_finished(&self) -> bool {
        self.stage == Stage::Ended
    }

    /// How it ended, once it has.
    pub fn end(&self) -> Option<&ProbeEnd> {
        self.end.as_ref()
    }

    /// What was sent (each command as it went out) and read, in order, as
    /// lines for a person.
    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// The readings so far: `before`, `on the other slot`, `after`.
    pub fn readings(&self) -> &[(&'static str, SlotReading)] {
        &self.readings
    }

    /// The first slot.
    pub fn first(&self) -> char {
        self.first
    }

    /// What to keep for the next run now: `Some` while the other slot may be
    /// the active one, or the first was set and not seen to run again.
    /// `None`: nothing to keep (delete what was kept). A front end writes it
    /// after every [`Self::step`] and [`Self::restart_seen`].
    pub fn record(&self) -> Option<ProbeRecord> {
        let open = match &self.end {
            None => self.other_may_be_active,
            Some(ProbeEnd::NotMeasured { active, .. }) => *active != Active::First,
            Some(_) => false,
        };
        open.then(|| ProbeRecord {
            phone: self.phone.clone(),
            first: self.first,
        })
    }

    /// The measurement, if it read the other slot and the phone is back on
    /// the first: for the checks that follow.
    pub(crate) fn measured(&self) -> Option<MeasuredSlot> {
        match &self.end {
            Some(ProbeEnd::Measured { other, .. }) => Some(MeasuredSlot {
                phone: self.phone.clone(),
                first: self.first,
                version: other.clone(),
            }),
            _ => None,
        }
    }

    fn finish(&mut self, end: ProbeEnd) -> ProbeNext {
        self.log.push(match &end {
            ProbeEnd::Measured { other, .. } => format!(
                "measured: slot {}'s bootloader is {other}; slot {} is active again",
                self.other, self.first
            ),
            ProbeEnd::NotMeasured { why, active } => {
                format!("not measured ({why:?}); active: {active:?}")
            }
            ProbeEnd::PutBack => format!("slot {} is active again", self.first),
        });
        self.end = Some(end);
        self.stage = Stage::Ended;
        ProbeNext::Finished
    }

    fn restart(&mut self, into: char) -> ProbeNext {
        let note = if into == self.first {
            format!("The phone is restarting into its bootloader again, back on slot {}. Leave it connected and don't press its buttons.", slot_name(into))
        } else {
            format!("The phone is restarting into slot {}'s bootloader. Leave it connected and don't press its buttons: it should come back to Fastboot Mode by itself.", slot_name(into))
        };
        ProbeNext::Restart {
            into,
            leave_within: LEAVE_WITHIN_SECONDS,
            back_within: BACK_WITHIN_SECONDS,
            note,
        }
    }

    /// The other slot's restart, or the first's, was seen to end as `seen`.
    /// Refused unless a restart was asked for.
    pub fn restart_seen(&mut self, seen: Restart) -> Result<()> {
        match (&self.stage, seen) {
            (Stage::AwaitOther, Restart::LeftAndBack) => self.stage = Stage::OnOther,
            (Stage::AwaitOther, Restart::NeverLeft) => self.stage = Stage::StayedOnFirst,
            (Stage::AwaitOther, Restart::NotBack) => {
                let into = self.other;
                self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::NotBack { into },
                    active: Active::NotKnown,
                });
            }
            (Stage::AwaitFirst, Restart::LeftAndBack) => self.stage = Stage::BackOnFirst,
            (Stage::AwaitFirst, Restart::NeverLeft) => {
                self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::NeverLeft,
                    active: Active::FirstSet,
                });
            }
            (Stage::AwaitFirst, Restart::NotBack) => {
                let into = self.first;
                self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::NotBack { into },
                    active: Active::FirstSet,
                });
            }
            (Stage::PutBackAwait, Restart::LeftAndBack) => self.stage = Stage::PutBackBack,
            (Stage::PutBackAwait, Restart::NeverLeft) => {
                self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::NeverLeft,
                    active: Active::FirstSet,
                });
            }
            (Stage::PutBackAwait, Restart::NotBack) => {
                let into = self.first;
                self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::NotBack { into },
                    active: Active::FirstSet,
                });
            }
            _ => {
                return Err(Error::Refused(
                    "no restart was asked for at this point of the measurement".into(),
                ))
            }
        }
        Ok(())
    }

    /// The next stretch of the measurement, on `transport`: the phone in
    /// Fastboot Mode now (the one that came back after a restart). Runs until
    /// a restart is asked for or the end.
    pub async fn step<T: Transport>(&mut self, transport: &mut T) -> ProbeNext {
        // One log, in the order things happened: the client adds each
        // command as it is sent, the steps their readings
        let mut client = Client::granted(transport, Grant::SlotProbe);
        client.log = core::mem::take(&mut self.log);
        let next = self.run(&mut client).await;
        // What `finish` said comes last, as it was
        let mut log = core::mem::take(&mut client.log);
        log.append(&mut self.log);
        self.log = log;
        next
    }

    async fn run<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        match self.stage {
            Stage::Begin => self.begin(client).await,
            Stage::OnOther => self.on_other(client).await,
            Stage::StayedOnFirst => self.stayed_on_first(client).await,
            Stage::BackOnFirst => self.back_on_first(client).await,
            Stage::PutBackBegin => self.put_back_begin(client).await,
            Stage::PutBackBack => self.put_back_back(client).await,
            // Waiting for what the front end saw of a restart, or over
            Stage::AwaitOther | Stage::AwaitFirst | Stage::PutBackAwait => {
                let into = if self.stage == Stage::AwaitOther {
                    self.other
                } else {
                    self.first
                };
                self.restart(into)
            }
            Stage::Ended => ProbeNext::Finished,
        }
    }

    /// The phone's serial number, against the pinned one.
    async fn pinned<T: Transport>(&self, client: &mut Client<T>) -> Result<bool> {
        let serial = client.getvar("serialno").await?;
        Ok(PhoneId::of_serial(&serial).as_ref() == Some(&self.phone))
    }

    async fn reading<T: Transport>(&self, client: &mut Client<T>) -> Result<SlotReading> {
        let current = client
            .getvar("current-slot")
            .await?
            .trim_start_matches('_')
            .to_string();
        let bootloader = client.getvar("version-bootloader").await?;
        let all = client.getvar_all().await?;
        Ok(SlotReading {
            current,
            bootloader,
            flags: SlotReading::flags_of(&all),
        })
    }

    /// `set_active` for the first slot, while the phone answers: where a
    /// step failed, so that the end leaves the first slot active.
    async fn set_first<T: Transport>(&mut self, client: &mut Client<T>) -> Result<()> {
        client
            .simple(&format!("set_active:{}", self.first))
            .await
            .map(|_| self.first_set = true)
    }

    /// Ends with `why`, after setting the first slot active again. `ran_on`:
    /// which slot's bootloader runs now, to say which flag is confirmed.
    async fn back_out<T: Transport>(
        &mut self,
        client: &mut Client<T>,
        why: ProbeTrouble,
        first_runs: bool,
    ) -> ProbeNext {
        let active = match self.set_first(client).await {
            Ok(()) if first_runs => Active::First,
            Ok(()) => Active::FirstSet,
            Err(_) if !self.other_may_be_active => Active::First,
            Err(_) => Active::NotKnown,
        };
        self.finish(ProbeEnd::NotMeasured { why, active })
    }

    async fn begin<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        let before = match async {
            let pinned = self.pinned(client).await?;
            let reading = self.reading(client).await?;
            Ok::<_, Error>((pinned, reading))
        }
        .await
        {
            Ok(got) => got,
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::First,
                })
            }
        };
        let (pinned, reading) = before;
        let first = self.first.to_string();
        let why = if !pinned {
            Some("another phone answered than the one the installer looked at".to_string())
        } else if reading.current != first {
            Some(format!("the phone runs from slot {}", reading.current))
        } else if reading.bootloader != self.first_version {
            Some(format!(
                "its bootloader is {} now, not the {} the installer read",
                reading.bootloader, self.first_version
            ))
        } else {
            None
        };
        if let Some(why) = why {
            return self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::NotAsLookedAt(why),
                active: Active::First,
            });
        }
        if slot_b_reads_as_started(&reading.flags) {
            return self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::SlotBStarted,
                active: Active::First,
            });
        }
        client.log.push(format!("before: {}", reading.line()));
        self.readings.push(("before", reading));
        // From here the other slot may be the active one
        self.other_may_be_active = true;
        match client.simple(&format!("set_active:{}", self.other)).await {
            Ok(_) => {}
            Err(Error::DeviceFail { message, .. }) => {
                return self
                    .back_out(client, ProbeTrouble::SetActiveRefused(message), true)
                    .await
            }
            Err(e) => return self.back_out(client, ProbeTrouble::Lost(e), true).await,
        }
        // Before any restart the bootloader that runs is the first slot's;
        // a current-slot that already names the other can't show a restart
        match client.getvar("current-slot").await {
            Ok(slot) if slot.trim_start_matches('_') == first => {}
            Ok(_) => {
                return self
                    .back_out(client, ProbeTrouble::CurrentFollowsFlag, true)
                    .await
            }
            Err(e) => return self.back_out(client, ProbeTrouble::Lost(e), true).await,
        }
        match client.simple("reboot-bootloader").await {
            Ok(_) => {
                self.stage = Stage::AwaitOther;
                let other = self.other;
                self.restart(other)
            }
            Err(e) => self.back_out(client, ProbeTrouble::Lost(e), true).await,
        }
    }

    async fn stayed_on_first<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        // It never left the bus: the first slot's bootloader still runs.
        // Only the pinned phone is told anything
        match self.pinned(client).await {
            Ok(true) => self.back_out(client, ProbeTrouble::NeverLeft, true).await,
            Ok(false) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::OtherPhone,
                active: Active::NotKnown,
            }),
            Err(e) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::Lost(e),
                active: Active::NotKnown,
            }),
        }
    }

    async fn on_other<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        match self.pinned(client).await {
            Ok(true) => {}
            // Nothing more to a phone that is not the pinned one
            Ok(false) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::OtherPhone,
                    active: Active::NotKnown,
                })
            }
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::NotKnown,
                })
            }
        }
        let reading = match self.reading(client).await {
            Ok(r) => r,
            Err(e) => return self.back_out(client, ProbeTrouble::Lost(e), false).await,
        };
        client
            .log
            .push(format!("on slot {}: {}", self.other, reading.line()));
        let on_other = reading.current == self.other.to_string();
        let version = reading.bootloader.clone();
        self.readings.push(("on the other slot", reading));
        if !on_other {
            // It came back on the first slot: whose bootloader answered is
            // not the other's, so nothing was measured
            return self
                .back_out(client, ProbeTrouble::CameBackOnFirst, true)
                .await;
        }
        if version.is_empty() {
            return self
                .back_out(
                    client,
                    ProbeTrouble::Lost(Error::Phone(
                        "the phone reports no version-bootloader".into(),
                    )),
                    false,
                )
                .await;
        }
        self.other_version = Some(version);
        match self.set_first(client).await {
            Ok(()) => {}
            Err(Error::DeviceFail { message, .. }) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::PutBackRefused(message),
                    active: Active::NotKnown,
                })
            }
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::NotKnown,
                })
            }
        }
        match client.simple("reboot-bootloader").await {
            Ok(_) => {
                self.stage = Stage::AwaitFirst;
                let first = self.first;
                self.restart(first)
            }
            Err(e) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::Lost(e),
                active: Active::FirstSet,
            }),
        }
    }

    async fn back_on_first<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        match self.pinned(client).await {
            Ok(true) => {}
            Ok(false) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::OtherPhone,
                    active: Active::FirstSet,
                })
            }
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::FirstSet,
                })
            }
        }
        let after = match self.reading(client).await {
            Ok(r) => r,
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::FirstSet,
                })
            }
        };
        client.log.push(format!("after: {}", after.line()));
        let first = self.first.to_string();
        if after.current != first {
            self.readings.push(("after", after));
            return self
                .back_out(client, ProbeTrouble::BackOnOther, false)
                .await;
        }
        // From here the first slot is active and its bootloader runs
        self.other_may_be_active = false;
        let before = self
            .readings
            .first()
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        let changed: Vec<String> = before
            .flags
            .iter()
            .map(|(k, v)| (k, Some(v.as_str()), after.flag(k)))
            .chain(
                after
                    .flags
                    .iter()
                    .filter(|(k, _)| before.flag(k).is_none())
                    .map(|(k, v)| (k, None, Some(v.as_str()))),
            )
            .filter(|(_, was, now)| was != now)
            .map(|(k, was, now)| {
                format!(
                    "{k}: {} -> {}",
                    was.unwrap_or("not reported"),
                    now.unwrap_or("not reported")
                )
            })
            .collect();
        let first_version = self.first_version.clone();
        let now_version = after.bootloader.clone();
        let not_startable = match (
            after.flag(&format!("slot-unbootable:{first}")),
            after.flag(&format!("slot-successful:{first}")),
            after.flag(&format!("slot-retry-count:{first}")),
        ) {
            (Some("yes"), _, _) => Some(format!("slot-unbootable:{first} reads yes")),
            (_, Some(s), Some("0")) if s != "yes" => Some(format!(
                "slot-successful:{first} reads {s} and slot-retry-count:{first} reads 0"
            )),
            _ => None,
        };
        self.readings.push(("after", after));
        if now_version != first_version {
            return self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::FirstChanged {
                    was: first_version,
                    now: now_version,
                },
                active: Active::First,
            });
        }
        if let Some(why) = not_startable {
            return self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::FirstNotStartable(why),
                active: Active::First,
            });
        }
        let other = self.other_version.clone().unwrap_or_default();
        self.finish(ProbeEnd::Measured { other, changed })
    }

    async fn put_back_begin<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        match self.pinned(client).await {
            Ok(true) => {}
            Ok(false) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::OtherPhone,
                    active: Active::NotKnown,
                })
            }
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::NotKnown,
                })
            }
        }
        let current = match client.getvar("current-slot").await {
            Ok(c) => c.trim_start_matches('_').to_string(),
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::NotKnown,
                })
            }
        };
        client.log.push(format!("put back: current slot {current}"));
        // The flag may name the other slot whichever bootloader runs: it is
        // set in any case
        match self.set_first(client).await {
            Ok(()) => {}
            Err(Error::DeviceFail { message, .. }) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::PutBackRefused(message),
                    active: Active::NotKnown,
                })
            }
            Err(e) => {
                return self.finish(ProbeEnd::NotMeasured {
                    why: ProbeTrouble::Lost(e),
                    active: Active::NotKnown,
                })
            }
        }
        // The first slot's bootloader runs already: no restart is needed
        if current == self.first.to_string() {
            self.other_may_be_active = false;
            return self.finish(ProbeEnd::PutBack);
        }
        match client.simple("reboot-bootloader").await {
            Ok(_) => {
                self.stage = Stage::PutBackAwait;
                let first = self.first;
                self.restart(first)
            }
            Err(e) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::Lost(e),
                active: Active::FirstSet,
            }),
        }
    }

    async fn put_back_back<T: Transport>(&mut self, client: &mut Client<T>) -> ProbeNext {
        let checked = async {
            let pinned = self.pinned(client).await?;
            let current = client.getvar("current-slot").await?;
            Ok::<_, Error>((pinned, current.trim_start_matches('_').to_string()))
        }
        .await;
        match checked {
            Ok((true, current)) if current == self.first.to_string() => {
                self.other_may_be_active = false;
                self.finish(ProbeEnd::PutBack)
            }
            Ok((true, _)) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::BackOnOther,
                active: Active::FirstSet,
            }),
            Ok((false, _)) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::OtherPhone,
                active: Active::FirstSet,
            }),
            Err(e) => self.finish(ProbeEnd::NotMeasured {
                why: ProbeTrouble::Lost(e),
                active: Active::FirstSet,
            }),
        }
    }
}
