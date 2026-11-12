// SPDX-License-Identifier: MIT
//! The install, step by step, as one state machine for every front end.
//!
//! A front end holds a [`Flow`], shows what [`Flow::screen`] says, and hands
//! in what the person did. It decides nothing: the order of the steps, every
//! refusal and every word of a stop are here, so the command-line tool and
//! the web page can't differ.
//!
//! ```text
//! begin (release verified, build index checked)
//!   -> Warning        acknowledge_warning / decline_warning
//!   -> Phone          read_phone / no_phone
//!   -> Unlock         read_phone again, after the owner unlocked
//!   -> FactoryImage   factory_zip, then the bytes, then factory_zip_done
//!   -> Answers        answers
//!   -> DryRun         dry_run, then dry_run_done
//!   -> Done
//! ```
//!
//! This version ends at the dry run. It has no step that writes to a phone.
//!
//! What the flow sends to a phone is `getvar:` and, to a locked phone it is
//! about to show the unlock steps for, the one question
//! `flashing get_unlock_ability` ([`unlock_ability`]): both read, and nothing
//! else is sent. They go through the transport a front end lends it:
//! [`Flow::read_phone`] asks the phone what it is, and [`Flow::dry_run`]
//! opens a [`Session`] that holds every write back. It never sends
//! `flashing unlock`: the unlock step shows that command for the owner to
//! run. It never switches slots.
//!
//! Every end that isn't `Done` is a [`Stop`]: a typed reason, the words for
//! the person, and a closing line that says what is true of the phone at
//! that moment and whether starting again can get past it. No stop ends in a
//! fixed reassurance.
use crate::build_index::{Build, BuildIndex, Lookup};
use crate::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use crate::fastboot::{
    probe, unlock_ability, PhoneId, PhoneOptions, PhoneReport, Session, Transport, UnlockAbility,
};
use crate::hash::StreamHasher;
use crate::signature::TrustedKeys;
use crate::{Error, Result};
use serde::Serialize;

const ISSUES: &str = "https://github.com/derekross/omarchy-mobile/issues";
const DOWNLOAD: &str = "https://omamobile.org/download";
/// The command the owner runs to unlock the bootloader. Shown, never sent:
/// the one `flashing` command this crate sends is the question
/// [`crate::fastboot::GET_UNLOCK_ABILITY`], which only reads.
pub const UNLOCK_COMMAND: &str = "fastboot flashing unlock";

/// What the person running the install said up front.
#[derive(Debug, Clone, Default)]
pub struct FlowOptions {
    /// Slot B already holds Omarchy Mobile and this installs it again.
    /// Without it a phone running from slot B stops the install.
    pub reinstall: bool,
}

/// Where the install is, and what the front end shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The one warning. Nothing is asked of a phone before it is accepted.
    Warning,
    /// A phone in Fastboot Mode is needed. `not_found`: the front end looked
    /// and none answered.
    Phone { not_found: bool },
    /// The phone is the right one and can be installed, but its bootloader
    /// is locked. The steps are for the owner; the flow sends none of them.
    /// `oem_unlocking`: what the phone said when asked whether it would take
    /// the unlock command.
    Unlock { oem_unlocking: UnlockAbility },
    /// The owner fetches Google's factory image for this build and hands the
    /// saved file over. `refused`: why the last file wasn't taken.
    FactoryImage {
        build: Build,
        /// Other builds with the same bootloader and baseband.
        same_pair: Vec<String>,
        refused: Option<ZipRefusal>,
    },
    /// The owner's answers for the first start.
    Answers,
    /// Everything is in hand for the dry run.
    DryRun,
    /// The dry run passed. The flow ends here in this version.
    Done(DryRunPlan),
    /// The install stopped. Nothing more is accepted; a new flow starts over.
    Stopped(Stop),
}

/// Why an install stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopKind {
    /// The release's signature, checksums or rules did not hold.
    ReleaseNotVerified(Error),
    /// The release carries no build index.
    NoBuildIndex,
    /// The build index is not the signed one, or breaks a rule.
    BuildIndexRefused(Error),
    /// The warning was not accepted.
    WarningDeclined,
    /// The phone is another model than the release is for.
    WrongModel { found: String },
    /// Another phone answered than the one this install looked at first.
    PhoneChanged,
    /// The phone runs from slot B, and this is not a reinstall.
    RunsFromSlotB,
    /// The bootloader reports only the current slot's version. `probe_declared`:
    /// the descriptor allows reading the other by switching slots, which
    /// this version does not do.
    BootloaderSlotsUnknown { probe_declared: bool },
    /// The two slots' bootloaders differ.
    BootloaderSlotsDiffer { a: String, b: String },
    /// The phone reports no baseband version to look up.
    BasebandUnknown,
    /// No build in the index has both the phone's bootloader and baseband.
    BuildNotListed {
        bootloader: String,
        baseband: String,
    },
    /// The phone's build is listed but not marked tested.
    BuildNotTested {
        build: String,
        newer_tested: Option<String>,
    },
    /// At the dry run the phone's bootloader was locked again.
    LockedAtDryRun,
    /// At the dry run the phone reported another bootloader or baseband than
    /// the factory image was chosen for.
    BuildChanged,
    /// The dry run's own checks refused the phone.
    DryRunRefused(Error),
}

/// What is true of the phone when the install stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneFact {
    /// No phone was asked anything.
    NotAsked,
    /// A phone was asked what it is (and, if it is locked, whether it would
    /// take an unlock command); nothing else was sent.
    OnlyRead,
    /// As `OnlyRead`, and the unlock steps were shown: the owner may have
    /// unlocked the bootloader, which erases the phone.
    OnlyReadUnlockShown,
    /// Two phones answered in turn. Each was only asked what it is.
    TwoPhones { unlock_shown: bool },
}

/// Whether starting the installer again can get past a stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Nothing has to change first.
    Whenever,
    /// Once the person has done what the message says.
    AfterFix,
    /// As things are it stops at the same place; the message says what has
    /// to change, or who to ask.
    StopsAgain,
}

/// A stop: the reason, the words, and what they leave true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub kind: StopKind,
    pub title: String,
    pub message: Vec<String>,
    pub phone: PhoneFact,
    pub retry: Retry,
}

impl Stop {
    /// The closing lines: what is true of the phone, then whether starting
    /// again helps. Always derived from the two facts, never fixed.
    pub fn footer(&self) -> Vec<String> {
        let erased =
            "If you unlocked its bootloader, that erased the phone: Android starts on it as new.";
        let mut out = match self.phone {
            PhoneFact::NotAsked => vec!["Nothing was sent to a phone.".to_string()],
            PhoneFact::OnlyRead => vec![
                "The installer only asked the phone what it is. It has written nothing to it."
                    .to_string(),
            ],
            PhoneFact::OnlyReadUnlockShown => vec![
                "The installer only asked the phone what it is and whether it can be unlocked. It has written nothing to it."
                    .to_string(),
                erased.to_string(),
            ],
            PhoneFact::TwoPhones { unlock_shown } => {
                let mut v = vec![if unlock_shown {
                    "The installer only asked each of the two phones what it is, and the first whether it can be unlocked. It has written nothing to either."
                        .to_string()
                } else {
                    "The installer only asked each of the two phones what it is. It has written nothing to either."
                        .to_string()
                }];
                if unlock_shown {
                    v.push("If you unlocked the first phone's bootloader, that erased that phone: Android starts on it as new.".to_string());
                }
                v
            }
        };
        out.push(
            match self.retry {
                Retry::Whenever => "You can start the installer again at any time.",
                Retry::AfterFix => "Start the installer again once that is done.",
                Retry::StopsAgain => {
                    "Starting the installer again as things are will stop here again."
                }
            }
            .to_string(),
        );
        out
    }

    /// A short, stable name for the kind, for front ends and tests.
    pub fn id(&self) -> &'static str {
        match self.kind {
            StopKind::ReleaseNotVerified(_) => "release-not-verified",
            StopKind::NoBuildIndex => "no-build-index",
            StopKind::BuildIndexRefused(_) => "build-index-refused",
            StopKind::WarningDeclined => "warning-declined",
            StopKind::WrongModel { .. } => "wrong-model",
            StopKind::PhoneChanged => "phone-changed",
            StopKind::RunsFromSlotB => "runs-from-slot-b",
            StopKind::BootloaderSlotsUnknown { .. } => "bootloader-slots-unknown",
            StopKind::BootloaderSlotsDiffer { .. } => "bootloader-slots-differ",
            StopKind::BasebandUnknown => "baseband-unknown",
            StopKind::BuildNotListed { .. } => "build-not-listed",
            StopKind::BuildNotTested { .. } => "build-not-tested",
            StopKind::LockedAtDryRun => "locked-at-dry-run",
            StopKind::BuildChanged => "build-changed",
            StopKind::DryRunRefused(_) => "dry-run-refused",
        }
    }
}

/// Why a file was not taken as the factory image. The flow stays at the
/// step, and the owner can hand over another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipRefusal {
    /// Not the name Google gives this build's file.
    WrongName { got: String },
    /// Not the size the signed index records.
    WrongSize { name: String, got: u64, want: u64 },
    /// The size fits and the SHA-256 doesn't.
    WrongSha256 { name: String },
}

/// Hashes the owner's factory zip as the front end reads it, a chunk at a
/// time: the file is never held whole. Made by [`Flow::factory_zip`].
pub struct FactoryZipCheck {
    name: String,
    release: String,
    build: String,
    sha256: String,
    size: u64,
    hasher: StreamHasher,
}

impl FactoryZipCheck {
    pub fn update(&mut self, chunk: &[u8]) {
        self.hasher.update(chunk);
    }
    /// Bytes hashed so far, and the file's expected size.
    pub fn progress(&self) -> (u64, u64) {
        (self.hasher.len(), self.size)
    }
}

/// Proof that the owner's file is the one Google publishes for the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFactoryZip {
    pub build: String,
    pub name: String,
    pub size: u64,
}

/// What the owner typed, before any rule.
#[derive(Clone, Default)]
pub struct AnswersInput<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub password_again: &'a str,
    /// Empty for the default, `<username>-phone`.
    pub hostname: &'a str,
    pub timezone: &'a str,
    /// Empty for no Wi-Fi.
    pub wifi_ssid: &'a str,
    /// Empty for an open network.
    pub wifi_psk: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerField {
    Username,
    Password,
    Hostname,
    Timezone,
    WifiSsid,
    WifiPsk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnswerProblem {
    pub field: AnswerField,
    pub message: String,
}

/// The owner's answers, each within the rules of Omarchy Mobile's
/// `tools/install-phone`. Kept in memory by the front end only; the flow
/// keeps a summary without the secrets.
#[derive(Clone)]
pub struct OwnerAnswers {
    pub username: String,
    pub password: String,
    pub hostname: String,
    pub timezone: String,
    /// The network's name and, unless it is open, its password.
    pub wifi: Option<(String, Option<String>)>,
}

impl core::fmt::Debug for OwnerAnswers {
    // Neither secret, and not the network's name, in debug output
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnerAnswers")
            .field("username", &self.username)
            .field("hostname", &self.hostname)
            .field("timezone", &self.timezone)
            .finish_non_exhaustive()
    }
}

/// What the plan shows of the answers: no secret, and no network name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswersSummary {
    pub username: String,
    /// Digits only gives a PIN pad on the phone.
    pub pin: bool,
    pub hostname: String,
    pub timezone: String,
    pub wifi: bool,
}

/// Names the system uses (install-phone's `RESERVED`).
const RESERVED: [&str; 34] = [
    "root",
    "alarm",
    "omamobile",
    "omandroid",
    "omarchy",
    "bin",
    "daemon",
    "mail",
    "ftp",
    "http",
    "nobody",
    "dbus",
    "tss",
    "uuidd",
    "alpm",
    "git",
    "avahi",
    "cups",
    "cups-browsed",
    "lp",
    "_talkd",
    "polkitd",
    "rtkit",
    "qemu",
    "brltty",
    "gluster",
    "rpc",
    "libvirt-qemu",
    "pcscd",
    "nvidia-persistenced",
    "sddm",
    "builder",
    "admin",
    "systemd",
];

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

impl OwnerAnswers {
    /// Apply every rule; all the problems at once, so a form can mark each
    /// field.
    pub fn check(input: &AnswersInput<'_>) -> core::result::Result<Self, Vec<AnswerProblem>> {
        let mut problems = Vec::new();
        let mut bad = |field, message: &str| {
            problems.push(AnswerProblem {
                field,
                message: message.to_string(),
            })
        };
        let u = input.username;
        let ub = u.as_bytes();
        let user_ok = (1..=32).contains(&ub.len())
            && (ub[0].is_ascii_lowercase() || ub[0] == b'_')
            && ub
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(b));
        if !user_ok {
            bad(
                AnswerField::Username,
                "Use lower-case letters, digits, - and _, starting with a letter (at most 32).",
            );
        } else if RESERVED.contains(&u) || u.starts_with("systemd-") {
            bad(
                AnswerField::Username,
                "That name is used by the system; pick your own.",
            );
        }
        if input.password.is_empty() {
            bad(AnswerField::Password, "It can't be empty.");
        } else if has_control(input.password) {
            bad(
                AnswerField::Password,
                "It can't hold a line break or another control character.",
            );
        } else if input.password != input.password_again {
            bad(AnswerField::Password, "The two don't match.");
        }
        let hostname = if input.hostname.is_empty() {
            format!("{u}-phone")
        } else {
            input.hostname.to_string()
        };
        let hb = hostname.as_bytes();
        let host_ok = (1..=63).contains(&hb.len())
            && hb.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            && hb[0] != b'-'
            && hb[hb.len() - 1] != b'-';
        // A user name may hold _, which a host name can't: only then is the
        // default itself the problem.
        if !host_ok && (user_ok || !input.hostname.is_empty()) {
            bad(
                AnswerField::Hostname,
                "Use 1 to 63 letters, digits or dashes, not starting or ending with a dash.",
            );
        }
        let tz_ok = !input.timezone.is_empty()
            && input.timezone.split('/').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
            });
        if !tz_ok {
            bad(
                AnswerField::Timezone,
                "That isn't a time zone (they look like Europe/Paris or America/New_York).",
            );
        }
        let wifi = if input.wifi_ssid.is_empty() {
            if !input.wifi_psk.is_empty() {
                bad(
                    AnswerField::WifiSsid,
                    "A Wi-Fi password needs the network's name.",
                );
            }
            None
        } else {
            if input.wifi_ssid.chars().count() > 32 || has_control(input.wifi_ssid) {
                bad(
                    AnswerField::WifiSsid,
                    "A Wi-Fi network name is at most 32 characters.",
                );
            }
            let p = input.wifi_psk;
            let printable =
                (8..=63).contains(&p.len()) && p.bytes().all(|b| (b' '..=b'~').contains(&b));
            let raw_key = p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit());
            if !(p.is_empty() || printable || raw_key) {
                bad(
                    AnswerField::WifiPsk,
                    "A Wi-Fi password is 8 to 63 characters.",
                );
            }
            Some((
                input.wifi_ssid.to_string(),
                (!p.is_empty()).then(|| p.to_string()),
            ))
        };
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(OwnerAnswers {
            username: u.to_string(),
            password: input.password.to_string(),
            hostname,
            timezone: input.timezone.to_string(),
            wifi,
        })
    }

    pub fn summary(&self) -> AnswersSummary {
        AnswersSummary {
            username: self.username.clone(),
            pin: self.password.bytes().all(|b| b.is_ascii_digit()),
            hostname: self.hostname.clone(),
            timezone: self.timezone.clone(),
            wifi: self.wifi.is_some(),
        }
    }
}

/// What the dry run found and what an install would send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunPlan {
    pub product: String,
    pub current_slot: String,
    pub bootloader: String,
    pub baseband: Option<String>,
    /// The factory image that was checked.
    pub factory_zip: VerifiedFactoryZip,
    pub answers: AnswersSummary,
    /// The writes an install would send, in order, in words.
    pub would: Vec<String>,
    /// Whether the front end walked the release's images through the dry
    /// run (each checked against the signed manifest, the root image cut
    /// into sparse images). When not, `would` comes from the manifest alone.
    pub images_walked: bool,
}

/// One screen, as text. Both front ends render this and add nothing of
/// their own to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Screen {
    /// `warning`, `phone`, `unlock`, `factory-image`, `answers`, `dry-run`,
    /// `done` or `stopped`.
    pub state: &'static str,
    /// For `stopped`: [`Stop::id`].
    pub stop: Option<&'static str>,
    pub title: String,
    pub body: Vec<String>,
    /// Numbered steps for the person.
    pub steps: Vec<ScreenStep>,
    /// A page to open in a browser, when the step has one.
    pub link: Option<String>,
    /// The file the step asks for.
    pub file: Option<FileWanted>,
    /// Why what was last handed in wasn't taken; the step is still open.
    pub problem: Option<String>,
    /// The writes an install would send (`done`).
    pub plan: Vec<String>,
    /// Closing lines: what is true of the phone, and what comes next.
    pub footer: Vec<String>,
    /// What the front end does next: `acknowledge`, `read-phone`,
    /// `take-file`, `answers`, `dry-run` or `none`.
    pub action: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScreenStep {
    pub text: String,
    /// A command for the owner to type in a terminal. Shown, never run.
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileWanted {
    pub name: String,
    pub size: u64,
    pub build: String,
}

pub struct Flow {
    release: Option<VerifiedRelease>,
    index: Option<BuildIndex>,
    options: FlowOptions,
    step: Step,
    /// The phone this install is for, from the first look at it.
    pinned: Option<PhoneId>,
    /// Whether any phone has been asked anything.
    asked: bool,
    report: Option<PhoneReport>,
    unlock_shown: bool,
    zip: Option<VerifiedFactoryZip>,
    answers: Option<AnswersSummary>,
    dry_run_open: bool,
}

fn step(text: impl Into<String>) -> ScreenStep {
    ScreenStep {
        text: text.into(),
        command: None,
    }
}

/// A file's size for a person: `3.9 GB`, or megabytes below one.
fn about(size: u64) -> String {
    if size >= 1_000_000_000 {
        format!("{:.1} GB", size as f64 / 1e9)
    } else {
        format!("{:.1} MB", size as f64 / 1e6)
    }
}

impl Flow {
    /// Verify the release and its build index, before anything else: a
    /// release that can't be used stops here, not after the owner has
    /// erased a phone for it. `index` is the bundle's `factory-builds.json`.
    /// The flow is returned either way; a failure is its first step.
    pub fn begin(
        files: &ReleaseFiles<'_>,
        index: Option<&[u8]>,
        trusted: &TrustedKeys,
        options: FlowOptions,
    ) -> Self {
        let mut flow = Flow {
            release: None,
            index: None,
            options,
            step: Step::Warning,
            pinned: None,
            asked: false,
            report: None,
            unlock_shown: false,
            zip: None,
            answers: None,
            dry_run_open: false,
        };
        let release = match verify_release(files, trusted) {
            Ok(r) => r,
            Err(e) => {
                flow.stop(
                    StopKind::ReleaseNotVerified(e.clone()),
                    Retry::AfterFix,
                    "This release did not pass its checks",
                    vec![
                        format!("The release's files did not pass their checks, so nothing of them is used ({e})."),
                        format!("Fetch the release again from {DOWNLOAD}. If it fails the same way, the files are not the ones Omarchy Mobile signed: don't install them, and tell us at {ISSUES}"),
                    ],
                );
                return flow;
            }
        };
        let name = release
            .manifest()
            .device_name
            .clone()
            .unwrap_or_else(|| release.manifest().codename.clone());
        let listed = release.has_build_index();
        let index = match index {
            _ if !listed => Err(None),
            None => Err(Some(Error::Checksum(format!(
                "{} was not fetched with the release",
                crate::build_index::FILE
            )))),
            Some(bytes) => release.build_index(bytes).map_err(Some),
        };
        flow.release = Some(release);
        match index {
            Ok(index) => flow.index = Some(index),
            Err(None) => flow.stop(
                StopKind::NoBuildIndex,
                Retry::StopsAgain,
                "This release can't be installed with this installer",
                vec![
                    format!("This release carries no list of Google's builds for the {name}, so the installer can't tell which of Google's factory images matches the phone."),
                    "It takes a release that carries one. The install guide has the steps for this release.".into(),
                ],
            ),
            Err(Some(e)) => {
                // Bytes that aren't the signed ones may be a bad fetch; a
                // signed list that breaks a rule is the release's own fault.
                let fetch = matches!(e, Error::Checksum(_));
                flow.stop(
                    StopKind::BuildIndexRefused(e.clone()),
                    if fetch { Retry::AfterFix } else { Retry::StopsAgain },
                    "The list of Google's builds did not pass its checks",
                    vec![
                        format!("The release's list of Google's builds for the {name} did not pass its checks, so it isn't used ({e})."),
                        if fetch {
                            format!("Fetch the release again from {DOWNLOAD}.")
                        } else {
                            format!("That is a fault in the release, not in your download. Please tell us at {ISSUES}")
                        },
                    ],
                )
            }
        }
        flow
    }

    pub fn step(&self) -> &Step {
        &self.step
    }
    /// The verified release, unless verifying it is what stopped the flow.
    pub fn release(&self) -> Option<&VerifiedRelease> {
        self.release.as_ref()
    }
    /// The factory image the owner handed over, once it has been checked.
    pub fn factory_zip_verified(&self) -> Option<&VerifiedFactoryZip> {
        self.zip.as_ref()
    }

    fn phone_fact(&self) -> PhoneFact {
        match (self.asked, self.unlock_shown) {
            (false, _) => PhoneFact::NotAsked,
            (true, false) => PhoneFact::OnlyRead,
            (true, true) => PhoneFact::OnlyReadUnlockShown,
        }
    }

    fn stop(&mut self, kind: StopKind, retry: Retry, title: &str, message: Vec<String>) {
        let phone = self.phone_fact();
        self.stop_with(kind, retry, phone, title, message);
    }

    fn stop_with(
        &mut self,
        kind: StopKind,
        retry: Retry,
        phone: PhoneFact,
        title: &str,
        message: Vec<String>,
    ) {
        self.step = Step::Stopped(Stop {
            kind,
            title: title.to_string(),
            message,
            phone,
            retry,
        });
    }

    fn out_of_order<T>(&self, what: &str) -> Result<T> {
        Err(Error::Refused(match &self.step {
            Step::Stopped(_) => format!("the install has stopped; {what} is not accepted"),
            Step::Warning => format!("the warning has not been accepted; {what} comes after it"),
            _ => format!("{what} is not this step"),
        }))
    }

    fn names(&self) -> (String, String) {
        let m = self.release.as_ref().map(|r| r.manifest());
        let codename = m.map(|m| m.codename.clone()).unwrap_or_default();
        let name = m
            .and_then(|m| m.device_name.clone())
            .unwrap_or_else(|| codename.clone());
        (name, codename)
    }

    fn keys_fastboot(&self) -> String {
        let (_, codename) = self.names();
        self.release
            .as_ref()
            .and_then(|r| r.rules().keys_fastboot.clone())
            .unwrap_or_else(|| {
                format!(
                    "the key combination is on the phone's page, https://omamobile.org/devices/{codename}"
                )
            })
    }

    /// The owner read the warning and accepted it. Asked once.
    pub fn acknowledge_warning(&mut self) -> Result<()> {
        if self.step != Step::Warning {
            return self.out_of_order("the warning's answer");
        }
        self.step = Step::Phone { not_found: false };
        Ok(())
    }

    /// The owner did not accept the warning: the install ends.
    pub fn decline_warning(&mut self) -> Result<()> {
        if self.step != Step::Warning {
            return self.out_of_order("the warning's answer");
        }
        self.stop(
            StopKind::WarningDeclined,
            Retry::Whenever,
            "Not accepted",
            vec!["The warning was not accepted, so the install stops here.".into()],
        );
        Ok(())
    }

    /// The front end looked and found no phone in Fastboot Mode.
    pub fn no_phone(&mut self) -> Result<()> {
        match self.step {
            Step::Phone { .. } => self.step = Step::Phone { not_found: true },
            Step::Unlock { .. } => {}
            _ => return self.out_of_order("a look for the phone"),
        }
        Ok(())
    }

    /// Ask the phone on `transport` what it is (`getvar:` only), and decide
    /// what comes next. `chosen` is the device the person picked, from its
    /// USB serial number; the phone that answers must be that one.
    ///
    /// A locked phone that passes every stop is asked one thing more, last:
    /// whether it would take the unlock command (`flashing
    /// get_unlock_ability`, which reads). No answer changes nothing but the
    /// unlock step's words.
    ///
    /// Refused, with nothing sent, until the warning has been accepted. A
    /// transport error leaves the step as it is, to try again.
    pub async fn read_phone<T: Transport>(
        &mut self,
        transport: &mut T,
        chosen: &PhoneId,
    ) -> Result<()> {
        if !matches!(self.step, Step::Phone { .. } | Step::Unlock { .. }) {
            return self.out_of_order("a look at the phone");
        }
        let report = probe(transport).await?;
        self.asked = true;
        if report.id != *chosen {
            return Err(Error::Refused(
                "the phone that answered is not the one that was chosen (serial numbers differ)"
                    .into(),
            ));
        }
        match self.judge(&report) {
            Judged::Stopped => {}
            Judged::Locked => {
                // Asked only of a phone about to be shown the unlock steps,
                // and after every other question
                let oem_unlocking = unlock_ability(transport).await;
                self.unlock_shown = true;
                self.step = Step::Unlock { oem_unlocking };
            }
            Judged::Ready { build, same_pair } => {
                self.step = Step::FactoryImage {
                    build: *build,
                    same_pair,
                    refused: None,
                };
            }
        }
        self.report = Some(report);
        Ok(())
    }

    /// Everything the install needs to know of the phone, in the order that
    /// keeps a person from erasing a phone it can't go on with: every stop
    /// comes before the unlock steps. Used for each look at the phone.
    fn judge(&mut self, report: &PhoneReport) -> Judged {
        let (name, codename) = self.names();
        let second_phone = self
            .pinned
            .as_ref()
            .is_some_and(|first| *first != report.id);
        let two_phones = PhoneFact::TwoPhones {
            unlock_shown: self.unlock_shown,
        };
        if report.product != codename {
            let found = if report.product.is_empty() {
                "unknown".to_string()
            } else {
                report.product.clone()
            };
            // Not pinned: this phone is not part of the install
            let fact = if second_phone {
                two_phones
            } else {
                self.phone_fact()
            };
            self.stop_with(
                StopKind::WrongModel { found: found.clone() },
                Retry::AfterFix,
                fact,
                "This is another phone model",
                vec![
                    format!("The phone that is connected is a '{found}', and this release is for the {name} ({codename})."),
                    format!("Connect the {name}, or get the release for this phone from {DOWNLOAD}"),
                ],
            );
            return Judged::Stopped;
        }
        match &self.pinned {
            Some(_) if second_phone => {
                self.stop_with(
                    StopKind::PhoneChanged,
                    Retry::AfterFix,
                    two_phones,
                    "This is not the phone the install was started on",
                    vec![
                        "The phone connected now is not the phone this install looked at first.".into(),
                        "Connect one phone only, the one you mean to install, and keep it connected.".into(),
                    ],
                );
                return Judged::Stopped;
            }
            Some(_) => {}
            None => self.pinned = Some(report.id.clone()),
        }
        if report.current_slot == "b" && !self.options.reinstall {
            self.stop(
                StopKind::RunsFromSlotB,
                Retry::StopsAgain,
                "This phone runs from its second system copy",
                vec![
                    "This phone is running from its second system copy (slot B). Omarchy Mobile is written there, and Android must stay on the first copy (slot A) as your way back.".into(),
                    "If Omarchy Mobile is already on this phone and you are installing it again, start the installer again and say that this is a reinstall.".into(),
                    "If the phone runs Android (it moves to slot B after an Android system update), Android has to be put on slot A first. The install guide says how.".into(),
                ],
            );
            return Judged::Stopped;
        }
        match &report.slot_bootloaders {
            Some((a, b)) if a == b => {}
            Some((a, b)) => {
                self.stop(
                    StopKind::BootloaderSlotsDiffer { a: a.clone(), b: b.clone() },
                    Retry::StopsAgain,
                    "The phone's two bootloader copies differ",
                    vec![
                        format!("The phone keeps two copies of its system (slot A and slot B), each with its own bootloader. Omarchy Mobile starts from slot B, so both have to be the same version, and they are not (A: {a}, B: {b})."),
                        "With different versions the phone may not start from slot B. Google's own tools put the same version on both: the install guide says how.".into(),
                    ],
                );
                return Judged::Stopped;
            }
            None => {
                let probe_declared = self
                    .release
                    .as_ref()
                    .is_some_and(|r| r.rules().bootloader_slot_probe);
                let why = if probe_declared {
                    "Reading the other copy's version means making that copy the active one and switching back. This release allows that for this phone, but this version of the installer does not do it yet, and it never switches a phone's slots."
                } else {
                    "Reading the other copy's version would mean making that copy the active one and switching back. The installer does not do that to this phone."
                };
                self.stop(
                    StopKind::BootloaderSlotsUnknown { probe_declared },
                    Retry::StopsAgain,
                    "The installer can't check the phone's second bootloader copy",
                    vec![
                        format!("The phone keeps two copies of its system (slot A and slot B), each with its own bootloader. Omarchy Mobile starts from slot B, so slot B's bootloader has to be the same version as slot A's, and this phone only tells the version of the copy in use ({}).", report.bootloader),
                        why.into(),
                        format!("If the two copies differ, the phone may not start from slot B, so the installer stops rather than guess. To ask about this phone, open an issue: {ISSUES}"),
                    ],
                );
                return Judged::Stopped;
            }
        }
        let Some(baseband) = report.baseband.clone() else {
            self.stop(
                StopKind::BasebandUnknown,
                Retry::StopsAgain,
                "The phone doesn't tell its baseband version",
                vec![
                    "The installer finds Google's factory image for a phone by its bootloader and baseband versions, and this phone reports no baseband version.".into(),
                    format!("Please tell us at {ISSUES}"),
                ],
            );
            return Judged::Stopped;
        };
        let lookup = match self.index.as_ref() {
            Some(index) => match index.lookup(&report.bootloader, &baseband) {
                Lookup::Tested { build, same_pair } => Ok((
                    build.clone(),
                    same_pair.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )),
                Lookup::NotTested {
                    build,
                    newer_tested,
                } => Err(Some((
                    build.build.clone(),
                    newer_tested.map(|b| b.build.clone()),
                ))),
                Lookup::NotListed => Err(None),
            },
            None => Err(None),
        };
        let after_update = "After an Android update, Android runs from slot B and the two bootloader copies can differ; the installer then stops for that, and the install guide has the way to put Android back on slot A.";
        let (build, same_pair) = match lookup {
            Ok(found) => found,
            Err(None) => {
                self.stop(
                    StopKind::BuildNotListed { bootloader: report.bootloader.clone(), baseband: baseband.clone() },
                    Retry::AfterFix,
                    "This phone's Android version isn't one this release knows",
                    vec![
                        format!("No build in the release's list of Google's builds has both this phone's bootloader ({}) and its baseband ({baseband}). The installer uses a build only when both match and the release was tested with it; there is no nearest build.", report.bootloader),
                        "If Android offers an update, install it (Settings > System > System update) and start the installer again.".into(),
                        after_update.into(),
                        format!("If Android is up to date, its build is newer than this release knows. Open an issue so it can be added: {ISSUES}"),
                    ],
                );
                return Judged::Stopped;
            }
            Err(Some((build, newer))) => {
                let mut message = vec![format!(
                    "This phone's bootloader and baseband match Google's build {build}, but no Omarchy Mobile release has been tested with that build's firmware, so the installer does not use it."
                )];
                match &newer {
                    Some(newer) => {
                        message.push(format!("The list has a newer build that was tested: {newer}. If Android offers an update to it, install it (Settings > System > System update) and start the installer again."));
                        message.push(after_update.into());
                    }
                    None => message.push(format!("No newer build in the list was tested either, so updating Android does not help. Build {build} has to be tested first: ask at {ISSUES}")),
                }
                self.stop(
                    StopKind::BuildNotTested {
                        build,
                        newer_tested: newer.clone(),
                    },
                    if newer.is_some() {
                        Retry::AfterFix
                    } else {
                        Retry::StopsAgain
                    },
                    "This phone's Android build hasn't been tested with Omarchy Mobile",
                    message,
                );
                return Judged::Stopped;
            }
        };
        if report.unlocked {
            Judged::Ready {
                build: Box::new(build),
                same_pair,
            }
        } else {
            Judged::Locked
        }
    }

    /// The owner picked a file as the factory image: its name and size are
    /// checked here, before a byte is read. `Some`: feed the file's bytes to
    /// the check, then [`Self::factory_zip_done`]. `None`: not taken, and
    /// the screen says why.
    pub fn factory_zip(&mut self, file_name: &str, size: u64) -> Result<Option<FactoryZipCheck>> {
        let Step::FactoryImage { build, refused, .. } = &mut self.step else {
            return self.out_of_order("a factory image");
        };
        // A browser that saves a file twice adds " (1)" before the ending
        let plain = match file_name
            .strip_suffix(".zip")
            .and_then(|s| s.rsplit_once(" ("))
        {
            Some((stem, n))
                if n.strip_suffix(')')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
            {
                format!("{stem}.zip")
            }
            _ => file_name.to_string(),
        };
        if plain != build.zip {
            *refused = Some(ZipRefusal::WrongName {
                got: file_name.to_string(),
            });
            return Ok(None);
        }
        if size != build.size {
            *refused = Some(ZipRefusal::WrongSize {
                name: file_name.to_string(),
                got: size,
                want: build.size,
            });
            return Ok(None);
        }
        *refused = None;
        Ok(Some(FactoryZipCheck {
            name: file_name.to_string(),
            release: self
                .index
                .as_ref()
                .map(|i| i.release().to_string())
                .unwrap_or_default(),
            build: build.build.clone(),
            sha256: build.sha256.clone(),
            size: build.size,
            hasher: StreamHasher::new(),
        }))
    }

    /// Every byte of the file has been fed to `check`.
    pub fn factory_zip_done(&mut self, check: FactoryZipCheck) -> Result<()> {
        let ours = self
            .index
            .as_ref()
            .map(|i| i.release().to_string())
            .unwrap_or_default();
        let Step::FactoryImage { build, refused, .. } = &mut self.step else {
            return self.out_of_order("a factory image");
        };
        if check.release != ours || check.build != build.build || check.sha256 != build.sha256 {
            return Err(Error::Refused(
                "the file was checked for another build or another install".into(),
            ));
        }
        let (digest, len) = check.hasher.finish();
        if len != build.size {
            *refused = Some(ZipRefusal::WrongSize {
                name: check.name,
                got: len,
                want: build.size,
            });
            return Ok(());
        }
        if digest != build.sha256 {
            *refused = Some(ZipRefusal::WrongSha256 { name: check.name });
            return Ok(());
        }
        self.zip = Some(VerifiedFactoryZip {
            build: build.build.clone(),
            name: check.name,
            size: len,
        });
        self.step = Step::Answers;
        Ok(())
    }

    /// The owner's answers, already within the rules ([`OwnerAnswers::check`]).
    /// The flow keeps the summary; the secrets stay with the front end.
    pub fn answers(&mut self, answers: &OwnerAnswers) -> Result<()> {
        if self.step != Step::Answers {
            return self.out_of_order("the owner's answers");
        }
        self.answers = Some(answers.summary());
        self.step = Step::DryRun;
        Ok(())
    }

    /// Start the dry run on the phone at `transport`: ask it again what it
    /// is, apply every rule again, then open a session that holds every
    /// write back. `Some(session)`: the checks passed; a front end that has
    /// the release's images may walk them through the session, then hands it
    /// to [`Self::dry_run_done`]. `None`: the flow stopped.
    pub async fn dry_run<T: Transport>(&mut self, mut transport: T) -> Result<Option<Session<T>>> {
        if self.step != Step::DryRun || self.dry_run_open {
            return self.out_of_order("the dry run");
        }
        let report = probe(&mut transport).await?;
        match self.judge(&report) {
            Judged::Stopped => return Ok(None),
            Judged::Locked => {
                self.stop(
                    StopKind::LockedAtDryRun,
                    Retry::AfterFix,
                    "The phone's bootloader is locked",
                    vec![
                        "The phone's bootloader was unlocked when the installer first looked, and it is locked now. Nothing can be written to a locked phone.".into(),
                        "Unlocking it again erases the phone again.".into(),
                    ],
                );
                return Ok(None);
            }
            Judged::Ready { build, .. } => {
                if self.zip.as_ref().is_none_or(|z| z.build != build.build) {
                    self.stop(
                        StopKind::BuildChanged,
                        Retry::AfterFix,
                        "The phone's firmware changed",
                        vec![
                            "The phone reports another bootloader or baseband than when the factory image was chosen, so that file is no longer known to be the right one.".into(),
                            "Start the installer again: it finds the build for the phone as it is now.".into(),
                        ],
                    );
                    return Ok(None);
                }
            }
        }
        self.report = Some(report.clone());
        let release = self
            .release
            .clone()
            .expect("a flow past the warning has a release");
        let options = PhoneOptions {
            reinstall: self.options.reinstall,
            // Never the owner's word for the other slot: the flow stops instead
            bootloader_slots_confirmed: None,
            dry_run: true,
        };
        match Session::connect_pinned(transport, &release, &report.id, options).await {
            Ok(session) => {
                self.dry_run_open = true;
                Ok(Some(session))
            }
            Err(e) => {
                let lost = matches!(e, Error::Transport(_));
                self.stop(
                    StopKind::DryRunRefused(e.clone()),
                    if lost { Retry::AfterFix } else { Retry::StopsAgain },
                    "The dry run stopped",
                    vec![
                        format!("The checks an install starts with did not pass ({e})."),
                        if lost {
                            "The connection to the phone was lost. Check the cable and keep the phone in Fastboot Mode.".into()
                        } else {
                            format!("If this looks wrong for your phone, tell us at {ISSUES}")
                        },
                    ],
                );
                Ok(None)
            }
        }
    }

    /// The session [`Self::dry_run`] opened returned an error while the front
    /// end walked the release's images through it: the install stops.
    pub fn dry_run_failed(&mut self, error: &Error) -> Result<()> {
        if self.step != Step::DryRun || !self.dry_run_open {
            return self.out_of_order("the dry run's end");
        }
        self.dry_run_open = false;
        let lost = matches!(error, Error::Transport(_));
        self.stop(
            StopKind::DryRunRefused(error.clone()),
            if lost { Retry::AfterFix } else { Retry::StopsAgain },
            "The dry run stopped",
            vec![
                format!("The dry run stopped partway ({error})."),
                if lost {
                    "The connection to the phone was lost. Check the cable and keep the phone in Fastboot Mode.".into()
                } else {
                    format!("If this looks wrong for your phone, tell us at {ISSUES}")
                },
            ],
        );
        Ok(())
    }

    /// The dry run is over: read what it would have sent, and give the
    /// transport back.
    pub fn dry_run_done<T: Transport>(&mut self, session: Session<T>) -> Result<T> {
        if self.step != Step::DryRun || !self.dry_run_open {
            return self.out_of_order("the dry run's end");
        }
        let ours = self.release.as_ref().map(|r| r.id());
        if !session.is_dry_run() || Some(session.release_id()) != ours {
            return Err(Error::Refused("not the dry run this install opened".into()));
        }
        let walked = session.is_done();
        let would: Vec<String> = if walked {
            // Sparse images of the root image, counted rather than listed
            let mut out = Vec::new();
            let mut userdata = 0usize;
            for line in session
                .log()
                .iter()
                .filter_map(|l| l.strip_prefix("would: "))
            {
                if line.ends_with("flash:userdata") {
                    userdata += 1;
                    continue;
                }
                if userdata > 0 {
                    out.push(format!(
                        "flash userdata: the root image, in {userdata} pieces (erases userdata)"
                    ));
                    userdata = 0;
                }
                out.push(line.to_string());
            }
            out
        } else {
            session.plan()
        };
        let facts = session.facts().clone();
        let (Some(factory_zip), Some(answers)) = (self.zip.clone(), self.answers.clone()) else {
            return self.out_of_order("the dry run's end");
        };
        self.dry_run_open = false;
        self.step = Step::Done(DryRunPlan {
            product: facts.product,
            current_slot: facts.current_slot,
            bootloader: facts.bootloader,
            baseband: facts.baseband,
            factory_zip,
            answers,
            would,
            images_walked: walked,
        });
        Ok(session.into_transport())
    }

    /// The current step as text. See [`Screen`].
    pub fn screen(&self) -> Screen {
        let (name, codename) = self.names();
        let keys = self.keys_fastboot();
        let label = self
            .release
            .as_ref()
            .map(|r| r.manifest().label.clone())
            .unwrap_or_default();
        let mut s = Screen {
            state: "",
            stop: None,
            title: String::new(),
            body: Vec::new(),
            steps: Vec::new(),
            link: None,
            file: None,
            problem: None,
            plan: Vec::new(),
            footer: Vec::new(),
            action: "none",
        };
        let developer_options = String::from("In Settings > About phone, tap \"Build number\" seven times. Then in Settings > System > Developer options, switch on \"OEM unlocking\". If it is greyed out, connect the phone to the internet once (it stays grey until then after a reset); if it is still grey, the phone is most likely locked to its carrier and can't run Omarchy Mobile.");
        match &self.step {
            Step::Warning => {
                s.state = "warning";
                s.action = "acknowledge";
                s.title = "Read this once".into();
                s.body = vec!["It is the only warning; nothing will ask again.".into()];
                s.steps = vec![
                    step("The phone is erased. Everything on it is lost: photos, messages, apps, accounts and two-factor codes. Copy off what you want to keep first. If its bootloader is still locked, unlocking it (the next step) is what erases it, before anything of Omarchy Mobile is written."),
                    step(format!("Omarchy Mobile replaces Android as the phone's system. There are no Google apps and no Play Store, and Android apps don't run on it. This release is a {label}: some things a phone normally does are missing or unfinished. The list for this phone: https://omamobile.org/devices/{codename}")),
                    step("You can go back. Android stays on the phone, unused, as a quick way back, and Google's own tools can put the phone back as it was sold. The install guide's \"Going back to Android\" has the steps."),
                ];
                s.footer = vec!["Nothing is asked of the phone until you accept.".into()];
            }
            Step::Phone { not_found } => {
                s.state = "phone";
                s.action = "read-phone";
                s.title = format!("Connect the {name}");
                if *not_found {
                    s.problem = Some("No phone in Fastboot Mode answered.".into());
                }
                s.body = vec![
                    format!("Connect the {name} with a USB cable that carries data, and put it in Fastboot Mode: {keys}."),
                    "The installer then asks the phone what it is. That reads; it changes nothing on the phone.".into(),
                ];
                s.footer = vec!["Connect one phone only.".into()];
            }
            Step::Unlock { oem_unlocking } => {
                s.state = "unlock";
                s.action = "read-phone";
                s.title = "Unlock the bootloader".into();
                s.body = vec![
                    format!("The {name} is in Fastboot Mode, and its bootloader is still locked."),
                    "The installer has checked what it can before you erase anything: this is the right model, it runs from slot A or is a reinstall, its two bootloader copies match, and this release was tested with its Android build.".into(),
                    "Unlocking is the moment the phone is erased, as the warning said: copy off photos, messages and two-factor codes before you do it.".into(),
                ];
                // What the phone itself said about "OEM unlocking", when it
                // said anything; otherwise the steps cover both cases
                let switch_on = match oem_unlocking {
                    UnlockAbility::Allowed => {
                        s.body.push("The phone says that \"OEM unlocking\" is already switched on, so it will take the unlock command.".into());
                        None
                    }
                    UnlockAbility::NotAllowed => {
                        s.body.push("The phone says that \"OEM unlocking\" is not switched on yet. Until it is, the phone refuses the unlock command, so switch it on in Android's Developer options first (step 1).".into());
                        Some(format!("Switch on \"OEM unlocking\": press Power to start Android. {developer_options} Then put the phone back in Fastboot Mode: {keys}."))
                    }
                    UnlockAbility::Unknown => Some(format!("If \"OEM unlocking\" is not switched on yet: press Power to start Android. {developer_options} Then put the phone back in Fastboot Mode: {keys}.")),
                };
                s.body.push(
                    "The installer does not send the unlock command. You do, when you are ready:"
                        .into(),
                );
                s.steps = switch_on.into_iter().map(step).collect();
                s.steps.extend([
                    ScreenStep {
                        text: "On a computer with Android's fastboot tool, with the phone in Fastboot Mode, run:".into(),
                        command: Some(UNLOCK_COMMAND.into()),
                    },
                    step("On the phone, press a volume key until it says \"Unlock the bootloader\", then press Power. The phone erases itself and starts Android again."),
                    step(format!("Bring it back to Fastboot Mode ({keys}) and let the installer look again.")),
                ]);
                s.footer = vec!["The installer has only asked the phone what it is and whether it can be unlocked. It has written nothing to it.".into()];
            }
            Step::FactoryImage {
                build,
                same_pair,
                refused,
            } => {
                s.state = "factory-image";
                s.action = "take-file";
                s.title = "Google's factory image".into();
                s.body = vec![
                    "The phone's Wi-Fi, Bluetooth, sound, camera and modem need files that only Google may hand out. They are in Google's \"factory image\" for the phone. Omarchy Mobile never ships them, and the installer never downloads them: you download the image from Google, and the installer checks the file you saved.".into(),
                    format!("This phone's bootloader and baseband match Google's build {}, which release {} was tested with.", build.build, build.tested_with.as_deref().unwrap_or("")),
                ];
                if !same_pair.is_empty() {
                    s.body.push(format!(
                        "(Also in the list with the same two versions, which can't be told apart from here: {}. {} is the tested one, and the one used.)",
                        same_pair.join(", "),
                        build.build
                    ));
                }
                s.steps = vec![
                    step("Open Google's page (the link below) and accept Google's terms there."),
                    step(format!("Find build {} in the list for \"{codename}\" and save its file. It is about {}: {}", build.build, about(build.size), build.zip)),
                    step("Hand the saved file to the installer. It checks the file's size and SHA-256 against the release's signed list."),
                ];
                s.link = Some(build.page.clone());
                s.file = Some(FileWanted {
                    name: build.zip.clone(),
                    size: build.size,
                    build: build.build.clone(),
                });
                s.problem = refused.as_ref().map(|r| match r {
                    ZipRefusal::WrongName { got } => format!("{got} is not the file for this phone. The file to save from Google's page is {}", build.zip),
                    ZipRefusal::WrongSize { name, got, want } if got < want => format!("{name} is {got} bytes, and the file should be {want}: the download isn't complete."),
                    ZipRefusal::WrongSize { name, got, want } => format!("{name} is {got} bytes, and the file should be {want}: it is not the file Google publishes for build {}.", build.build),
                    ZipRefusal::WrongSha256 { name } => format!("{name} is not the file Google publishes for build {} (its SHA-256 differs). Download it again from Google's page.", build.build),
                });
                s.footer =
                    vec!["The file stays on this computer. Nothing of it is sent anywhere.".into()];
            }
            Step::Answers => {
                s.state = "answers";
                s.action = "answers";
                s.title = "About you and the phone".into();
                s.body = vec![
                    "A user name, a PIN or password to unlock the phone (digits only gives a PIN pad), the phone's name, the time zone and, if you like, a Wi-Fi network.".into(),
                    "This version of the installer checks your answers and keeps them nowhere: it has no way yet to put them on the phone.".into(),
                ];
            }
            Step::DryRun => {
                s.state = "dry-run";
                s.action = "dry-run";
                s.title = "The dry run".into();
                s.body = vec![
                    "The installer now asks the phone every question an install starts with, and lists what an install would write.".into(),
                    "A dry run sends no write: it reads.".into(),
                ];
                s.footer = vec!["Keep the phone connected, in Fastboot Mode.".into()];
            }
            Step::Done(plan) => {
                s.state = "done";
                s.title = "The dry run passed. Nothing was written".into();
                let a = &plan.answers;
                s.body = vec![
                    format!(
                        "The {name} ({}) is unlocked and runs from slot {}; bootloader {}{}.",
                        plan.product,
                        plan.current_slot,
                        plan.bootloader,
                        plan.baseband
                            .as_ref()
                            .map(|b| format!(", baseband {b}"))
                            .unwrap_or_default()
                    ),
                    format!(
                        "Google's factory image for build {} is the right file ({}).",
                        plan.factory_zip.build, plan.factory_zip.name
                    ),
                    format!(
                        "User {} ({}), phone name {}, time zone {}, Wi-Fi {}.",
                        a.username,
                        if a.pin { "PIN" } else { "password" },
                        a.hostname,
                        a.timezone,
                        if a.wifi { "set" } else { "not set" }
                    ),
                    if plan.images_walked {
                        "Every image of the release was checked against its signed checksum and walked through the dry run. An install would send these writes, and only these:".into()
                    } else {
                        "The release's images were not fetched for this dry run, so the list is the signed plan's. An install would send these writes, and only these:".into()
                    },
                ];
                s.plan = plan.would.clone();
                s.footer = vec![
                    "The dry run only asked the phone questions. It has written nothing to it.".into(),
                    "This version of the installer ends here: it has no step that writes to a phone.".into(),
                ];
            }
            Step::Stopped(stop) => {
                s.state = "stopped";
                s.stop = Some(stop.id());
                s.title = stop.title.clone();
                s.body = stop.message.clone();
                s.footer = stop.footer();
            }
        }
        s
    }
}

enum Judged {
    Stopped,
    Locked,
    Ready {
        build: Box<Build>,
        same_pair: Vec<String>,
    },
}
