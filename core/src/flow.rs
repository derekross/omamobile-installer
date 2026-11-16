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
//!   -> SlotB          answer_slot_b: what slot B holds, asked of a phone
//!                     that runs from slot B in a front end that can ask
//!                     ([`FlowOptions::asks_what_slot_b_holds`])
//!   -> Unlock         read_phone again, after the owner unlocked
//!   -> GoogleTerms    accept_google_terms / decline_google_terms
//!                     (left out when the owner has the file, or the front
//!                     end says it can't fetch from Google)
//!   -> FactoryImage   factory_download, the fetch, factory_download_done; or
//!                     factory_zip, then the bytes, then factory_zip_done
//!   -> Answers        answers
//!   -> DryRun         dry_run, then dry_run_done
//!   -> Done           a dry run ends here ([`FlowOptions::write`] not set)
//!
//!   -> Confirm        confirm_install: the owner types the phone's codename
//!   -> Write          begin_write, then write_done or write_failed
//!   -> Installed
//! ```
//!
//! A flow is a dry run unless it was begun with [`FlowOptions::write`], and a
//! dry run has no step that writes to a phone. One that writes goes through
//! the same dry run first, with every image of the release walked through
//! it, then lists exactly what it will write and what is erased, and waits
//! for the phone's codename to be typed. What is written, and in which order,
//! is what Omarchy Mobile's `tools/install-phone` writes: slot B's boot
//! images, `userdata`, then `set_active:b` and `reboot`.
//!
//! [`Flow::begin_write`] is the one place a session that can write is
//! opened, and it checks every condition again there: the release and its
//! build index verified, this phone past every stop in this run and looked
//! at once more, unlocked, the same phone, the codename typed in this run,
//! not a dry run, and the maintainer's bootloader-slot override not used.
//!
//! Until then, what the flow sends to a phone is `getvar:` and, to a locked
//! phone it is about to show the unlock steps for, the one question
//! `flashing get_unlock_ability` ([`unlock_ability`]): both read, and nothing
//! else is sent. They go through the transport a front end lends it:
//! [`Flow::read_phone`] asks the phone what it is, and [`Flow::dry_run`]
//! opens a [`Session`] that holds every write back. It never sends
//! `flashing unlock`: the unlock step shows that command for the owner to
//! run. It never switches slots to look at one, and the only slot command
//! it has is the install's own `set_active:b`, after the last write.
//!
//! What the flow has fetched from the internet is one file: Google's factory
//! image, from the address in the release's signed build index, and only
//! after the owner accepted Google's terms on the step that shows them
//! ([`Flow::accept_google_terms`], the maintainer's decision of 2026-11-12).
//! The front end does the fetching and [`crate::download`] decides every
//! answer. An owner who doesn't accept, or whose download can't be made,
//! saves the file from Google's page and hands it in.
//!
//! **A release that installs on the running slot** (manifest format 2,
//! Omarchy Mobile's ACTIVE-SLOT-PLAN) has none of the slot B steps: no
//! question of what slot B holds, no measurement of the other bootloader
//! slot, no bootloader-slot check or stop, and no `set_active` at all. Its
//! one slot rule is that the phone's `current-slot` is one the release
//! accepts ([`StopKind::CurrentSlotNotAccepted`]) and stays the same from
//! the first look to the write ([`StopKind::CurrentSlotChanged`]); the
//! session reads it once more before the first write and after the last.
//! It writes that slot's boot images and `userdata`, then restarts the
//! phone: Android on the phone is replaced, and the way back is Google's
//! factory image ([`GO_BACK_TO_ANDROID`]).
//!
//! **Repair** ([`FlowOptions::repair`], docs/DESIGN.md §11) is a mode of
//! this flow for such a release, and only in a flow that writes: the way
//! back from a bad kernel once Android is gone. It writes the running slot's
//! boot images again and nothing else: no `userdata`, so the account and
//! everything stored are kept; no `set_active`; never the other slot. Its
//! steps are the warning, the look (model, pinned phone, `current-slot`
//! accepted, unlocked; no build index and no factory image, since no
//! firmware is written), the dry run, the confirmation, the write and
//! [`Step::Repaired`]. The session is the one [`Flow::begin_write`] opens,
//! with the token it makes, like every write.
//!
//! Every end that isn't `Done` is a [`Stop`]: a typed reason, the words for
//! the person, and a closing line that says what is true of the phone at
//! that moment and whether starting again can get past it. No stop ends in a
//! fixed reassurance.
use crate::build_index::{Build, BuildIndex, Lookup};
use crate::bundle::{verify_release, ReleaseFiles, VerifiedRelease};
use crate::download::{Ending, FactoryDownload, FetchFailure, Mismatch, Unavailable};
use crate::fastboot::{
    probe, unlock_ability, PhoneId, PhoneOptions, PhoneReport, Session, Transport, UnlockAbility,
    WriteReach,
};
use crate::firstboot::{root_answers_on, RootAnswers};
use crate::hash::StreamHasher;
use crate::manifest::Slot;
use crate::signature::TrustedKeys;
use crate::slot_probe::{
    slot_b_reads_as_started, Active, MeasuredSlot, ProbeEnd, ProbeRecord, ProbeTrouble, SlotProbe,
};
use crate::{Error, Result};
use serde::Serialize;

pub(crate) const ISSUES: &str = "https://github.com/derekross/omarchy-mobile/issues";
const DOWNLOAD: &str = "https://omamobile.org/download";
/// What Google's factory images page asks a person to accept before it hands
/// out a file. Google's words, shown as Google's on the terms step.
pub const GOOGLE_TERMS: [&str; 2] = [
    "Downloading of the system image and use of the device software is subject to the Google Terms of Service",
    "By continuing, you agree to the Google Terms of Service and Privacy Policy",
];
/// The rest of what Google's page says there, the quoted part in Google's
/// words.
pub const GOOGLE_TERMS_THIRD_PARTY: &str = "Google's page also says that the download and use \"may also be subject to certain third-party terms of service, which can be found in Settings > About phone > Legal information\".";
pub const GOOGLE_TERMS_OF_SERVICE: &str = "https://policies.google.com/terms";
pub const GOOGLE_PRIVACY_POLICY: &str = "https://policies.google.com/privacy";
/// Said on the factory-image step of a front end that cannot fetch from
/// Google's server ([`FlowOptions::cannot_fetch_from_google`]), in place of
/// the terms step: why the owner fetches the file by hand there. Measured on
/// 2026-11-12: `dl.google.com` sends no `Access-Control-Allow-Origin`
/// header, so a browser lets no page read the file.
pub const CANNOT_FETCH_FROM_GOOGLE: &str = "A web page is not allowed to read files from Google's server, so this page cannot download the image for you. The installer's command-line tool can download it for you. Here you download the image from Google's page yourself, where Google asks you to accept its terms, and the installer checks the file you saved.";
/// The command the owner runs to unlock the bootloader. Shown, never sent:
/// the one `flashing` command this crate sends is the question
/// [`crate::fastboot::GET_UNLOCK_ABILITY`], which only reads.
pub const UNLOCK_COMMAND: &str = "fastboot flashing unlock";
/// Said on the unlock screen, before its steps, by a flow that ends at the
/// dry run ([`Flow::ends_at_dry_run`]): the steps there erase the phone, and
/// such a flow installs nothing afterwards.
pub const NO_INSTALL_YET: &str = "This version of the installer cannot install Omarchy Mobile yet. It only checks the phone and lists what an install would write. Unlocking the bootloader erases the phone, and there is no reason to unlock it for this version: do not unlock the phone for it.";
/// The phones whose slot handling this write path was written for: what
/// `set_active:b` costs and what a failed start falls back to differ from
/// phone to phone, and only these were read up on. A release for another
/// phone stops a flow that writes ([`StopKind::WritesNotForDevice`]).
pub const WRITE_DEVICES: [&str; 1] = ["cheetah"];
/// What a front end says where it offers the switch that makes a flow write
/// (the command line's `--write`): exactly what is written and erased.
pub const WRITE_HELP: &str = "Writes to the phone. A release made to install on the slot the phone runs from (manifest format 2) writes that slot's boot partitions (the release's boot images, each to its partition with that slot's suffix) and userdata, then restarts the phone; it makes no slot active and never writes the other slot. Android on the phone is replaced: the way back is Google's factory image, with Google's Android Flash Tool. An older release (format 1) writes slot B's boot partitions (the _b suffix) and userdata, then makes slot B the active slot and restarts the phone; Android on slot A stays, and needs a factory reset if it is ever started again. Either way userdata is ERASED: everything Android stored on the phone is lost. The bootloader, radio, persist, efs, modem, devinfo and vbmeta are never written. The run first does everything a dry run does, then lists each partition with its size and waits for you to type the phone's codename; nothing is written before that. Your answers (user name, PIN or password, phone name, time zone, Wi-Fi) go into the place the release's system image keeps for them, so the phone sets up your account at its first start; a release whose system image keeps no such place is not installed.";
/// What the phone does with the answers, said where the owner confirms the
/// writes. Only for a release whose system image keeps a place for them;
/// a flow that writes stops for any other ([`StopKind::NoPlaceForAnswers`]).
/// What a run that hands the flow this computer's SSH key adds to
/// [`ANSWERS_AT_FIRST_START`].
pub const SSH_KEY_WITH_THE_ANSWERS: &str = "This computer's SSH public key goes with your answers: at its first start the phone sets it up for root and for your account, so that this computer can reach the phone over the USB cable for the steps after the write. Only the public half goes; the private key stays on this computer.";
pub const ANSWERS_AT_FIRST_START: &str = "At its first start the phone sets up your account from your answers: your user name, your PIN or password, the phone's name and time zone, and the Wi-Fi network if you gave one (it connects once Google's files for Wi-Fi are on the phone). The installer puts them into the place the release's system image keeps for them as it writes userdata. Your PIN or password goes there only as a hash, made on this computer; the Wi-Fi password goes as you typed it, because the phone needs it to connect. Neither is sent anywhere but to the phone.";
/// Why this installer does not install a release made before the system
/// image kept a place for the answers.
pub const NO_PLACE_FOR_ANSWERS: &str = "This release's system image keeps no place for your answers, so a phone installed from it by this installer would start with no account and nothing to unlock it with. This installer does not install such a release.";
/// Said on the unlock screen, before its steps, by a flow that may write
/// What the confirmation says instead of [`ANSWERS_AT_FIRST_START`] for a
/// release whose root image offers the install link: the answers go over
/// the link, and the image is written as the release signed it.
const LINK_ANSWERS: &str = "Your answers are not written into the images: what is flashed is the release as it was signed. After the restart the phone offers the install link on this cable, and the installer gives it your answers there, saves its radio backup on this computer and sends it the factory image you have from Google.";
/// ([`FlowOptions::write`]): the steps there erase the phone, and this
/// version's writes have run against the tests' pretend phone only.
pub const NO_USABLE_PHONE_YET: &str = "Unlocking the bootloader erases the phone. This version of the installer has written only to a pretend phone in its tests, not yet to a real one: unlock the phone for this version only if it is one you can do without.";
/// The way back to Android once a release on the running slot (format 2)
/// is installed: said on the warning, the confirmation, the last screen and
/// the stops after a write, in the same words.
pub const GO_BACK_TO_ANDROID: &str = "The way back to Android is Google's factory image for the phone, written with Google's Android Flash Tool (in a browser, at https://flash.android.com) or with the factory image's own flash-all script, while the bootloader is still unlocked. It puts Android back as the phone was sold and erases the phone again. The install guide's \"Going back to Android\" has the steps. Don't lock the bootloader unless both of the phone's slots hold Google's images.";

/// What a release on the running slot (format 2) does to Android, said on
/// the warning and the confirmation.
pub const ANDROID_REPLACED: &str = "Android on this phone is replaced: Omarchy Mobile is written over the Android the phone starts now, and no copy of Android is kept on the phone to go back to.";

/// Shown on every screen once the maintainer's override has skipped the
/// bootloader-slot check, so both front ends say it in the same words.
pub const SLOT_CHECK_SKIPPED: &str = "The bootloader-slot check was skipped by a maintainer override. This phone tells the version of only one of its two bootloader copies, and the installer went on without knowing the other. This run is not evidence that an install would be allowed.";

/// Shown on every screen once the flow has gone past a phone that reports
/// one bootloader slot's version on the owner's statement about the other
/// ([`FlowOptions::bootloader_slots_match`]), the confirmation among them.
pub const SLOTS_ON_THE_OWNERS_WORD: &str = "This phone tells the version of only one of its two bootloader copies. The installer has not read the other copy's version and cannot: it goes on because this run was started with the statement that someone read it from the phone and that it is the same version. If that is not so, the phone may not start from slot B.";

/// Said on the dry run's last screen and on the confirmation of a
/// reinstall on a phone that tells one bootloader copy's version only: the
/// copy it tells is slot B's, the one an install from slot B needs
/// (decided by the maintainer on 2026-11-13).
pub const SLOT_B_BOOTLOADER_IN_USE: &str =
    "slot B's bootloader, which the phone already runs; the install switches no slot";

/// The phones whose other bootloader slot the installer may measure, if
/// the release's descriptor allows it too (`BOOTLOADER_SLOT_PROBE=yes`):
/// Tensor Pixels, whose bootloader keeps the slot flags in its own storage
/// and changes them on `set_active`. A Qualcomm A/B phone keeps them in the
/// GPT's attributes, a write Omarchy Mobile allows only by hand, so no such
/// phone is on this list whatever its descriptor says (docs/DESIGN.md §6).
pub const SLOT_PROBE_DEVICES: [&str; 1] = ["cheetah"];

/// Said on the dry run's last screen and on the confirmation when the
/// other bootloader slot was measured in this run.
pub const OTHER_SLOT_MEASURED: &str = "the installer measured slot B's bootloader in this run: it made slot B active, restarted the phone into its bootloader, read the version there, made slot A active again and restarted back";

/// The owner's answer to the question whether to measure the other
/// bootloader slot ([`Step::SlotProbe`]), or to put slot A back after a run
/// that left slot B active ([`Step::SlotProbePutBack`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAnswer {
    /// Measure it: two restarts into the bootloader.
    Measure,
    /// Make slot A active again, and restart into its bootloader.
    PutBack,
    /// Don't: the install stops here.
    Stop,
}

impl ProbeAnswer {
    pub const ALL: [ProbeAnswer; 3] = [
        ProbeAnswer::Measure,
        ProbeAnswer::PutBack,
        ProbeAnswer::Stop,
    ];
    pub fn answer(self) -> &'static str {
        match self {
            ProbeAnswer::Measure => "measure",
            ProbeAnswer::PutBack => "put-back",
            ProbeAnswer::Stop => "stop",
        }
    }
    pub fn from_answer(answer: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.answer() == answer)
    }
}

/// The owner's answer to the question of what slot B holds
/// ([`Step::SlotB`]). On a screen each is a [`ScreenChoice`] whose
/// `answer` is [`SlotBHolds::answer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotBHolds {
    /// Omarchy Mobile: this is a reinstall.
    OmarchyMobile,
    /// Android, moved there by a system update.
    Android,
    /// The owner does not know.
    NotSure,
}

impl SlotBHolds {
    pub const ALL: [SlotBHolds; 3] = [
        SlotBHolds::OmarchyMobile,
        SlotBHolds::Android,
        SlotBHolds::NotSure,
    ];
    /// The answer's name on a screen and in a front end's message.
    pub fn answer(self) -> &'static str {
        match self {
            SlotBHolds::OmarchyMobile => "omarchy-mobile",
            SlotBHolds::Android => "android",
            SlotBHolds::NotSure => "not-sure",
        }
    }
    /// The answer a front end handed in by its name, if it is one.
    pub fn from_answer(answer: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.answer() == answer)
    }
    fn label(self) -> &'static str {
        match self {
            SlotBHolds::OmarchyMobile => "Omarchy Mobile is on slot B: install it again",
            SlotBHolds::Android => "Android is on slot B, after a system update",
            SlotBHolds::NotSure => "I am not sure",
        }
    }
}

/// What a session must hold to write to a phone, and proof of where it was
/// made: [`Flow::begin_write`], after every condition of a write was checked
/// there (the run was begun to write and is not a dry run, the maintainer's
/// override was not used, the release and its build index verified, the
/// phone's codename typed in this run, the release's images checked, and
/// the phone, asked once more, the pinned one, past every stop, unlocked
/// and on the build the factory image was chosen for).
///
/// Its one field is private to this module, so no other module of this
/// crate and no other crate can make one, and it can be neither copied nor
/// cloned: the session that takes it keeps it. A function can ask for one,
/// which is all another crate can do with the name:
///
/// ```
/// use omamobile_installer_core::flow::WriteToken;
/// fn a_session_that_writes_needs(_token: WriteToken) {}
/// ```
///
/// Making one is a compile error, here by its constructor:
///
/// ```compile_fail
/// use omamobile_installer_core::flow::WriteToken;
/// fn a_session_that_writes_needs(_token: WriteToken) {}
/// a_session_that_writes_needs(WriteToken(()));
/// ```
///
/// and it has no other: no `Default`, no `Clone`, no function that returns
/// one.
///
/// ```compile_fail
/// use omamobile_installer_core::flow::WriteToken;
/// fn a_session_that_writes_needs(_token: WriteToken) {}
/// a_session_that_writes_needs(WriteToken::default());
/// ```
///
/// The function that opens a writing session with it is not public either,
/// so a front end has one way to such a session: the flow's steps, to
/// `begin_write`.
pub struct WriteToken(());

impl core::fmt::Debug for WriteToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("WriteToken")
    }
}

/// What the person running the install said up front.
#[derive(Debug, Clone, Default)]
pub struct FlowOptions {
    /// Slot B already holds Omarchy Mobile and this installs it again.
    /// Without it a phone running from slot B stops the install, or, in a
    /// front end that can ask ([`Self::asks_what_slot_b_holds`]), is asked
    /// what slot B holds.
    ///
    /// On a phone that runs from slot B it also passes the bootloader-slot
    /// check of a phone that tells one copy's version only: the copy it
    /// tells is slot B's, which the phone already runs, and an install from
    /// slot B switches no slot ([`SLOT_B_BOOTLOADER_IN_USE`]; decided by the
    /// maintainer on 2026-11-13). On a phone that runs from slot A it
    /// changes nothing.
    pub reinstall: bool,
    /// The front end can show the question of what slot B holds
    /// ([`Step::SlotB`], its `choices`) and hand the answer in
    /// ([`Flow::answer_slot_b`]): the desktop app, through the tool's
    /// `json` mode. Without it a phone that runs from slot B, in a run not
    /// said up front to be a reinstall, stops at `runs-from-slot-b`, as it
    /// always did.
    pub asks_what_slot_b_holds: bool,
    /// The maintainer's override, for a dry run only: go on past
    /// [`StopKind::BootloaderSlotsUnknown`], and say so on every screen from
    /// then on ([`SLOT_CHECK_SKIPPED`]). It passes no other stop: two
    /// versions that are known to differ still stop.
    ///
    /// It can't reach a write. Together with [`Self::write`] it stops the
    /// flow in [`Flow::begin`] ([`StopKind::OverrideInInstall`]); the
    /// session a dry run opens is refused with
    /// [`Error::OverrideOutsideDryRun`] unless it holds every write back;
    /// and [`Flow::begin_write`] refuses a run in which a check was skipped.
    pub maintainer_skip_bootloader_slot_check: bool,
    /// The owner's statement about a phone that reports one bootloader
    /// slot's version only: the other slot's version was read from the
    /// phone, and it is this one. It is the rule of Omarchy Mobile's
    /// `tools/install-phone` (`--bootloader-slots-match VERSION`), which
    /// `tools/install-guided` applies before the unlock steps too:
    ///
    /// - both slots reported: they are compared, and this is not used;
    /// - one reported, and this equals it: the flow goes on, and says on
    ///   every screen whose word it rests on
    ///   ([`SLOTS_ON_THE_OWNERS_WORD`]);
    /// - one reported, and this differs from it: a stop that names both
    ///   ([`StopKind::BootloaderSlotsStatedOther`]);
    /// - one reported and nothing stated: the stop there always was
    ///   ([`StopKind::BootloaderSlotsUnknown`]).
    ///
    /// The installer has no way to check the statement, and never switches
    /// slots to try. It is compared with the phone at every look: the
    /// first, the dry run's and the write's, and by each session at
    /// `connect` ([`PhoneOptions::bootloader_slots_confirmed`]). Only a
    /// front end whose owner typed it may set it: the command line's
    /// `--bootloader-slots-match`. The web page has no way to.
    pub bootloader_slots_match: Option<String>,
    /// The owner already has Google's factory image, saved from Google's
    /// page, and hands that file in. The installer then downloads nothing,
    /// so the step that shows Google's terms is left out: they were Google's
    /// to ask on its page.
    pub owner_has_factory_zip: bool,
    /// The front end can't fetch the factory image from Google's server at
    /// all, and says so here, once, at the start: a web page, which a
    /// browser does not let read a file from `dl.google.com` (the server
    /// sends no `Access-Control-Allow-Origin`; measured on 2026-11-12). The
    /// flow then leaves the terms step out, as it does for an owner who has
    /// the file: there are no terms to ask for a download that can't be
    /// made. It goes straight to the owner's own download and says why
    /// ([`CANNOT_FETCH_FROM_GOOGLE`]), and gives out no download.
    pub cannot_fetch_from_google: bool,
    /// This run may write to the phone. Without it the flow is a dry run and
    /// ends at [`Step::Done`]. With it, the dry run is followed by the list
    /// of what will be written, the typed confirmation and the writes. It
    /// can't be combined with the maintainer's override, and it is refused
    /// for a phone the write path was not written for ([`WRITE_DEVICES`]).
    pub write: bool,
    /// This computer's SSH public key, one OpenSSH line, for slot B's
    /// initramfs: the image that carries it is written with the key
    /// appended, as `tools/install-phone` appends it
    /// ([`crate::ramdisk_key`]), so that the initramfs's rescue SSH lets
    /// this computer in. The confirmation says so. Set by the front end
    /// that writes (the tool's `json --write`); the key is checked when a
    /// session opens.
    pub owner_ssh_key: Option<String>,
    /// The front end can carry out the other-slot measurement's restarts:
    /// lend a transport for each stretch, see the phone leave the bus and
    /// come back by its serial number, and draw the question (the tool's
    /// `install --write` and `json --write`, and the page's write preview).
    /// Together with [`Self::write`], on a first install from slot A of a
    /// phone on [`SLOT_PROBE_DEVICES`] whose release's descriptor says
    /// `BOOTLOADER_SLOT_PROBE=yes`, the flow offers the measurement instead
    /// of stopping at `bootloader-slots-unknown` ([`Step::SlotProbe`]). A
    /// dry run never measures: a measurement writes the bootloader's slot
    /// flags, and a dry run only reads.
    pub can_probe_slots: bool,
    /// What an earlier run's measurement left to remember
    /// ([`ProbeRecord::to_text`]), when the front end kept it: the phone it
    /// measured may still have slot B active. On that phone, running from
    /// the other slot, the flow first offers to put slot A back
    /// ([`Step::SlotProbePutBack`]). Something that is not such a record is
    /// ignored.
    pub slot_probe_record: Option<String>,
    /// The front end goes on after the write with the steps the release's
    /// own tools carry out (the tool's `json --write`, for a release that
    /// ships them): the last screen says that they come next rather than
    /// that nothing does. The flow itself still ends at the write.
    pub after_write_steps: bool,
    /// Repair the phone (docs/DESIGN.md §11, "Repair"): write the running
    /// slot's boot images from this release again, and nothing else. Only
    /// with [`Self::write`] ([`StopKind::RepairNeedsWrite`] otherwise) and
    /// only for a release that installs on the running slot, manifest
    /// format 2 ([`StopKind::RepairNotForRelease`] otherwise), both stopped
    /// in [`Flow::begin`]. `userdata` is never written, so the phone keeps
    /// its account and everything on it; no slot is made active, and the
    /// other slot is never written. There is no build index lookup, no
    /// factory image and no answers: no firmware is written. A locked phone
    /// stops ([`StopKind::RepairLocked`]): unlocking would erase what a
    /// Repair keeps. The SSH key and the steps after the write are not for
    /// it, and are ignored.
    pub repair: bool,
}

/// What a front end says where it offers Repair (the command line's
/// `repair`): exactly what is written and what is kept.
pub const REPAIR_HELP: &str = "Repairs a phone that runs Omarchy Mobile from a release that installs on the slot the phone runs from (manifest format 2), when a kernel update left it unable to start: it writes the release's boot images again to the slot the phone runs from (each to its partition with that slot's suffix), then restarts the phone. Nothing else: userdata is NOT written, so your account and everything on the phone are kept; no slot is made active; the other slot, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are never written. It needs no factory image and no answers. The phone has to be unlocked and in Fastboot Mode on the slot the release accepts. The run first checks the phone and every image against the release's signed checksums, then lists each partition and waits for you to type the phone's codename; nothing is written before that.";

/// Said on a Repair's warning, its confirmation and its last screen: what
/// it keeps, and the ways back if it does not help.
pub const REPAIR_KEEPS: &str = "A repair does not write userdata: your account, your files and everything else stored on the phone are kept. No slot is made active, and the other slot, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written.";

/// The way on when a Repair does not get the phone to start.
pub const REPAIR_WAY_BACK: &str = "If the phone still does not start after a repair, a full install of this release with the installer writes everything again, userdata included, which erases what is stored on the phone. Or put Android back with Google's factory image, which erases the phone too.";

/// The owner accepted Google's terms on the terms step: for which build,
/// and when. Kept in the flow and nowhere else; it is never sent anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermsAccepted {
    pub build: String,
    /// Seconds since 1970-01-01 00:00 UTC, by the front end's clock.
    pub at: u64,
}

impl TermsAccepted {
    /// `2026-11-12 at 14:03 UTC`.
    pub fn when(&self) -> String {
        let (days, secs) = (self.at / 86400, self.at % 86400);
        // Days since 1970-01-01 to a date of the Gregorian calendar
        let z = days + 719_468;
        let (era, doe) = (z / 146_097, z % 146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let (d, m) = (
            doy - (153 * mp + 2) / 5 + 1,
            if mp < 10 { mp + 3 } else { mp - 9 },
        );
        let y = yoe + era * 400 + u64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02} at {:02}:{:02} UTC",
            secs / 3600,
            secs % 3600 / 60
        )
    }
}

/// How the factory image comes to the installer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// The owner accepted Google's terms: the installer downloads the file
    /// from Google's server. A file the owner hands in is taken as well.
    Download,
    /// The owner saves the file from Google's page and hands it in.
    OwnerFile(OwnerFileWhy),
}

/// Why the owner fetches the file by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerFileWhy {
    /// They said up front that they have it ([`FlowOptions::owner_has_factory_zip`]).
    SaidSo,
    /// The front end can't fetch from Google's server
    /// ([`FlowOptions::cannot_fetch_from_google`]).
    CannotFetch,
    /// They did not accept Google's terms in the installer.
    TermsDeclined,
    /// They accepted, and the installer could not download the file.
    Unavailable(Unavailable),
}

/// A download that ended early. The step stays open, and it can be tried
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadTrouble {
    /// Fewer bytes arrived than the file has. `detail`: what the front end's
    /// HTTP client said, if anything.
    Interrupted {
        got: u64,
        want: u64,
        detail: Option<String>,
    },
    /// The front end could not keep the bytes.
    Storage(String),
}

/// What the front end does with the bytes of a download that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadEnd {
    /// The file is complete and is the one the signed index describes.
    Verified,
    /// Not complete, or nothing of this try was taken. What is on this
    /// computer of the file may be kept, to go on from.
    Kept,
    /// What is on this computer can't be the file: it is complete with
    /// another SHA-256, or more arrived than the file has. Delete every byte
    /// that was kept of it. Never the answer to a size the server only
    /// announced: that says nothing about bytes kept earlier.
    Discard,
}

/// Where the install is, and what the front end shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The one warning. Nothing is asked of a phone before it is accepted.
    Warning,
    /// A phone in Fastboot Mode is needed. `not_found`: the front end looked
    /// and none answered.
    Phone { not_found: bool },
    /// The phone runs from slot B, and the run was not said to be a
    /// reinstall: the owner is asked what slot B holds, Omarchy Mobile or
    /// Android after a system update, since fastboot can't tell the two
    /// apart. Only in a front end that can ask
    /// ([`FlowOptions::asks_what_slot_b_holds`]).
    SlotB,
    /// The phone tells one bootloader copy's version, runs from slot A, and
    /// is otherwise ready: the owner is asked whether the installer may
    /// measure the other copy ([`ProbeAnswer`]). Only in a flow that writes,
    /// in a front end that can ([`FlowOptions::can_probe_slots`]), for a
    /// release and phone that allow it.
    SlotProbe,
    /// An earlier run's measurement on this phone ended with slot B
    /// possibly active, and the phone runs from it: the owner is asked
    /// whether to put slot A back first.
    SlotProbePutBack,
    /// The measurement (or the putting back) is under way: the front end
    /// holds it, and hands it back with [`Flow::slot_probe_done`].
    SlotProbeRunning { put_back: bool },
    /// The phone is the right one and can be installed, but its bootloader
    /// is locked. The steps are for the owner; the flow sends none of them.
    /// `oem_unlocking`: what the phone said when asked whether it would take
    /// the unlock command.
    Unlock { oem_unlocking: UnlockAbility },
    /// What Google's download page asks a person to accept, before anything
    /// is downloaded. Nothing happens until the owner answers.
    GoogleTerms {
        build: Build,
        /// Other builds with the same bootloader and baseband.
        same_pair: Vec<String>,
    },
    /// Google's factory image for this build: downloaded by the installer,
    /// or saved by the owner and handed over (`source`). `refused`: why the
    /// last file handed over wasn't taken. `trouble`: why the last download
    /// ended early.
    FactoryImage {
        build: Build,
        /// Other builds with the same bootloader and baseband.
        same_pair: Vec<String>,
        source: ImageSource,
        refused: Option<ZipRefusal>,
        trouble: Option<DownloadTrouble>,
    },
    /// The owner's answers for the first start.
    Answers,
    /// Everything is in hand for the dry run.
    DryRun,
    /// The dry run passed. A dry run ends here.
    Done(DryRunPlan),
    /// A flow that writes, after its dry run: exactly what will be written
    /// and what is erased. Nothing happens until the owner types the phone's
    /// codename.
    Confirm(DryRunPlan),
    /// The codename was typed. The next thing is the writes themselves.
    Write(DryRunPlan),
    /// Every write was confirmed by the phone, slot B is active and the
    /// phone was told to restart. The flow ends here.
    Installed(DryRunPlan),
    /// A Repair ran through: the running slot's boot images were written
    /// and confirmed, `userdata` was not touched, and the phone was told to
    /// restart. The flow ends here.
    Repaired(DryRunPlan),
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
    /// A release that installs on the running slot (format 2), on a phone
    /// whose current slot is not one the release accepts.
    CurrentSlotNotAccepted { slot: String, accepted: Vec<String> },
    /// A release that installs on the running slot: the phone's current
    /// slot is not the one it ran from when the installer first looked.
    CurrentSlotChanged { was: String, now: String },
    /// The bootloader reports only the current slot's version. `probe_declared`:
    /// the descriptor allows reading the other by switching slots, which
    /// this version does not do.
    BootloaderSlotsUnknown { probe_declared: bool },
    /// The two slots' bootloaders differ.
    BootloaderSlotsDiffer { a: String, b: String },
    /// The owner did not want the other bootloader slot measured.
    SlotProbeDeclined,
    /// The measurement of the other slot did not read it, or the putting
    /// back of slot A did not finish: why, and which slot is active as far
    /// as the phone confirmed.
    SlotProbeStopped { why: ProbeTrouble, active: Active },
    /// The bootloader reports only the current slot's version, and the
    /// version the owner stated for the other slot
    /// ([`FlowOptions::bootloader_slots_match`]) is not that one.
    BootloaderSlotsStatedOther { stated: String, phone: String },
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
    /// What the installer downloaded from Google's server is not the file
    /// the release's signed list of Google's builds describes.
    FactoryDownloadMismatch { build: String, why: Mismatch },
    /// At the dry run the phone's bootloader was locked again.
    LockedAtDryRun,
    /// At the dry run the phone reported another bootloader or baseband than
    /// the factory image was chosen for.
    BuildChanged,
    /// The dry run's own checks refused the phone.
    DryRunRefused(Error),
    /// A flow that writes was begun with the maintainer's bootloader-slot
    /// override, which is for dry runs only.
    OverrideInInstall,
    /// A flow that writes, for a phone this write path was not written for.
    WritesNotForDevice { codename: String },
    /// A flow that writes, for a release whose system image keeps no place
    /// for the owner's answers (one made before the placeholders).
    NoPlaceForAnswers,
    /// A flow that writes needs every image of the release on this
    /// computer, checked, before it asks for the confirmation.
    ImagesNotChecked,
    /// What was typed at the confirmation was not the phone's codename.
    NotConfirmed,
    /// After the confirmation the phone's bootloader was locked again.
    LockedAtWrite,
    /// The checks a write starts with refused the phone, or the link was
    /// lost, before anything was written.
    WriteRefused(Error),
    /// The writes began and did not run through.
    WriteInterrupted(Error),
    /// Repair was asked for in a flow that does not write. Stopped in
    /// `begin`, before the release is read.
    RepairNeedsWrite,
    /// Repair was asked for with a release that does not install on the
    /// running slot (format 1). Stopped in `begin`.
    RepairNotForRelease,
    /// Repair, and the phone's bootloader is locked: unlocking erases the
    /// phone, and a Repair is there to keep what is on it.
    RepairLocked,
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
    /// The phone was asked what it is and its other bootloader slot was
    /// measured: its slot flags were changed and slot A made active again,
    /// and nothing else was written.
    SlotsMeasured { unlock_shown: bool },
    /// A measurement of the other slot (or the putting back of slot A)
    /// ended early. `active`: which slot is active as far as is known.
    SlotProbeStopped { active: Active, unlock_shown: bool },
    /// The writes began and did not run through. `reach`: how far they got,
    /// from what the phone confirmed. `on_slot_b`: the phone ran from slot B
    /// when they began (a reinstall), so slot B is the one it starts from.
    WriteStopped { reach: WriteReach, on_slot_b: bool },
    /// A release on the running slot (format 2): the writes to `slot`, the
    /// slot the phone runs from, began and did not run through.
    WriteStoppedRunning { reach: WriteReach, slot: Slot },
    /// A Repair: the writes to the boot images of `slot`, the slot the
    /// phone runs from, began and did not run through. `userdata` was not
    /// touched.
    RepairStopped { reach: WriteReach, slot: Slot },
}

/// Whether starting the installer again can get past a stop. On a screen
/// ([`Screen::retry`]) it reads `whenever`, `after-fix` or `stops-again`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
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
            PhoneFact::SlotsMeasured { unlock_shown } => {
                let mut v = vec!["The installer asked the phone what it is and measured its second bootloader copy: that changed the bootloader's slot flags, and slot A was made the active slot again and seen to run. It has written nothing else to the phone.".to_string()];
                if unlock_shown {
                    v.push(erased.to_string());
                }
                v
            }
            PhoneFact::SlotProbeStopped {
                active,
                unlock_shown,
            } => {
                let mut v = vec![match active {
                    Active::First => "The installer changed only the bootloader's slot flags, and slot A is the active slot again: the phone runs its bootloader. Nothing was written to the phone's partitions.",
                    Active::FirstSet => "The installer changed only the bootloader's slot flags. Slot A is set as the active slot again, but the phone may still be running slot B's bootloader until it restarts. Nothing was written to the phone's partitions.",
                    Active::NotKnown => "The installer changed only the bootloader's slot flags, and slot B may still be the active slot: the phone was not reachable to put slot A back. Nothing was written to the phone's partitions.",
                }
                .to_string()];
                if unlock_shown {
                    v.push(erased.to_string());
                }
                v
            }
            // Says what a rerun does itself, for each place a write can stop
            PhoneFact::WriteStopped { reach, on_slot_b } => {
                return write_stopped_footer(reach, on_slot_b)
            }
            PhoneFact::WriteStoppedRunning { reach, slot } => {
                return write_stopped_running_footer(reach, slot)
            }
            PhoneFact::RepairStopped { reach, slot } => return repair_stopped_footer(reach, slot),
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
            StopKind::CurrentSlotNotAccepted { .. } => "current-slot-not-accepted",
            StopKind::CurrentSlotChanged { .. } => "current-slot-changed",
            StopKind::BootloaderSlotsUnknown { .. } => "bootloader-slots-unknown",
            StopKind::BootloaderSlotsDiffer { .. } => "bootloader-slots-differ",
            StopKind::SlotProbeDeclined => "slot-probe-declined",
            StopKind::SlotProbeStopped { .. } => "slot-probe-stopped",
            StopKind::BootloaderSlotsStatedOther { .. } => "bootloader-slots-stated-other",
            StopKind::BasebandUnknown => "baseband-unknown",
            StopKind::BuildNotListed { .. } => "build-not-listed",
            StopKind::BuildNotTested { .. } => "build-not-tested",
            StopKind::FactoryDownloadMismatch { .. } => "factory-download-mismatch",
            StopKind::LockedAtDryRun => "locked-at-dry-run",
            StopKind::BuildChanged => "build-changed",
            StopKind::DryRunRefused(_) => "dry-run-refused",
            StopKind::OverrideInInstall => "override-in-an-install",
            StopKind::WritesNotForDevice { .. } => "writes-not-for-this-device",
            StopKind::NoPlaceForAnswers => "no-place-for-answers",
            StopKind::ImagesNotChecked => "images-not-checked",
            StopKind::NotConfirmed => "not-confirmed",
            StopKind::LockedAtWrite => "locked-at-write",
            StopKind::WriteRefused(_) => "write-refused",
            StopKind::WriteInterrupted(_) => "write-interrupted",
            StopKind::RepairNeedsWrite => "repair-needs-write",
            StopKind::RepairNotForRelease => "repair-not-for-this-release",
            StopKind::RepairLocked => "repair-locked",
        }
    }
}

/// The closing lines of a stop after the writes began: what is on the phone
/// now, what the owner should do, and what starting again does. Derived from
/// how far the phone confirmed the writes, never fixed.
fn write_stopped_footer(reach: WriteReach, on_slot_b: bool) -> Vec<String> {
    let again = "Bring the phone to Fastboot Mode and start the installer again: it checks the phone again and writes every image again, from the first.";
    let slot_a = "Slot A, where Android is, was not written, and it is still the slot the phone starts from.";
    let slot_b_broken = "The phone was running from slot B, which is the slot being written, so it may not start until an install has run through.";
    let erased = "What Android had stored on the phone is erased. If the phone starts Android now, Android finds its data gone and asks for a factory reset.";
    let reinstall = "If the installer then says that the phone runs from slot B, or asks what is on slot B, that is this install. Start the phone once from its Fastboot Mode screen, so that it runs slot B's own bootloader, bring it back to Fastboot Mode, and start the installer again as a reinstall: slot B holds Omarchy Mobile.";
    let lines: Vec<&str> = match reach {
        WriteReach::Nothing => vec![
            "No write reached the phone: the installer sent it nothing that changes it.",
            "You can start the installer again at any time.",
        ],
        WriteReach::BootImages { done, of, in_doubt } => {
            let first = format!(
                "The install stopped while it was writing slot B's boot images: {done} of {of} were written{}.",
                if in_doubt { ", and the next one may be partly written" } else { "" }
            );
            let mut v = vec![first, "userdata was not written: what the phone had stored is still there.".to_string()];
            v.push(if on_slot_b { slot_b_broken } else { slot_a }.to_string());
            v.push(again.to_string());
            return v;
        }
        WriteReach::Userdata { pieces } => {
            let first = format!(
                "The install stopped while it was writing userdata: slot B's boot images are written, and {pieces} pieces of the system image, with one more that may be partly written."
            );
            let mut v = vec![first];
            v.push(if on_slot_b { slot_b_broken } else { slot_a }.to_string());
            v.push(erased.to_string());
            v.push(again.to_string());
            return v;
        }
        WriteReach::Written if on_slot_b => vec![
            "Every image was written and the phone confirmed each one. The command that makes slot B the active slot was not sent or was refused; the phone was running from slot B already.",
            again,
        ],
        WriteReach::Written => vec![
            "Every image was written and the phone confirmed each one, but slot B was not made the active slot.",
            slot_a,
            erased,
            again,
        ],
        WriteReach::SlotInDoubt => vec![
            "Every image was written and the phone confirmed each one. The command that makes slot B the active slot was sent, and the phone's answer did not arrive: the installer does not know which slot is active.",
            erased,
            again,
            reinstall,
        ],
        WriteReach::SlotSet | WriteReach::Rebooted => vec![
            "Every image was written, the phone confirmed each one, and slot B is the active slot. Only the restart was not confirmed.",
            "Restart the phone from its Fastboot Mode screen (Start): it then starts Omarchy Mobile from slot B.",
            "Or start the installer again, with the phone in Fastboot Mode: it writes every image again, from the first.",
            reinstall,
        ],
    };
    lines.into_iter().map(str::to_string).collect()
}

/// [`write_stopped_footer`] for a release on the running slot (format 2):
/// the slot written is the one the phone starts from, Android on it is
/// being replaced, and no slot is ever made active.
fn write_stopped_running_footer(reach: WriteReach, slot: Slot) -> Vec<String> {
    let again = "Bring the phone to Fastboot Mode and start the installer again: it checks the phone again and writes every image again, from the first.";
    let android = format!("Or put Android back with Google's factory image: {GO_BACK_TO_ANDROID}");
    match reach {
        WriteReach::Nothing => vec![
            "No write reached the phone: the installer sent it nothing that changes it.".to_string(),
            "You can start the installer again at any time.".to_string(),
        ],
        WriteReach::BootImages { done, of, in_doubt } => vec![
            format!(
                "The install stopped while it was writing slot {slot}'s boot images: {done} of {of} were written{}.",
                if in_doubt { ", and the next one may be partly written" } else { "" }
            ),
            "userdata was not written: what the phone had stored is still there.".to_string(),
            format!("Slot {slot} is the slot the phone starts from, and the Android it started is now partly replaced: the phone may not start until an install has run through."),
            again.to_string(),
            android,
        ],
        WriteReach::Userdata { pieces } => vec![
            format!("The install stopped while it was writing userdata: slot {slot}'s boot images are written, and {pieces} pieces of the system image, with one more that may be partly written."),
            format!("The phone starts from slot {slot}, which now holds Omarchy Mobile's boot images, and its system image is not complete: it does not start until an install has run through. What Android had stored on the phone is erased."),
            again.to_string(),
            android,
        ],
        // The read-back, or the restart, was not confirmed. No slot is ever
        // made active, so the phone starts what was written to its slot
        _ => vec![
            format!("Every image was written to slot {slot}, and the phone confirmed each one. The restart was not sent, or the phone did not confirm it."),
            format!("If the phone is still in Fastboot Mode, restart it from its Fastboot Mode screen (Start): it starts the slot it runs from, slot {slot}, which holds Omarchy Mobile now. If the installer said the phone's current slot changed, start the installer again instead."),
            "Or start the installer again with the phone in Fastboot Mode: it checks the phone again and writes every image again, from the first.".to_string(),
        ],
    }
}

/// [`write_stopped_running_footer`] for a Repair: only the slot's boot
/// images were being written, `userdata` was not touched, and no slot is
/// ever made active.
fn repair_stopped_footer(reach: WriteReach, slot: Slot) -> Vec<String> {
    let again = "Bring the phone to Fastboot Mode and start the repair again: it checks the phone again and writes every boot image again, from the first. It still does not write userdata.";
    let kept = "userdata was not written: your account and everything stored on the phone are still there.";
    match reach {
        WriteReach::Nothing => vec![
            "No write reached the phone: the installer sent it nothing that changes it.".to_string(),
            "You can start the repair again at any time.".to_string(),
        ],
        WriteReach::BootImages { done, of, in_doubt } => vec![
            format!(
                "The repair stopped while it was writing slot {slot}'s boot images: {done} of {of} were written{}.",
                if in_doubt { ", and the next one may be partly written" } else { "" }
            ),
            kept.to_string(),
            format!("Slot {slot} is the slot the phone starts from, and its boot images are now partly this release's: the phone may not start until a repair has run through."),
            again.to_string(),
        ],
        // Every boot image was written: the read-back or the restart was
        // not confirmed. No slot is ever made active
        _ => vec![
            format!("Every boot image was written to slot {slot}, and the phone confirmed each one. The restart was not sent, or the phone did not confirm it."),
            kept.to_string(),
            format!("If the phone is still in Fastboot Mode, restart it from its Fastboot Mode screen (Start): it starts the slot it runs from, slot {slot}. If the installer said the phone's current slot changed, start the repair again instead."),
            "Or start the repair again with the phone in Fastboot Mode: it checks the phone again and writes every boot image again, from the first.".to_string(),
        ],
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

/// Proof that the file, handed in by the owner or downloaded, is the one
/// Google publishes for the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFactoryZip {
    pub build: String,
    pub name: String,
    pub size: u64,
    /// The SHA-256 the signed build index records for it, which its bytes
    /// had: a front end that uses the file later checks it again.
    pub sha256: String,
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

/// The longest time zone name taken, in bytes. The longest in the zone
/// database is about 32; the bound keeps the answers within the one block
/// the system image keeps for them ([`crate::firstboot::SSH_KEY_MAX`]).
pub const TIMEZONE_MAX: usize = 64;

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
            && input.timezone.len() <= TIMEZONE_MAX
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
            // 32 bytes, as Wi-Fi and the phone's first start count them: a
            // letter with an accent is two, many emoji four
            if input.wifi_ssid.len() > 32 || has_control(input.wifi_ssid) {
                bad(
                    AnswerField::WifiSsid,
                    "A Wi-Fi network name is at most 32 characters (fewer with accented letters or emoji).",
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
    /// The factory image that was checked. `None` in a Repair, which
    /// writes no firmware and takes no factory image.
    pub factory_zip: Option<VerifiedFactoryZip>,
    /// The owner's answers. `None` in a Repair, which writes no `userdata`.
    pub answers: Option<AnswersSummary>,
    /// A Repair: `writes` are the running slot's boot images alone.
    pub repair: bool,
    /// The writes an install would send, in order, in words.
    pub would: Vec<String>,
    /// Whether the front end walked the release's images through the dry
    /// run (each checked against the signed manifest, the root image cut
    /// into sparse images). When not, `would` comes from the manifest alone.
    pub images_walked: bool,
    /// The maintainer's override skipped the bootloader-slot check: this
    /// plan is not evidence that an install would be allowed.
    pub slot_check_skipped: bool,
    /// Each partition an install writes, in order, with its sizes as the
    /// dry run found them on this phone. Empty unless the images were
    /// walked.
    pub writes: Vec<PlannedWrite>,
    /// The phone tells one bootloader copy's version only, and it runs from
    /// slot B in a reinstall: the version checked is slot B's bootloader,
    /// which the phone already runs, and the install switches no slot
    /// ([`SLOT_B_BOOTLOADER_IN_USE`]).
    pub slot_b_bootloader_in_use: bool,
    /// The phone tells one bootloader copy's version only, and this run
    /// measured the other: its version ([`OTHER_SLOT_MEASURED`]).
    pub other_slot_measured: Option<String>,
    /// The slot flags the measurement changed, each `name: before -> after`.
    pub slot_flags_changed: Vec<String>,
    /// A release on the running slot (format 2): the slot that is written,
    /// the one the phone runs from. `None` for format 1 (slot B, made
    /// active).
    pub running_slot: Option<Slot>,
    /// For a release on the running slot: the other slot reads as one that
    /// has started and can start (`slot-successful` yes, `slot-unbootable`
    /// no), so it holds a system, which the install leaves as it is.
    pub other_slot_started: bool,
}

/// One partition an install writes: what goes there and whether it fits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    /// The partition's name, from the verified release.
    pub partition: String,
    /// The release's file that is written to it.
    pub source: String,
    /// Bytes written: the image's size, or the root image's unpacked.
    pub bytes: u64,
    /// The partition's size on this phone.
    pub partition_bytes: u64,
    /// For `userdata`: how many sparse images the root image is sent in.
    pub pieces: Option<u32>,
}

impl PlannedWrite {
    /// One line of the list the owner confirms.
    pub fn line(&self) -> String {
        match self.pieces {
            None => format!(
                "{}: {}, {} bytes (the partition holds {} bytes)",
                self.partition, self.source, self.bytes, self.partition_bytes
            ),
            Some(pieces) => format!(
                "{}: {}, {} bytes unpacked, sent in {pieces} pieces (the partition holds {} bytes). ERASES userdata",
                self.partition, self.source, self.bytes, self.partition_bytes
            ),
        }
    }
}

/// One screen, as text. Both front ends render this and add nothing of
/// their own to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Screen {
    /// `warning`, `phone`, `unlock`, `google-terms`, `factory-image`,
    /// `answers`, `dry-run`, `done`, `confirm`, `write`, `installed` or
    /// `stopped`.
    pub state: &'static str,
    /// For `stopped`: [`Stop::id`].
    pub stop: Option<&'static str>,
    /// For `stopped`: whether starting the installer again can get past the
    /// stop ([`Stop::retry`]), so a front end offers "start again" where it
    /// can help and not where it can't. `None` on every other screen.
    pub retry: Option<Retry>,
    pub title: String,
    /// Said before everything else on the screen, and made to stand out:
    /// a check was skipped by the maintainer's override, or the screen shows
    /// steps that erase the phone in a flow that installs nothing
    /// ([`NO_INSTALL_YET`]).
    pub notice: Option<String>,
    pub body: Vec<String>,
    /// Someone else's words, drawn after the body and set apart as theirs.
    pub quote: Option<Quote>,
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
    /// The answers the person picks one of, in order (`slot-b`). Empty on
    /// every other screen.
    pub choices: Vec<ScreenChoice>,
    /// On `answers` in a run that writes a release whose root image offers
    /// the install link: the link carries three answers more (an SSH public
    /// key, a full name and an email address for git, each optional), so the
    /// front end asks for them. Left out of the JSON when false: a release
    /// that takes its answers in the image's placeholders has no place for
    /// them, and a front end asks for none.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub extras: bool,
    /// What the front end does next: `acknowledge`, `read-phone`,
    /// `slot-b` (hand in one of `choices`), `accept-terms`, `download`
    /// (which also takes a file the owner hands in), `take-file`,
    /// `answers`, `dry-run`, `confirm` (hand in what the owner typed),
    /// `write` or `none`.
    pub action: &'static str,
}

/// One answer to a question of the core's: what the front end hands back
/// (`answer`), and the core's words for it (`label`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScreenChoice {
    pub answer: &'static str,
    pub label: String,
}

/// Words that are not Omarchy Mobile's, with whose they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Quote {
    /// Whose words follow, in ours.
    pub intro: String,
    /// Their words, each drawn as a quotation.
    pub lines: Vec<String>,
    /// Ours again, about the rest of what they say.
    pub after: Vec<String>,
    /// The pages their words name.
    pub links: Vec<ScreenLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScreenLink {
    pub label: String,
    pub url: String,
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
    /// The owner's acceptance of Google's terms, if they gave it.
    terms: Option<TermsAccepted>,
    zip: Option<VerifiedFactoryZip>,
    answers: Option<AnswersSummary>,
    /// The answers as the blocks of this release's root image they go into,
    /// with the password hashed: handed to each session the flow opens.
    /// `None` for a release whose image keeps no place for them.
    root_answers: Option<RootAnswers>,
    dry_run_open: bool,
    /// The maintainer's override let a phone past the bootloader-slot stop.
    slot_check_skipped: bool,
    /// A look at the phone went past the one-slot stop on the owner's
    /// statement ([`FlowOptions::bootloader_slots_match`]).
    slots_on_owners_word: bool,
    /// A look at the phone went past the one-slot stop because it runs from
    /// slot B in a reinstall: the version it tells is slot B's.
    slot_b_bootloader_in_use: bool,
    /// The owner answered the question of what slot B holds in this run.
    slot_b_answered: bool,
    /// The owner typed the phone's codename at the confirmation, in this run.
    confirmed: bool,
    /// What an earlier run's measurement left (from the front end), or this
    /// run's, while slot B may be the active one.
    probe_record: Option<ProbeRecord>,
    /// The other slot's version, measured in this run on the pinned phone.
    measured: Option<MeasuredSlot>,
    /// The slot flags the measurement changed, each `name: before -> after`.
    probe_changed: Vec<String>,
    /// A measurement ran in this run and ended with slot A active (the
    /// phone's flags were written).
    probe_ran: bool,
    /// The phone is locked, and its other slot will be measured after the
    /// unlock: the unlock screen says so.
    probe_pending: bool,
    write_open: bool,
    /// How far the writes got when they stopped, and whether the phone ran
    /// from slot B.
    write_reach: Option<(WriteReach, bool)>,
    /// The serial number the writing session pinned, kept from the end of
    /// a write that ran through for the install link's serial proof
    /// ([`Self::link`]), and nowhere else.
    link_serial: Option<String>,
    /// A release on the running slot: the slot the phone ran from at the
    /// first look that passed, which every later look must see again.
    first_slot: Option<Slot>,
    /// A release on the running slot: the slot the writes began on, for
    /// the stop's closing lines.
    write_running: Option<Slot>,
    /// A release on the running slot: the slot a write that ran through
    /// wrote, for the install link.
    write_slot: Option<Slot>,
}

fn step(text: impl Into<String>) -> ScreenStep {
    ScreenStep {
        text: text.into(),
        command: None,
    }
}

/// [`about`], for the install link's screens.
pub(crate) fn about_size(size: u64) -> String {
    about(size)
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
            terms: None,
            zip: None,
            answers: None,
            root_answers: None,
            dry_run_open: false,
            slot_check_skipped: false,
            slots_on_owners_word: false,
            slot_b_bootloader_in_use: false,
            slot_b_answered: false,
            confirmed: false,
            probe_record: None,
            measured: None,
            probe_changed: Vec::new(),
            probe_ran: false,
            probe_pending: false,
            write_open: false,
            write_reach: None,
            link_serial: None,
            first_slot: None,
            write_running: None,
            write_slot: None,
        };
        // What an earlier run's measurement left, if the front end kept it
        flow.probe_record = flow
            .options
            .slot_probe_record
            .as_deref()
            .and_then(ProbeRecord::parse);
        // The override exists for dry runs: a flow that could write refuses
        // it whole, before a release is read or a phone is asked anything
        if flow.options.write && flow.options.maintainer_skip_bootloader_slot_check {
            flow.stop(
                StopKind::OverrideInInstall,
                Retry::AfterFix,
                "The bootloader-slot check can't be skipped in an install",
                vec![
                    "This run was started to write to a phone, and with the maintainer's override that skips the bootloader-slot check. The override is for dry runs only: nothing is ever written to a phone whose second bootloader copy was not checked.".into(),
                    "Start the installer again without the override, or as a dry run.".into(),
                ],
            );
            return flow;
        }
        // A Repair writes: it is a mode of a flow that may write, and there
        // is nothing it would show in a dry run that its own confirmation
        // does not list before anything is written
        if flow.options.repair && !flow.options.write {
            flow.stop(
                StopKind::RepairNeedsWrite,
                Retry::AfterFix,
                "A repair is a run that writes",
                vec![
                    "This run was asked to repair a phone, and it was not started as a run that may write. A repair writes the boot images of the slot the phone runs from, after it has listed them and you have typed the phone's codename.".into(),
                    "Start the installer's repair as a run that writes.".into(),
                ],
            );
            return flow;
        }
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
        let codename = release.manifest().codename.clone();
        // What making slot B active costs, and what a failed start falls
        // back to, differ by phone: writes only where that was read up on
        if flow.options.write && !WRITE_DEVICES.contains(&codename.as_str()) {
            flow.stop(
                StopKind::WritesNotForDevice { codename: codename.clone() },
                Retry::StopsAgain,
                "This version of the installer can't install on this phone model",
                vec![
                    format!("This release is for the {name} ({codename}). This version of the installer writes only to: {}.", WRITE_DEVICES.join(", ")),
                    "An install ends by making slot B the phone's active slot. What that costs, and what the phone falls back to when a start fails, differ from model to model, and for this model they have not been worked into this installer.".into(),
                    format!("A dry run works for this phone. To install, use the installer that comes with the release ({DOWNLOAD}), or ask at {ISSUES}"),
                ],
            );
            flow.release = Some(release);
            return flow;
        }
        // A Repair writes the running slot's boot images again: a release
        // that writes slot B and makes it active has no such thing. It needs
        // no place for answers and no build index, since it writes neither
        // userdata nor firmware
        if flow.options.repair {
            if !release.manifest().on_running_slot() {
                flow.stop(
                    StopKind::RepairNotForRelease,
                    Retry::StopsAgain,
                    "This release can't repair a phone",
                    vec![
                        format!("This release of Omarchy Mobile for the {name} ({codename}) is written to slot B and makes slot B the active slot, with Android kept on slot A. A repair is for a release that installs on the slot the phone runs from, and writes that slot's boot images again."),
                        format!("A phone installed from such a release is put right by installing it again. To repair a phone, use a release made to install on the slot the phone runs from, from {DOWNLOAD}."),
                    ],
                );
            }
            flow.release = Some(release);
            return flow;
        }
        // A phone written from an image with no place for the answers
        // starts with no account: a flow that writes stops here, before
        // the owner erases a phone for it. A release whose root image
        // offers the install link takes the answers there instead
        if flow.options.write
            && release.manifest().root_image.placeholders.is_empty()
            && release.manifest().install_link.is_none()
        {
            flow.stop(
                StopKind::NoPlaceForAnswers,
                Retry::StopsAgain,
                "This release can't be installed with this installer",
                vec![
                    NO_PLACE_FOR_ANSWERS.into(),
                    format!("The installer that comes with the release (\"install\", run in a terminal) puts the answers on the phone itself; or use a newer release, from {DOWNLOAD}."),
                ],
            );
            flow.release = Some(release);
            return flow;
        }
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
    /// The owner's acceptance of Google's terms on the terms step, if they
    /// gave it: which build, and when.
    pub fn google_terms_accepted(&self) -> Option<&TermsAccepted> {
        self.terms.as_ref()
    }
    /// The factory image, once it has been checked.
    pub fn factory_zip_verified(&self) -> Option<&VerifiedFactoryZip> {
        self.zip.as_ref()
    }
    /// The phone this install is pinned to, from the first look at it: the
    /// digest a later step compares the phone that answers with.
    pub fn pinned_phone(&self) -> Option<&PhoneId> {
        self.pinned.as_ref()
    }

    /// Whether this flow ends at the dry run, with no step that writes to a
    /// phone: every flow that was not begun with [`FlowOptions::write`]. A
    /// screen that shows the owner something that can't be undone (the
    /// unlock steps) asks this and says so ([`NO_INSTALL_YET`]). It is
    /// answered from the flow's own state, so the sentence stays true: a
    /// flow that may write says what its install leaves instead
    /// ([`NO_USABLE_PHONE_YET`]).
    pub fn ends_at_dry_run(&self) -> bool {
        !self.options.write
    }

    /// Whether this release installs on the slot the phone runs from
    /// (manifest format 2), with none of the slot B steps.
    pub fn on_running_slot(&self) -> bool {
        self.release
            .as_ref()
            .is_some_and(|r| r.manifest().on_running_slot())
    }

    /// Whether this flow is a Repair ([`FlowOptions::repair`]).
    pub fn repairs(&self) -> bool {
        self.options.repair
    }

    /// Whether the release's system image keeps a place for the owner's
    /// answers ([`crate::manifest::RootImage::placeholders`]).
    fn has_place_for_answers(&self) -> bool {
        self.release
            .as_ref()
            .is_some_and(|r| !r.manifest().root_image.placeholders.is_empty())
    }

    fn phone_fact(&self) -> PhoneFact {
        if let (Some((reach, _)), Some(slot)) = (self.write_reach, self.write_running) {
            if self.options.repair {
                return PhoneFact::RepairStopped { reach, slot };
            }
            return PhoneFact::WriteStoppedRunning { reach, slot };
        }
        if let Some((reach, on_slot_b)) = self.write_reach {
            return PhoneFact::WriteStopped { reach, on_slot_b };
        }
        if self.probe_ran {
            return PhoneFact::SlotsMeasured {
                unlock_shown: self.unlock_shown,
            };
        }
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
            // A Repair keeps what is on the phone, and unlocking erases it:
            // no unlock steps, and no question about them
            Judged::Locked if self.options.repair => self.stop(
                StopKind::RepairLocked,
                Retry::StopsAgain,
                "The phone's bootloader is locked",
                vec![
                    "A repair writes the boot images of the slot the phone runs from, and only an unlocked phone takes them. This phone's bootloader is locked.".into(),
                    "Unlocking it erases the phone, which is what a repair is there to avoid, so the installer does not show the steps for it here. A phone that runs Omarchy Mobile was unlocked to install it; if it is locked now, it most likely runs Android again.".into(),
                    format!("To install Omarchy Mobile on it anyway, use the installer's install, which erases the phone. To ask about this phone, open an issue: {ISSUES}"),
                ],
            ),
            Judged::Locked => {
                // Asked only of a phone about to be shown the unlock steps,
                // and after every other question
                let oem_unlocking = unlock_ability(transport).await;
                self.unlock_shown = true;
                self.step = Step::Unlock { oem_unlocking };
            }
            judged => self.after_look(judged),
        }
        self.report = Some(report);
        Ok(())
    }

    /// The owner's answer to what slot B holds ([`Step::SlotB`]). Omarchy
    /// Mobile makes the run a reinstall, and the phone the flow just looked
    /// at is judged again as one, with no new question sent to it: the
    /// bootloader copy, the build, the lock. Android, or not sure, stops
    /// the install with nothing sent and nothing written.
    pub fn answer_slot_b(&mut self, holds: SlotBHolds) -> Result<()> {
        if self.step != Step::SlotB || self.on_running_slot() {
            return self.out_of_order("the answer about slot B");
        }
        let Some(report) = self.report.clone() else {
            return self.out_of_order("the answer about slot B");
        };
        self.slot_b_answered = true;
        match holds {
            SlotBHolds::OmarchyMobile => {
                self.options.reinstall = true;
                match self.judge(&report) {
                    // A locked phone is not asked here whether it would
                    // take the unlock command: the unlock screen gives the
                    // steps for both answers, and its next look asks
                    Judged::Locked => {
                        self.unlock_shown = true;
                        self.step = Step::Unlock {
                            oem_unlocking: UnlockAbility::Unknown,
                        };
                    }
                    judged => self.after_look(judged),
                }
            }
            SlotBHolds::Android => self.stop(
                StopKind::RunsFromSlotB,
                Retry::StopsAgain,
                "Android runs from slot B",
                vec![
                    "You said that Android is on slot B, where an Android system update moved it. Omarchy Mobile is written to slot B, so installing now would overwrite the Android this phone uses.".into(),
                    "Android has to be put back on the first system copy (slot A) first, and slot A made the one the phone starts from. The install guide says how.".into(),
                ],
            ),
            SlotBHolds::NotSure => {
                let keys = self.keys_fastboot();
                self.stop(
                    StopKind::RunsFromSlotB,
                    Retry::AfterFix,
                    "Find out what is on slot B first",
                    vec![
                        "The installer goes on only once you know what slot B holds: if it is Android, an install would overwrite the Android this phone uses.".into(),
                        format!("Start the phone from its Fastboot Mode screen (Start) and see what starts: Omarchy Mobile or Android. Then bring it back to Fastboot Mode ({keys}) and start the installer again."),
                        "That restart also matters if an installer made slot B active and the phone has not started since: until it restarts, the phone may still be running the other copy's bootloader, and the version it tells is then that copy's.".into(),
                    ],
                );
            }
        }
        Ok(())
    }

    /// The owner's answer on [`Step::SlotProbe`] or [`Step::SlotProbePutBack`].
    ///
    /// `Measure` (on `SlotProbe`) and `PutBack` (on `SlotProbePutBack`) give
    /// the measurement, or the putting back, for the phone this flow looked
    /// at and pinned: the front end runs it ([`SlotProbe::step`],
    /// [`SlotProbe::restart_seen`]) and hands it back with
    /// [`Self::slot_probe_done`]. Nothing is sent to the phone here. `Stop`
    /// ends the install, with nothing more sent.
    pub fn slot_probe(&mut self, answer: ProbeAnswer) -> Result<Option<SlotProbe>> {
        // A release on the running slot switches no slot: no measurement
        // and no putting back is ever made for it, whatever the step says
        if self.on_running_slot() {
            return self.out_of_order("the answer about the bootloader slots");
        }
        let (Some(report), Some(pinned)) = (self.report.clone(), self.pinned.clone()) else {
            return self.out_of_order("the answer about the bootloader slots");
        };
        match (&self.step, answer) {
            (Step::SlotProbe, ProbeAnswer::Measure) => {
                // Asked of a phone the last look judged so, and only that
                if !(self.probe_allowed(&report) && report.unlocked && report.id == pinned) {
                    return self.out_of_order("a measurement of the other slot");
                }
                self.step = Step::SlotProbeRunning { put_back: false };
                let first = report.current_slot.chars().next().unwrap_or('a');
                Ok(Some(SlotProbe::measure(pinned, first, report.bootloader)))
            }
            (Step::SlotProbePutBack, ProbeAnswer::PutBack) => {
                let Some(record) = self.probe_record.clone().filter(|r| r.phone == pinned) else {
                    return self.out_of_order("putting slot A back");
                };
                self.step = Step::SlotProbeRunning { put_back: true };
                Ok(Some(SlotProbe::put_back(&record)))
            }
            (Step::SlotProbe | Step::SlotProbePutBack, ProbeAnswer::Stop) => {
                let put_back = self.step == Step::SlotProbePutBack;
                let keys = self.keys_fastboot();
                if put_back {
                    self.stop_with(
                        StopKind::SlotProbeDeclined,
                        Retry::Whenever,
                        PhoneFact::SlotProbeStopped {
                            active: Active::NotKnown,
                            unlock_shown: self.unlock_shown,
                        },
                        "Slot B stays the active slot",
                        vec![
                            "You chose not to put slot A back now. The phone runs from slot B, which an earlier run of the installer made active to measure it.".into(),
                            format!("To do it by hand, with Android's fastboot tool and the phone in Fastboot Mode ({keys}): \"fastboot --set-active=a\", then \"fastboot reboot-bootloader\". Starting the installer again offers it again."),
                        ],
                    );
                } else {
                    self.stop(
                        StopKind::SlotProbeDeclined,
                        Retry::Whenever,
                        "Not measured",
                        vec![
                            "You chose not to have the phone's second bootloader copy measured. Without its version the installer does not make slot B active, so the install stops here.".into(),
                            "Nothing was sent to the phone for it.".into(),
                        ],
                    );
                }
                Ok(None)
            }
            _ => self.out_of_order("the answer about the bootloader slots"),
        }
    }

    /// The measurement (or the putting back) [`Self::slot_probe`] gave, once
    /// it has finished. A measurement that read the same version on both
    /// slots goes on as a look at the phone that passed; one that read
    /// another stops at `bootloader-slots-differ`; one that did not read it
    /// stops at `slot-probe-stopped`, saying which slot is active as far as
    /// the phone confirmed. A putting back that finished asks for a new look
    /// at the phone. Refused, with nothing changed, for a measurement that
    /// has not finished or was made for another phone.
    pub fn slot_probe_done(&mut self, probe: SlotProbe) -> Result<()> {
        let Step::SlotProbeRunning { .. } = self.step else {
            return self.out_of_order("the end of a measurement");
        };
        if !probe.is_finished() {
            return Err(Error::Refused(
                "the measurement has not finished: hand it back once it says so".into(),
            ));
        }
        let (Some(report), Some(pinned)) = (self.report.clone(), self.pinned.clone()) else {
            return self.out_of_order("the end of a measurement");
        };
        if probe.phone() != &pinned {
            return Err(Error::Refused(
                "that measurement was made for another phone".into(),
            ));
        }
        self.probe_record = probe.record();
        let keys = self.keys_fastboot();
        let by_hand = format!("With Android's fastboot tool, with the phone in Fastboot Mode ({keys}): \"fastboot --set-active=a\", then \"fastboot reboot-bootloader\".");
        let end = probe.end().cloned();
        match end {
            Some(ProbeEnd::PutBack) => {
                self.probe_record = None;
                self.step = Step::Phone { not_found: false };
            }
            Some(ProbeEnd::Measured { other, changed }) => {
                self.probe_ran = true;
                self.probe_changed = changed;
                if other != report.bootloader {
                    self.stop(
                        StopKind::BootloaderSlotsDiffer { a: report.bootloader.clone(), b: other.clone() },
                        Retry::StopsAgain,
                        "The phone's two bootloader copies differ",
                        vec![
                            format!("The installer measured the phone's second bootloader copy: slot A's is {}, slot B's is {other}. Omarchy Mobile starts from slot B, so both have to be the same version, and they are not.", report.bootloader),
                            "Slot A is the active slot again, and the phone runs its bootloader.".into(),
                            "With different versions the phone may not start from slot B. Google's own tools put the same version on both: the install guide says how.".into(),
                        ],
                    );
                } else {
                    self.measured = probe.measured();
                    match self.judge(&report) {
                        Judged::Locked => {
                            self.unlock_shown = true;
                            self.step = Step::Unlock {
                                oem_unlocking: UnlockAbility::Unknown,
                            };
                        }
                        judged => self.after_look(judged),
                    }
                }
            }
            Some(ProbeEnd::NotMeasured { why, active }) => {
                let (title, mut message, retry) = probe_stop_words(&why, &keys);
                if active != Active::First {
                    message.push(match active {
                        Active::FirstSet => format!("Slot A is set as the active slot again. Restart the phone into its bootloader once ({keys}, or \"fastboot reboot-bootloader\") before you start the installer again."),
                        _ => format!("Slot B may still be the active slot. Bring the phone to Fastboot Mode ({keys}) and start the installer again: it sees that this run left slot B active and offers to put slot A back first. {by_hand}"),
                    });
                }
                self.stop_with(
                    StopKind::SlotProbeStopped { why, active },
                    retry,
                    PhoneFact::SlotProbeStopped {
                        active,
                        unlock_shown: self.unlock_shown,
                    },
                    title,
                    message,
                );
            }
            None => return Err(Error::Refused("the measurement has not finished".into())),
        }
        Ok(())
    }

    /// What to keep for the next run about a measurement that may have left
    /// slot B active ([`ProbeRecord::to_text`]); `None`: nothing (delete
    /// what was kept). A front end that measures asks this after
    /// [`Self::slot_probe_done`], and the measurement itself after each of
    /// its steps ([`SlotProbe::record`]).
    pub fn slot_probe_record(&self) -> Option<ProbeRecord> {
        self.probe_record.clone()
    }

    /// The other slot's version, when this run measured it.
    pub fn slot_measured(&self) -> Option<&MeasuredSlot> {
        self.measured.as_ref()
    }

    /// What follows a look at the phone that did not find it locked.
    fn after_look(&mut self, judged: Judged) {
        match judged {
            Judged::Stopped | Judged::Locked => {}
            Judged::AskSlotB => self.step = Step::SlotB,
            Judged::OfferProbe => self.step = Step::SlotProbe,
            Judged::OfferPutBack => self.step = Step::SlotProbePutBack,
            // A Repair needs no factory image and no answers: the dry run
            // is next
            Judged::Repair => self.step = Step::DryRun,
            // The owner has the file: nothing is downloaded, so there are no
            // terms for the installer to show
            Judged::Ready { build, same_pair } if self.options.owner_has_factory_zip => {
                self.step = Step::FactoryImage {
                    build: *build,
                    same_pair,
                    source: ImageSource::OwnerFile(OwnerFileWhy::SaidSo),
                    refused: None,
                    trouble: None,
                };
            }
            // The front end can't download it: no terms are asked for a
            // download that can't be made
            Judged::Ready { build, same_pair } if self.options.cannot_fetch_from_google => {
                self.step = Step::FactoryImage {
                    build: *build,
                    same_pair,
                    source: ImageSource::OwnerFile(OwnerFileWhy::CannotFetch),
                    refused: None,
                    trouble: None,
                };
            }
            Judged::Ready { build, same_pair } => {
                self.step = Step::GoogleTerms {
                    build: *build,
                    same_pair,
                };
            }
        }
    }

    /// The stop for a phone that runs from slot B in a run that is not a
    /// reinstall, where the owner is not asked what slot B holds.
    fn stop_runs_from_slot_b(&mut self) {
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
        let at_look = matches!(self.step, Step::Phone { .. } | Step::Unlock { .. });
        // An earlier run's measurement on this phone may have left the
        // other slot active: that comes first, before slot B is judged
        if let Some(record) = self.probe_record.clone() {
            // Whichever bootloader runs, the active flag may still name the
            // other slot: it is set back before anything else
            if record.phone == report.id {
                // A release on the running slot switches no slot, so it
                // does not put slot A back either: the owner does
                let running = self.on_running_slot();
                if at_look && self.options.can_probe_slots && report.unlocked && !running {
                    return Judged::OfferPutBack;
                }
                let keys = self.keys_fastboot();
                self.stop_with(
                    StopKind::SlotProbeStopped {
                        why: ProbeTrouble::BackOnOther,
                        active: Active::NotKnown,
                    },
                    Retry::AfterFix,
                    PhoneFact::SlotProbeStopped {
                        active: Active::NotKnown,
                        unlock_shown: self.unlock_shown,
                    },
                    "Slot B may still be the active slot",
                    vec![
                        "An earlier run of the installer measured this phone's second bootloader copy and did not finish, so slot B may still be the phone's active slot.".into(),
                        if running {
                            format!("Make slot A active again first. With Android's fastboot tool, with the phone in Fastboot Mode ({keys}), run \"fastboot --set-active=a\", then \"fastboot reboot-bootloader\".")
                        } else {
                            format!("Make slot A active again first. With Android's fastboot tool, with the phone in Fastboot Mode ({keys}), run \"fastboot --set-active=a\", then \"fastboot reboot-bootloader\". Or start the installer in a run that installs: it offers to do it.")
                        },
                    ],
                );
                return Judged::Stopped;
            }
        }
        let mut needs_probe = false;
        if self.on_running_slot() {
            // A release on the running slot: the one slot rule. Nothing of
            // slot B's steps, and no bootloader copy to compare, since the
            // bootloader that answers is the one that starts what is written
            if let Some(stopped) = self.judge_running_slot(report) {
                return stopped;
            }
            // A Repair writes no firmware: no build index, no baseband and
            // no factory image. What is left to judge is the lock
            if self.options.repair {
                return if report.unlocked {
                    Judged::Repair
                } else {
                    Judged::Locked
                };
            }
        } else {
            let on_slot_b = report.current_slot == "b";
            if on_slot_b && !self.options.reinstall {
                // Fastboot can't tell Omarchy Mobile on slot B from Android
                // after an update: the owner is asked, where the front end can
                // ask, once, and only before the phone has been judged ready
                let asking = self.options.asks_what_slot_b_holds
                    && !self.slot_b_answered
                    && matches!(self.step, Step::Phone { .. } | Step::Unlock { .. });
                if asking {
                    return Judged::AskSlotB;
                }
                self.stop_runs_from_slot_b();
                return Judged::Stopped;
            }
            self.slot_b_bootloader_in_use = false;
            let stated_other = self
                .options
                .bootloader_slots_match
                .as_ref()
                .is_some_and(|stated| *stated != report.bootloader);
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
                // The owner's statement about the other slot, which is
                // install-phone's rule: it must be the version this slot
                // reports. A statement that is not that version stops first,
                // whatever else would let the phone past; a run that has the
                // override too rests on the stricter rule.
                None if stated_other => {
                    let stated = self
                        .options
                        .bootloader_slots_match
                        .clone()
                        .unwrap_or_default();
                    let phone = report.bootloader.clone();
                    self.stop(
                    StopKind::BootloaderSlotsStatedOther { stated: stated.clone(), phone: phone.clone() },
                    Retry::StopsAgain,
                    "The bootloader version that was stated is not this phone's",
                    vec![
                        format!("This run was started with the statement that the bootloader version read from the phone's other system copy is {stated}, the same as the copy in use. The copy in use has another version: the phone reports {phone}."),
                        "If the version was mistyped, start the installer again with the version that was read from the phone.".into(),
                        format!("If {stated} is what was read from the other copy, the two copies differ, and Omarchy Mobile can't be installed on this phone as it is. Don't write a bootloader by hand: a wrong one can leave the phone unable to start. Ask at {ISSUES}"),
                    ],
                );
                    return Judged::Stopped;
                }
                // A reinstall on a phone that runs from slot B: the version it
                // tells is the copy it runs, slot B's, which is the one an
                // install from slot B starts with, and the install switches no
                // slot (decided by the maintainer on 2026-11-13). Nobody's word
                // about slot A is needed, and none is taken.
                None if on_slot_b && self.options.reinstall => {
                    self.slot_b_bootloader_in_use = true;
                }
                None if self.options.bootloader_slots_match.is_some() => {
                    self.slots_on_owners_word = true;
                }
                // The maintainer's override: this stop and no other. The flow
                // ends at a dry run, and the session it opens refuses the
                // override unless every write is held back.
                None if self.options.maintainer_skip_bootloader_slot_check => {
                    self.slot_check_skipped = true;
                }
                // Measured in this run, on this phone, from the slot it runs
                // from, and the copy in use still has the version it had
                None if self.measured.as_ref().is_some_and(|m| {
                    m.phone == report.id
                        && report.current_slot == m.first.to_string()
                        && m.version == report.bootloader
                }) => {}
                // It can be measured: every other stop comes first, and the
                // owner is asked at the end of this look
                None if at_look && self.probe_allowed(report) => needs_probe = true,
                None => {
                    self.stop_slots_unknown(report);
                    return Judged::Stopped;
                }
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
            // A newer build was tested: an Android update leads somewhere
            Err(Some((build, Some(newer)))) => {
                self.stop(
                    StopKind::BuildNotTested {
                        build: build.clone(),
                        newer_tested: Some(newer.clone()),
                    },
                    Retry::AfterFix,
                    "This phone's Android build hasn't been tested with Omarchy Mobile",
                    vec![
                        format!("This phone's bootloader and baseband match Google's build {build}, but no Omarchy Mobile release has been tested with that build's firmware, so the installer does not use it."),
                        format!("The list has a newer build that was tested: {newer}. If Android offers an update to it, install it (Settings > System > System update) and start the installer again."),
                        after_update.into(),
                    ],
                );
                return Judged::Stopped;
            }
            // Nothing newer was tested: the owner is told to wait, never to
            // update Android, which could not help
            Err(Some((build, None))) => {
                self.stop(
                    StopKind::BuildNotTested {
                        build: build.clone(),
                        newer_tested: None,
                    },
                    Retry::StopsAgain,
                    "This phone's Android build has not been tested with Omarchy Mobile yet",
                    vec![
                        format!("This phone's bootloader and baseband match Google's build {build}. That build has not been tested with Omarchy Mobile yet, and the installer uses a build only after a release was tested with it."),
                        "Nothing you can install today changes that. No newer build in the release's list was tested either, so an Android update does not help.".into(),
                        format!("What you can do is wait: once build {build} has been tested, a later release of Omarchy Mobile lists it as tested, and the installer then goes on with this phone as it is. To ask about it, or to see whether someone already has, use the project's issue tracker: {ISSUES}"),
                    ],
                );
                return Judged::Stopped;
            }
        };
        self.probe_pending = needs_probe && !report.unlocked;
        if !report.unlocked {
            Judged::Locked
        } else if needs_probe {
            Judged::OfferProbe
        } else {
            Judged::Ready {
                build: Box::new(build),
                same_pair,
            }
        }
    }

    /// A release on the running slot: the phone's current slot must be one
    /// the release accepts, and the one the first look saw. `Some`: it
    /// stopped. Never advice to switch the slot by hand: that would start
    /// the other slot's bootloader, at a version nothing here has read.
    fn judge_running_slot(&mut self, report: &PhoneReport) -> Option<Judged> {
        let (name, _) = self.names();
        let accepted: Vec<String> = self
            .release
            .as_ref()
            .map(|r| {
                r.manifest()
                    .accepted_slots()
                    .iter()
                    .map(|s| s.name().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let current = Slot::parse(&report.current_slot);
        if let (Some(first), Some(now)) = (self.first_slot, current) {
            if first != now {
                self.stop(
                    StopKind::CurrentSlotChanged {
                        was: first.name().into(),
                        now: now.name().into(),
                    },
                    Retry::AfterFix,
                    "The phone now runs from its other slot",
                    vec![
                        format!("When the installer first looked, the {name} ran from slot {first}. Now it runs from slot {now}. This release is written to the slot the phone runs from, so the installer stops rather than write a slot it did not check."),
                        "That happens when the phone restarts into its other slot, or a slot was made active by hand in the meantime. Start the installer again: it looks at the phone as it is now.".into(),
                    ],
                );
                return Some(Judged::Stopped);
            }
        }
        let Some(slot) = current.filter(|s| accepted.iter().any(|a| a == s.name())) else {
            let list = accepted
                .iter()
                .map(|s| format!("slot {s}"))
                .collect::<Vec<_>>()
                .join(" or ");
            self.stop(
                StopKind::CurrentSlotNotAccepted {
                    slot: report.current_slot.clone(),
                    accepted: accepted.clone(),
                },
                Retry::StopsAgain,
                "This release can't be installed on the slot this phone runs from yet",
                vec![
                    format!("The {name} keeps two copies of its system, slot A and slot B, and runs from one of them; Omarchy Mobile is written to the one it runs from. This phone runs from slot {}, and this release installs only on {list} for now: installing on slot {} is not supported yet.", report.current_slot, report.current_slot),
                    "A phone runs from slot B after an Android system update. Don't make the other slot active by hand to get past this stop: that starts the other slot's bootloader, at a version the installer has not read, and can leave the phone unable to start.".into(),
                    format!("A later release may install on slot {}. To ask about this phone, open an issue: {ISSUES}", report.current_slot),
                ],
            );
            return Some(Judged::Stopped);
        };
        self.first_slot.get_or_insert(slot);
        None
    }

    /// Whether the other bootloader slot of this phone may be measured: the
    /// release's descriptor and the device list allow it, this is a run that
    /// installs in a front end that can do the restarts, and the phone is a
    /// first install as far as fastboot can show one (on slot A, not said
    /// to be a reinstall, and a slot B that does not read as started and
    /// startable).
    fn probe_allowed(&self, report: &PhoneReport) -> bool {
        let (_, codename) = self.names();
        self.release
            .as_ref()
            .is_some_and(|r| r.rules().bootloader_slot_probe && !r.manifest().on_running_slot())
            && SLOT_PROBE_DEVICES.contains(&codename.as_str())
            && self.options.can_probe_slots
            && self.options.write
            && !self.options.reinstall
            && report.current_slot == "a"
            && !slot_b_reads_as_started(&report.slot_flags)
    }

    /// The stop for a phone that tells one bootloader copy's version, where
    /// nothing lets the install past: worded for what this run could have
    /// done.
    fn stop_slots_unknown(&mut self, report: &PhoneReport) {
        let (_, codename) = self.names();
        let probe_declared = self
            .release
            .as_ref()
            .is_some_and(|r| r.rules().bootloader_slot_probe)
            && SLOT_PROBE_DEVICES.contains(&codename.as_str());
        let why = if !probe_declared {
            "Reading the other copy's version means making that copy the active one, restarting into it and switching back. The installer does not do that to this phone model yet.".to_string()
        } else if report.current_slot == "a" && slot_b_reads_as_started(&report.slot_flags) {
            "This release lets the installer measure the other copy on a first install, by making it the active one, restarting into it and switching back. This phone's slot B reads as a slot that has started and can start again, which is what an earlier install of Omarchy Mobile sent back to slot A looks like, and also Android after a system update; the installer can't tell the two apart, so it measures neither. If Omarchy Mobile is on slot B, start it from there and install again as a reinstall.".to_string()
        } else if !(self.options.write && self.options.can_probe_slots) || self.options.reinstall {
            "This release lets the installer measure the other copy on a first install, by making it the active one, restarting into it and switching back, with your yes. That writes the bootloader's slot flags, so a dry run does not do it, and neither does a front end that can't restart the phone: the command-line tool's \"install --write\" and the desktop app do, before anything is written.".to_string()
        } else {
            "The installer measures the other copy only on a first install from slot A, at the look at the phone, and not now.".to_string()
        };
        self.stop(
            StopKind::BootloaderSlotsUnknown { probe_declared },
            Retry::StopsAgain,
            "The installer can't check the phone's second bootloader copy",
            vec![
                format!("The phone keeps two copies of its system (slot A and slot B), each with its own bootloader. Omarchy Mobile starts from slot B, so slot B's bootloader has to be the same version as slot A's, and this phone only tells the version of the copy in use ({}).", report.bootloader),
                why,
                format!("If the two copies differ, the phone may not start from slot B, so the installer stops rather than guess. To ask about this phone, open an issue: {ISSUES}"),
            ],
        );
    }

    fn after_terms(&mut self, source: ImageSource) -> Result<()> {
        let Step::GoogleTerms { build, same_pair } = &self.step else {
            return self.out_of_order("an answer to Google's terms");
        };
        self.step = Step::FactoryImage {
            build: build.clone(),
            same_pair: same_pair.clone(),
            source,
            refused: None,
            trouble: None,
        };
        Ok(())
    }

    /// The owner accepted what Google's download page asks, as the terms
    /// step shows it. `now`: seconds since 1970-01-01 00:00 UTC by the front
    /// end's clock (the core has none). Recorded with the build, in this
    /// flow only. From here the installer may download the factory image.
    pub fn accept_google_terms(&mut self, now: u64) -> Result<()> {
        let Step::GoogleTerms { build, .. } = &self.step else {
            return self.out_of_order("an answer to Google's terms");
        };
        self.terms = Some(TermsAccepted {
            build: build.build.clone(),
            at: now,
        });
        self.after_terms(ImageSource::Download)
    }

    /// The owner did not accept Google's terms here. Nothing is downloaded,
    /// in this flow ever; they save the file from Google's page themselves
    /// and hand it in.
    pub fn decline_google_terms(&mut self) -> Result<()> {
        self.after_terms(ImageSource::OwnerFile(OwnerFileWhy::TermsDeclined))
    }

    /// Start one try at downloading the factory image. Refused unless the
    /// owner accepted Google's terms for this build on the terms step.
    /// `have`: how many bytes the front end kept of an earlier try. The
    /// front end carries out what the [`FactoryDownload`] asks and hands it
    /// to [`Self::factory_download_done`]; see [`crate::download`].
    pub fn factory_download(&mut self, have: u64) -> Result<FactoryDownload> {
        let accepted = self.terms.clone();
        let release = self
            .index
            .as_ref()
            .map(|i| i.release().to_string())
            .unwrap_or_default();
        let Step::FactoryImage {
            build,
            source,
            trouble,
            ..
        } = &mut self.step
        else {
            return self.out_of_order("a download of the factory image");
        };
        if *source != ImageSource::Download || accepted.is_none_or(|t| t.build != build.build) {
            return Err(Error::Refused(
                "Google's terms were not accepted for this build in this install, so nothing is downloaded"
                    .into(),
            ));
        }
        let download = FactoryDownload::new(release, build, have)?;
        *trouble = None;
        Ok(download)
    }

    /// A try at the download is over. `failure`: what kept the front end
    /// from carrying it out, if anything did. The answer says what to do
    /// with the bytes it kept; the screen says the rest.
    pub fn factory_download_done(
        &mut self,
        download: FactoryDownload,
        failure: Option<FetchFailure>,
    ) -> Result<DownloadEnd> {
        let ours = self
            .index
            .as_ref()
            .map(|i| i.release().to_string())
            .unwrap_or_default();
        let Step::FactoryImage {
            build,
            source,
            refused,
            trouble,
            ..
        } = &mut self.step
        else {
            return self.out_of_order("a download of the factory image");
        };
        if *source != ImageSource::Download
            || download.release != ours
            || download.build != build.build
            || download.sha256 != build.sha256
        {
            return Err(Error::Refused(
                "the download was started for another build or another install".into(),
            ));
        }
        if failure.is_none() && !download.kept_fed() {
            return Err(Error::Refused(
                "the bytes kept earlier were not all handed in".into(),
            ));
        }
        *refused = None;
        let (got, want) = download.progress();
        // What an earlier try left that this one went on from
        let had = download.resume_from();
        let ending = match (download.ending().cloned(), failure) {
            (Some(ending), _) => ending,
            (None, Some(FetchFailure::NoAnswer)) => Ending::Unavailable(Unavailable::NoAnswer),
            (None, Some(FetchFailure::Network(detail))) => {
                *trouble = Some(DownloadTrouble::Interrupted {
                    got,
                    want,
                    detail: Some(detail),
                });
                return Ok(DownloadEnd::Kept);
            }
            // A server that went quiet: ended by the front end, and open
            // to another try like any transfer that broke off
            (None, Some(FetchFailure::Stalled { seconds })) => {
                *trouble = Some(DownloadTrouble::Interrupted {
                    got,
                    want,
                    detail: Some(format!(
                        "Google's server sent nothing for {seconds} seconds"
                    )),
                });
                return Ok(DownloadEnd::Kept);
            }
            (None, Some(FetchFailure::Storage(detail))) => {
                *trouble = Some(DownloadTrouble::Storage(detail));
                return Ok(DownloadEnd::Kept);
            }
            (None, None) if got < want => {
                *trouble = Some(DownloadTrouble::Interrupted {
                    got,
                    want,
                    detail: None,
                });
                return Ok(DownloadEnd::Kept);
            }
            (None, None) => {
                let name = download.name().to_string();
                // Over every byte, the kept ones first
                let (digest, len) = download.finish();
                if digest == build.sha256 && len == build.size {
                    self.zip = Some(VerifiedFactoryZip {
                        build: build.build.clone(),
                        name,
                        size: len,
                        sha256: digest,
                    });
                    self.step = Step::Answers;
                    return Ok(DownloadEnd::Verified);
                }
                Ending::Mismatch(Mismatch::Sha256)
            }
        };
        match ending {
            // Not a fault of the file: the owner fetches it by hand
            Ending::Unavailable(why) => {
                *source = ImageSource::OwnerFile(OwnerFileWhy::Unavailable(why));
                *trouble = None;
                Ok(DownloadEnd::Kept)
            }
            Ending::Mismatch(why) => {
                let (id, zip, size) = (build.build.clone(), build.zip.clone(), build.size);
                let list = "the release's signed list of Google's builds";
                let first = match &why {
                    Mismatch::Sha256 => format!("The installer downloaded {zip} from Google's server, and the file is not what {list} says: its SHA-256 is another than the one the list records for build {id}."),
                    Mismatch::AnnouncedSize { got } => format!("Google's server offered a file of {got} bytes as {zip}, and that is not what {list} says: the list records {size} bytes for build {id}."),
                    Mismatch::TooManyBytes => format!("Google's server sent more for {zip} than the file has, so what it sent is not what {list} says: the list records {size} bytes for build {id}, and the installer took no more than that."),
                };
                let tell = format!("If it stops here again, Google's file and this release's list do not agree: please tell us at {ISSUES}");
                // Bytes on this computer are deleted only when they can't be
                // part of the right file: the file is complete with another
                // SHA-256, or more arrived than it has. A size the server
                // said before sending anything is no such proof: nothing of
                // that answer was taken, and what an earlier try kept is
                // still hashed from its first byte before it is used.
                let (end, second, third) = match &why {
                    Mismatch::AnnouncedSize { .. } if had > 0 => (
                        DownloadEnd::Kept,
                        format!("The installer took nothing of what the server offered. The {had} bytes an earlier try downloaded stay on this computer: the server's answer says nothing about them, and they are checked from the first byte, together with the rest, before anything of the file is used."),
                        format!("Starting the installer again goes on from those bytes. {tell}"),
                    ),
                    Mismatch::AnnouncedSize { .. } => (
                        DownloadEnd::Kept,
                        "The installer took nothing of what the server offered.".to_string(),
                        format!("Starting the installer again tries the download again. {tell}"),
                    ),
                    Mismatch::Sha256 | Mismatch::TooManyBytes => (
                        DownloadEnd::Discard,
                        "The installer uses nothing of that file, and what it had downloaded of it is deleted from this computer.".to_string(),
                        format!("Starting the installer again downloads the file afresh. {tell}"),
                    ),
                };
                self.stop(
                    StopKind::FactoryDownloadMismatch { build: id, why },
                    Retry::Whenever,
                    "The downloaded file is not the one this release lists",
                    vec![first, second, third],
                );
                Ok(end)
            }
        }
    }

    /// The owner picked a file as the factory image: its name and size are
    /// checked here, before a byte is read. `Some`: feed the file's bytes to
    /// the check, then [`Self::factory_zip_done`]. `None`: not taken, and
    /// the screen says why.
    pub fn factory_zip(&mut self, file_name: &str, size: u64) -> Result<Option<FactoryZipCheck>> {
        let Step::FactoryImage {
            build,
            refused,
            trouble,
            ..
        } = &mut self.step
        else {
            return self.out_of_order("a factory image");
        };
        *trouble = None;
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
            sha256: digest,
        });
        self.step = Step::Answers;
        Ok(())
    }

    /// The owner's answers, already within the rules ([`OwnerAnswers::check`]).
    /// `salt`: for the password's hash, from [`crate::firstboot::salt_from`]
    /// with 12 random bytes the front end draws for this install (the core
    /// has no source of randomness). The flow keeps the summary and, for a
    /// release whose system image keeps a place for them, the answers as
    /// the image's blocks ([`RootAnswers`]: the password only as its hash).
    /// The password itself stays with the front end.
    pub fn answers(&mut self, answers: &OwnerAnswers, salt: &str) -> Result<()> {
        if self.step != Step::Answers {
            return self.out_of_order("the owner's answers");
        }
        let release = self
            .release
            .as_ref()
            .expect("a flow past the warning has a release");
        // A release whose root image offers the install link gets the
        // answers there, after the write: its image is written as signed,
        // placeholders and all (docs/DESIGN.md §2, M1)
        self.root_answers = if release.manifest().root_image.placeholders.is_empty()
            || release.manifest().install_link.is_some()
        {
            None
        } else {
            // A release on the running slot records the slot it is written
            // to, for the phone's kernel updates (`SLOT=` in boot-update.conf)
            let slot =
                if release.manifest().on_running_slot() {
                    Some(self.first_slot.ok_or_else(|| {
                        Error::Refused("the phone's slot is not known yet".into())
                    })?)
                } else {
                    None
                };
            Some(root_answers_on(
                release,
                answers,
                self.options.owner_ssh_key.as_deref(),
                salt,
                slot,
            )?)
        };
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
            // Not asked past the look at the phone: `judge` stops instead
            Judged::AskSlotB => {
                self.stop_runs_from_slot_b();
                return Ok(None);
            }
            // Offered at a look only: `judge` stops instead
            Judged::OfferProbe | Judged::OfferPutBack => return Ok(None),
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
            // Only `judge` of a Repair says so, and a Repair needs nothing
            // more of the phone here
            Judged::Repair => {}
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
        let slot_b = !self.on_running_slot();
        let repair = self.options.repair;
        let options = PhoneOptions {
            reinstall: self.options.reinstall && slot_b,
            // The owner's statement, if one was made: the session compares
            // it with the phone again, as `judge` just did
            bootloader_slots_confirmed: self
                .options
                .bootloader_slots_match
                .clone()
                .filter(|_| slot_b),
            // Taken only by a session that holds every write back, which
            // is the only kind `connect_pinned` opens
            maintainer_skip_bootloader_slot_check: self.slot_check_skipped,
            // What this run measured, compared again by the session
            other_slot_measured: self.measured.clone().filter(|_| slot_b),
            // So that the dry run checks the image with the key against
            // its partition, as the write will send it. A Repair writes the
            // images as they were signed
            owner_ssh_key: self.options.owner_ssh_key.clone().filter(|_| !repair),
            repair,
        };
        let opened = match Session::connect_pinned(transport, &release, &report.id, options).await {
            // The answers walk through the dry run too: it checks the image
            // holds their places where the signed manifest says
            Ok(mut session) => match self.root_answers.clone().map(|a| session.set_answers(a)) {
                Some(Err(e)) => Err(e),
                _ => Ok(session),
            },
            Err(e) => Err(e),
        };
        match opened {
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
        if !session.is_dry_run()
            || Some(session.release_id()) != ours
            || session.repairs() != self.options.repair
        {
            return Err(Error::Refused("not the dry run this install opened".into()));
        }
        let repair = self.options.repair;
        // Once skipped, on any look at the phone, the whole run says so
        self.slot_check_skipped |= session.slot_check_skipped();
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
        let running_slot = session.running_slot();
        let other_slot_started = running_slot.is_some_and(|slot| {
            self.report.as_ref().is_some_and(|r| {
                let flag = |name: &str| {
                    r.slot_flags
                        .iter()
                        .find(|(k, _)| *k == format!("slot-{name}:{}", slot.other()))
                        .map(|(_, v)| v.as_str())
                };
                flag("successful") == Some("yes") && flag("unbootable") != Some("yes")
            })
        });
        // An install has both; a Repair neither, since it writes no
        // firmware and no userdata
        let (factory_zip, answers) = (self.zip.clone(), self.answers.clone());
        if !repair && (factory_zip.is_none() || answers.is_none()) {
            return self.out_of_order("the dry run's end");
        }
        // Each partition with its sizes on this phone, for the confirmation
        let mut writes = Vec::new();
        if let (true, true, Some(release)) = (repair, walked, self.release.as_ref()) {
            // The running slot's boot images, and nothing else
            let m = release.manifest();
            for (w, size) in m.boot_writes.iter().zip(session.boot_partition_sizes()) {
                writes.push(PlannedWrite {
                    partition: w.partition(facts.target_slot).name().to_string(),
                    source: w.source.clone(),
                    bytes: w.size,
                    partition_bytes: *size,
                    pieces: None,
                });
            }
        } else if let (false, true, Some(release), Some(walk)) = (
            repair,
            walked,
            self.release.as_ref(),
            session.userdata_walk(),
        ) {
            let m = release.manifest();
            for (w, size) in m.boot_writes.iter().zip(session.boot_partition_sizes()) {
                writes.push(PlannedWrite {
                    partition: w.partition(facts.target_slot).name().to_string(),
                    source: w.source.clone(),
                    bytes: w.size,
                    partition_bytes: *size,
                    pieces: None,
                });
            }
            writes.push(PlannedWrite {
                partition: m.root_image.partition.name().to_string(),
                source: m.root_image.name.clone(),
                bytes: walk.image_len,
                partition_bytes: walk.partition_size,
                pieces: Some(walk.pieces),
            });
        }
        self.dry_run_open = false;
        let plan = DryRunPlan {
            product: facts.product,
            current_slot: facts.current_slot,
            bootloader: facts.bootloader,
            baseband: facts.baseband,
            factory_zip,
            answers,
            repair,
            would,
            images_walked: walked,
            slot_check_skipped: self.slot_check_skipped,
            writes,
            slot_b_bootloader_in_use: self.slot_b_bootloader_in_use,
            other_slot_measured: self.measured.as_ref().map(|m| m.version().to_string()),
            slot_flags_changed: self.probe_changed.clone(),
            running_slot,
            other_slot_started,
        };
        if !self.options.write {
            self.step = Step::Done(plan);
        } else if self.slot_check_skipped {
            // Not reachable while `begin` refuses the two together; kept so
            // that no later change can lead a skipped check to a write
            self.stop(
                StopKind::OverrideInInstall,
                Retry::AfterFix,
                "The bootloader-slot check can't be skipped in an install",
                vec!["The bootloader-slot check was skipped in this run, and nothing is ever written to a phone whose second bootloader copy was not checked.".into()],
            );
        } else if plan.writes.is_empty() {
            self.stop(
                StopKind::ImagesNotChecked,
                Retry::AfterFix,
                "The release's images are not all on this computer",
                if repair {
                    vec![
                        "A repair writes the release's boot images. They were not all found beside the release's other files, so none of them was checked, and the installer does not ask you to confirm a write it has not checked.".into(),
                        format!("Fetch the whole release from {DOWNLOAD}: its images directory."),
                    ]
                } else {
                    vec![
                        "An install writes the release's boot images and its system image. They were not all found beside the release's other files, so none of them was checked, and the installer does not ask you to confirm a write it has not checked.".into(),
                        format!("Fetch the whole release from {DOWNLOAD}: its images directory, and the system image (.img.zst) beside it."),
                    ]
                },
            );
        } else {
            self.step = Step::Confirm(plan);
        }
        Ok(session.into_transport())
    }

    /// The owner's answer to the list of what will be written: what they
    /// typed, as typed. Only the phone's codename goes on; anything else
    /// stops the install, with nothing written.
    pub fn confirm_install(&mut self, typed: &str) -> Result<()> {
        let Step::Confirm(plan) = &self.step else {
            return self.out_of_order("the confirmation");
        };
        let plan = plan.clone();
        let (_, codename) = self.names();
        if typed != codename {
            self.stop(
                StopKind::NotConfirmed,
                Retry::Whenever,
                "Not confirmed",
                vec![format!(
                    "What was typed is not the phone's codename ({codename}), so the {} stops here. Nothing was written.",
                    if self.options.repair { "repair" } else { "install" }
                )],
            );
            return Ok(());
        }
        self.confirmed = true;
        self.step = Step::Write(plan);
        Ok(())
    }

    /// Open the one session that can write, on the phone at `transport`.
    /// Every condition is checked again here, whatever the steps before
    /// did: this run may write; the override was not used; the release and
    /// its build index are verified; the codename was typed in this run;
    /// and the phone, asked once more, is the pinned one, passes every stop,
    /// is unlocked, and has the build the factory image was chosen for.
    /// Then the session's own checks run (each boot image against its
    /// partition, a footer image exactly), and the root image's size is
    /// checked against `userdata`, all before the first write.
    ///
    /// `Some(session)`: the front end writes the release's images through
    /// it, in the plan's order, then hands it to [`Self::write_done`], or to
    /// [`Self::write_failed`] with the error that ended it. `None`: the flow
    /// stopped, and nothing was written.
    pub async fn begin_write<T: Transport>(
        &mut self,
        mut transport: T,
    ) -> Result<Option<Session<T>>> {
        let Step::Write(plan) = &self.step else {
            return self.out_of_order("a write");
        };
        if self.write_open {
            return self.out_of_order("a write");
        }
        if !self.options.write {
            return Err(Error::Refused(
                "this run is a dry run: nothing is written".into(),
            ));
        }
        if self.options.maintainer_skip_bootloader_slot_check || self.slot_check_skipped {
            return Err(Error::OverrideOutsideDryRun);
        }
        if !self.confirmed {
            return Err(Error::Refused(
                "the phone's codename was not typed in this run: nothing is written".into(),
            ));
        }
        let repair = self.options.repair;
        // A Repair writes no firmware, so it reads no build index; every
        // other write needs the verified one
        let (Some(release), true, Some(pinned)) = (
            self.release.clone(),
            repair || self.index.is_some(),
            self.pinned.clone(),
        ) else {
            return Err(Error::Refused(
                "no verified release, no verified build index or no phone looked at in this run: nothing is written".into(),
            ));
        };
        let Some(image_len) = plan.writes.last().map(|w| w.bytes) else {
            return Err(Error::Refused(
                "the release's images were not checked in this run: nothing is written".into(),
            ));
        };
        // What the owner confirmed for a Repair is the boot images alone,
        // and what they confirmed for an install is not a Repair
        if plan.repair != repair
            || (repair
                && (!release.manifest().on_running_slot()
                    || plan.writes.len() != release.manifest().boot_writes.len()
                    || plan
                        .writes
                        .iter()
                        .any(|w| w.partition == release.manifest().root_image.partition.name())))
        {
            return Err(Error::Refused(
                "the plan the owner confirmed is not this run's: nothing is written".into(),
            ));
        }
        let report = probe(&mut transport).await?;
        match self.judge(&report) {
            Judged::Stopped => return Ok(None),
            Judged::AskSlotB => {
                self.stop_runs_from_slot_b();
                return Ok(None);
            }
            Judged::OfferProbe | Judged::OfferPutBack => return Ok(None),
            Judged::Locked => {
                self.stop(
                    StopKind::LockedAtWrite,
                    Retry::AfterFix,
                    "The phone's bootloader is locked",
                    vec![
                        "The phone's bootloader was unlocked when the installer first looked, and it is locked now. Nothing can be written to a locked phone, and nothing was.".into(),
                        "Unlocking it again erases the phone again.".into(),
                    ],
                );
                return Ok(None);
            }
            Judged::Repair => {}
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
        let on_slot_b = report.current_slot == "b";
        self.report = Some(report);
        let slot_b = !self.on_running_slot();
        let options = PhoneOptions {
            reinstall: self.options.reinstall && slot_b,
            // As in the dry run: compared with the phone once more, by the
            // session that writes, before its first write
            bootloader_slots_confirmed: self
                .options
                .bootloader_slots_match
                .clone()
                .filter(|_| slot_b),
            maintainer_skip_bootloader_slot_check: false,
            other_slot_measured: self.measured.clone().filter(|_| slot_b),
            owner_ssh_key: self.options.owner_ssh_key.clone().filter(|_| !repair),
            repair,
        };
        // Every condition above held: the one token there is, for the one
        // session that writes. Nothing else in any crate can make one.
        let token = WriteToken(());
        let opened =
            match Session::connect_writing(transport, &release, &pinned, options, token).await {
                // A Repair: the boot images alone, with no answers and no
                // userdata. The session must say it is one
                Ok(session) if repair => {
                    if session.repairs() {
                        Ok(session)
                    } else {
                        Err(Error::Refused(
                            "the session is not a repair: nothing is written".into(),
                        ))
                    }
                }
                // The answers the owner gave in this run, as the image's
                // blocks: the session takes the root image only with them,
                // and checks that before the first write
                Ok(mut session) => match self.root_answers.clone() {
                    Some(answers) => match session.set_answers(answers) {
                        Ok(()) => session
                            .preflight_userdata(image_len)
                            .await
                            .map(|()| session),
                        Err(e) => Err(e),
                    },
                    // The install link gives the phone its answers after
                    // the write, so the image goes as the release signed it
                    None if self.has_install_link() => session
                        .preflight_userdata(image_len)
                        .await
                        .map(|()| session),
                    None => Err(Error::Refused(
                        "this run has no answers for the system image: nothing is written".into(),
                    )),
                },
                Err(e) => Err(e),
            };
        match opened {
            // The session must write the slot this run checked, and the
            // plan the owner confirmed named
            Ok(session) if session.running_slot() != plan_running(&self.step) => {
                let e = Error::Refused(
                    "the session is not on the slot the owner confirmed: nothing is written".into(),
                );
                self.stop(
                    StopKind::WriteRefused(e.clone()),
                    Retry::AfterFix,
                    "The install stopped before anything was written",
                    vec![format!(
                        "The checks a write starts with did not pass ({e})."
                    )],
                );
                drop(session);
                Ok(None)
            }
            Ok(session) => {
                self.write_open = true;
                // From here a stop says how far the writes got
                self.write_reach = Some((WriteReach::Nothing, on_slot_b));
                self.write_running = session.running_slot();
                Ok(Some(session))
            }
            Err(e) => {
                let lost = matches!(e, Error::Transport(_));
                self.stop(
                    StopKind::WriteRefused(e.clone()),
                    if lost { Retry::AfterFix } else { Retry::StopsAgain },
                    "The install stopped before anything was written",
                    vec![
                        format!("The checks a write starts with did not pass ({e})."),
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

    fn our_write<T: Transport>(&self, session: &Session<T>) -> Result<()> {
        if !matches!(self.step, Step::Write(_)) || !self.write_open {
            return self.out_of_order("a write's end");
        }
        let ours = self.release.as_ref().map(|r| r.id());
        if session.is_dry_run() || Some(session.release_id()) != ours {
            return Err(Error::Refused("not the write this install opened".into()));
        }
        Ok(())
    }

    /// The session [`Self::begin_write`] opened returned `error`: the
    /// install stops, and the stop says how far the phone confirmed the
    /// writes, what state that leaves it in, and what starting again does.
    pub fn write_failed<T: Transport>(&mut self, session: Session<T>, error: &Error) -> Result<T> {
        self.our_write(&session)?;
        let on_slot_b = self.write_reach.is_some_and(|(_, b)| b);
        let reach = session.reach();
        self.write_reach = Some((reach, on_slot_b));
        self.write_open = false;
        let lost = matches!(error, Error::Transport(_));
        self.stop(
            StopKind::WriteInterrupted(error.clone()),
            Retry::Whenever,
            match (reach == WriteReach::Nothing, self.options.repair) {
                (true, false) => "The install stopped before anything was written",
                (false, false) => "The install stopped partway",
                (true, true) => "The repair stopped before anything was written",
                (false, true) => "The repair stopped partway",
            },
            vec![
                format!("The install did not run through ({error})."),
                if lost {
                    "The connection to the phone was lost. Check the cable, and don't unplug the phone or press its keys while the installer writes.".into()
                } else {
                    format!("If it stops the same way again, tell us at {ISSUES}")
                },
            ],
        );
        Ok(session.into_transport())
    }

    /// Every write went through the session: the phone confirmed each one,
    /// slot B is active and the phone was told to restart.
    pub fn write_done<T: Transport>(&mut self, session: Session<T>) -> Result<T> {
        self.our_write(&session)?;
        if !session.is_done() || session.reach() != WriteReach::Rebooted {
            return Err(Error::Refused(
                "the writes have not run through: hand the session in as failed".into(),
            ));
        }
        let Step::Write(plan) = &self.step else {
            return self.out_of_order("a write's end");
        };
        self.write_open = false;
        self.write_reach = None;
        self.link_serial = Some(session.serial().to_string());
        self.write_slot = session.running_slot();
        self.step = if self.options.repair {
            Step::Repaired(plan.clone())
        } else {
            Step::Installed(plan.clone())
        };
        Ok(session.into_transport())
    }

    /// The release's root image offers the install link at its first start
    /// (`manifest.json`'s `install_link`): after the write the install goes
    /// on through [`Self::link`], and `installed` is not the end.
    pub fn has_install_link(&self) -> bool {
        self.release
            .as_ref()
            .is_some_and(|r| r.manifest().install_link.is_some())
    }

    /// After a write that ran through, for a release with the install link:
    /// the install's next part, from the phone's first start to a phone that
    /// is ready ([`crate::link::LinkFlow`]). It takes what this run pinned
    /// and checked (the serial number the writing session read before every
    /// write, for the serial proof; the owner's factory image as the signed
    /// build index describes it) and the owner's answers, which must be the
    /// ones handed to [`Self::answers`] (the flow kept their summary), with
    /// what the link carries besides (`extras`) and a salt for the
    /// password's hash ([`crate::firstboot::salt_from`]). `backup_place`:
    /// the front end already knows where the radio backup goes.
    pub fn link(
        &mut self,
        answers: &OwnerAnswers,
        extras: &crate::firstboot::Extras<'_>,
        salt: &str,
        version: &str,
        backup_place: bool,
    ) -> Result<crate::link::LinkFlow> {
        if !matches!(self.step, Step::Installed(_)) {
            return self.out_of_order("the install link");
        }
        if !self.has_install_link() {
            return Err(Error::Refused(
                "this release's root image offers no install link".into(),
            ));
        }
        if self.answers.as_ref() != Some(&answers.summary()) {
            return Err(Error::Refused(
                "those are not the answers this install was given".into(),
            ));
        }
        let (Some(serial), Some(zip), Some(index), Some(release)) = (
            self.link_serial.clone(),
            self.zip.clone(),
            self.index.as_ref(),
            self.release.as_ref(),
        ) else {
            return Err(Error::Refused(
                "the install has nothing to hand the link".into(),
            ));
        };
        let build = index
            .data()
            .builds
            .iter()
            .find(|b| b.build == zip.build && b.zip == zip.name)
            .ok_or_else(|| Error::Refused("the factory image is not in the index".into()))?;
        let sha256 = omamobile_link::digest::from_hex32(&build.sha256)
            .ok_or_else(|| Error::Refused("the index's SHA-256 is not one".into()))?;
        // A release on the running slot: the phone is told which slot was
        // written, for its kernel updates (`SLOT=`)
        let slot = self.write_slot.filter(|_| self.on_running_slot());
        let valid = crate::link::link_answers_on(answers, extras, salt, slot)?;
        let slot = slot.map(|s| s.name().to_string());
        let (name, codename) = self.names();
        Ok(crate::link::LinkFlow::new(crate::link::LinkStart {
            serial,
            device: codename,
            device_name: name,
            release: release.manifest().version.clone(),
            factory: omamobile_link::msg::FactoryBegin {
                size: zip.size,
                sha256,
                build: zip.build,
                name: zip.name,
            },
            answers: Some(valid),
            token: None,
            backup_place,
            version: version.to_string(),
            slot,
        }))
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
            retry: None,
            title: String::new(),
            notice: if self.slot_check_skipped {
                Some(SLOT_CHECK_SKIPPED.to_string())
            } else if self.slots_on_owners_word {
                Some(SLOTS_ON_THE_OWNERS_WORD.to_string())
            } else {
                None
            },
            body: Vec::new(),
            quote: None,
            steps: Vec::new(),
            link: None,
            file: None,
            problem: None,
            plan: Vec::new(),
            footer: Vec::new(),
            choices: Vec::new(),
            extras: false,
            action: "none",
        };
        let developer_options = String::from("In Settings > About phone, tap \"Build number\" seven times. Then in Settings > System > Developer options, switch on \"OEM unlocking\". If it is greyed out, connect the phone to the internet once (it stays grey until then after a reset); if it is still grey, the phone is most likely locked to its carrier and can't run Omarchy Mobile.");
        match &self.step {
            Step::Warning if self.options.repair => {
                s.state = "warning";
                s.action = "acknowledge";
                s.title = "Repair: read this once".into();
                s.body = vec!["It is the only warning; nothing will ask again.".into()];
                s.steps = vec![
                    step(format!("A repair is for a {name} that runs Omarchy Mobile and no longer starts, for example after a kernel update: it writes this release's boot images again to the slot the phone runs from, then restarts the phone. It needs the phone unlocked and in Fastboot Mode: {keys}.")),
                    step(REPAIR_KEEPS),
                    step(format!("The boot images written are this release's ({label}). Use the newest release for this phone: the system on the phone keeps the kernel modules of its own last update, and a kernel much older than them may start without some of its drivers.")),
                    step(format!("{REPAIR_WAY_BACK} {GO_BACK_TO_ANDROID}")),
                ];
                s.footer = vec!["Nothing is asked of the phone until you accept.".into()];
            }
            Step::Warning => {
                s.state = "warning";
                s.action = "acknowledge";
                s.title = "Read this once".into();
                s.body = vec!["It is the only warning; nothing will ask again.".into()];
                s.steps = vec![
                    step("The phone is erased. Everything on it is lost: photos, messages, apps, accounts and two-factor codes. Copy off what you want to keep first. If its bootloader is still locked, unlocking it (the next step) is what erases it, before anything of Omarchy Mobile is written."),
                    step(format!("Omarchy Mobile replaces Android as the phone's system. There are no Google apps and no Play Store, and Android apps don't run on it. This release is a {label}: some things a phone normally does are missing or unfinished. The list for this phone: https://omamobile.org/devices/{codename}")),
                    if self.on_running_slot() {
                        step(format!("{ANDROID_REPLACED} {GO_BACK_TO_ANDROID}"))
                    } else {
                        step("You can go back. Android stays on the phone, unused, as a quick way back, and Google's own tools can put the phone back as it was sold. The install guide's \"Going back to Android\" has the steps.")
                    },
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
            Step::SlotB => {
                s.state = "slot-b";
                s.action = "slot-b";
                s.title = "What is on this phone's slot B?".into();
                s.body = vec![
                    format!("The {name} is running from its second system copy (slot B). Omarchy Mobile is installed on slot B, and Android is kept on the first copy (slot A) as the way back."),
                    "A phone runs from slot B for one of two reasons: Omarchy Mobile was installed on it before, or Android moved there with a system update (an update is written to the other copy, and the phone then starts from it). From Fastboot Mode the installer can't tell which, so it asks you.".into(),
                    "If Omarchy Mobile is on slot B, the installer installs it again there: it writes slot B's boot partitions and userdata again, and the phone keeps starting from slot B. It switches no slot, so the bootloader the phone runs now, slot B's, is the one it will start with, and the installer checks that one.".into(),
                    "If you say Omarchy Mobile and slot B holds Android, the install overwrites the Android this phone is using. What is left on slot A is the Android from before the update, which may no longer start; getting the phone back to Android may then take Google's own tools and factory image.".into(),
                    "If you don't know, say so: the installer then tells you how to find out.".into(),
                ];
                s.choices = SlotBHolds::ALL
                    .into_iter()
                    .map(|a| ScreenChoice {
                        answer: a.answer(),
                        label: a.label().into(),
                    })
                    .collect();
                s.footer = vec![
                    "The installer only asked the phone what it is. It has written nothing to it, and writes nothing before you have answered.".into(),
                ];
            }
            Step::SlotProbe => {
                s.state = "slot-probe";
                s.action = "slot-probe";
                s.title = "Measure the phone's second bootloader copy?".into();
                let version = self
                    .report
                    .as_ref()
                    .map(|r| r.bootloader.clone())
                    .unwrap_or_default();
                s.body = vec![
                    format!("The {name} keeps two copies of its system, slot A and slot B, each with its own bootloader. Android runs from slot A, whose bootloader is {version}. Omarchy Mobile is written to slot B, and the install ends by making slot B the one the phone starts from, so slot B's bootloader has to be the same version. This phone tells the version of the copy it runs only."),
                    "The installer can measure slot B's version itself. It makes slot B the active slot and restarts the phone into slot B's bootloader, reads the version there, then makes slot A the active slot again and restarts back. The phone restarts into its bootloader twice, showing its Fastboot Mode screen each time; it does not start Android or anything else. How long a restart takes has not been timed on a real phone yet: the installer waits up to a minute and a half for each.".into(),
                    "Nothing is written to the phone's partitions. What changes is the bootloader's own record of which slot to start (its slot flags), which the installer sets back to slot A. Starting a slot's bootloader can also change that slot's flags; the installer reads them before and after, shows what changed, and stops if slot A could not start.".into(),
                    "The risk: this starts slot B's bootloader before its version is known, and checking that version is the reason for the measurement. If slot B's bootloader does not start, or the phone starts an operating system instead of staying in Fastboot Mode, it may not come back by itself. The way back is below; read it before you answer.".into(),
                ];
                s.steps = vec![
                    step("While it runs, keep the cable in and don't press the phone's buttons."),
                    step(format!("If the phone does not come back to Fastboot Mode, bring it there with its buttons: {keys}. If an operating system starts and asks for anything (a factory reset, \"Try again\"), choose nothing and use the buttons.")),
                    step("Then start the installer again: it sees that slot B was left active and offers to put slot A back first."),
                    ScreenStep {
                        text: "Or put slot A back yourself, with Android's fastboot tool and the phone in Fastboot Mode:".into(),
                        command: Some("fastboot --set-active=a && fastboot reboot-bootloader".into()),
                    },
                ];
                s.choices = vec![
                    ScreenChoice {
                        answer: ProbeAnswer::Measure.answer(),
                        label: "Measure slot B's bootloader (two restarts into the bootloader)"
                            .into(),
                    },
                    ScreenChoice {
                        answer: ProbeAnswer::Stop.answer(),
                        label: "Don't measure: stop the install here".into(),
                    },
                ];
                s.footer = vec![
                    "So far the installer has only asked the phone what it is. It sends nothing more before you answer.".into(),
                ];
            }
            Step::SlotProbePutBack => {
                s.state = "slot-probe-put-back";
                s.action = "slot-probe";
                s.title = "Put slot A back as the active slot".into();
                s.body = vec![
                    format!("An earlier run of the installer measured this {name}'s second bootloader copy and did not finish, so slot B may still be its active slot."),
                    "The installer can make slot A the active slot again, restart the phone into its bootloader if slot B's runs now, and look at it again. Nothing is written to the phone's partitions.".into(),
                ];
                s.steps = vec![ScreenStep {
                    text: "Or do it yourself, with Android's fastboot tool and the phone in Fastboot Mode:".into(),
                    command: Some("fastboot --set-active=a && fastboot reboot-bootloader".into()),
                }];
                s.choices = vec![
                    ScreenChoice {
                        answer: ProbeAnswer::PutBack.answer(),
                        label: "Make slot A active again (at most one restart into the bootloader)"
                            .into(),
                    },
                    ScreenChoice {
                        answer: ProbeAnswer::Stop.answer(),
                        label: "Not now: stop here".into(),
                    },
                ];
                s.footer = vec![
                    "The installer only asked the phone what it is in this run. It sends nothing more before you answer.".into(),
                ];
            }
            Step::SlotProbeRunning { put_back } => {
                s.state = "slot-probe-running";
                s.title = if *put_back {
                    "Putting slot A back".into()
                } else {
                    "Measuring slot B's bootloader".into()
                };
                s.body = vec![
                    "Leave the phone connected and don't press its buttons. It restarts into its bootloader by itself.".into(),
                ];
            }
            Step::Unlock { oem_unlocking } => {
                s.state = "unlock";
                s.action = "read-phone";
                s.title = "Unlock the bootloader".into();
                // The steps below erase the phone. A flow that installs
                // nothing afterwards says so first, where it can't be missed
                let no_install = self.ends_at_dry_run();
                // An install through the install link has run against the
                // tests' pretend phone only too: the same notice
                let first = if no_install {
                    NO_INSTALL_YET
                } else {
                    NO_USABLE_PHONE_YET
                };
                s.notice = Some(match s.notice.take() {
                    Some(skipped) => format!("{first} {skipped}"),
                    None => first.to_string(),
                });
                s.body = vec![
                    format!("The {name} is in Fastboot Mode, and its bootloader is still locked."),
                    if self.on_running_slot() {
                        format!("The installer has checked what it can before you erase anything: this is the right model, it runs from slot {}, the slot this release is written to, and this release was tested with its Android build.", self.first_slot.map(|s| s.name()).unwrap_or("a"))
                    } else if self.slot_check_skipped {
                        "The installer has checked what it can before you erase anything: this is the right model, it runs from slot A or is a reinstall, and this release was tested with its Android build. Its second bootloader copy was not checked.".into()
                    } else if self.slot_b_bootloader_in_use {
                        format!("The installer has checked what it can before you erase anything: this is the right model, it runs from slot B and you said slot B holds Omarchy Mobile (a reinstall), and this release was tested with its Android build. The bootloader it reports is {SLOT_B_BOOTLOADER_IN_USE}.")
                    } else if self.slots_on_owners_word {
                        "The installer has checked what it can before you erase anything: this is the right model, it runs from slot A or is a reinstall, and this release was tested with its Android build. Its second bootloader copy was not read by the installer: that it matches the first is the statement this run was started with.".into()
                    } else if self.probe_pending {
                        "The installer has checked what it can before you erase anything: this is the right model, it runs from slot A, and this release was tested with its Android build. Its second bootloader copy can only be measured once the bootloader is unlocked: after you unlock, the installer asks whether it may measure it, and if the two copies differ the install stops there, after the unlock has already erased the phone.".into()
                    } else {
                        "The installer has checked what it can before you erase anything: this is the right model, it runs from slot A or is a reinstall, its two bootloader copies match, and this release was tested with its Android build.".into()
                    },
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
                s.body.push(if no_install {
                    "The installer does not send the unlock command, and these steps are not for now. They are what a version that can install will ask of you, shown so that you know them. This version gives you no reason to take them.".into()
                } else {
                    "The installer does not send the unlock command. You do, when you are ready:"
                        .into()
                });
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
            Step::GoogleTerms { build, .. } => {
                s.state = "google-terms";
                s.action = "accept-terms";
                s.title = "Google's terms for the factory image".into();
                s.body = vec![
                    format!("The phone's Wi-Fi, Bluetooth, sound, camera and modem need files that only Google may hand out. They are in Google's \"factory image\" for the phone. This phone's bootloader and baseband match Google's build {}, which release {} was tested with, and the installer can download that image for you from Google's own server.", build.build, build.tested_with.as_deref().unwrap_or("")),
                    "Google's page says these files are for use only on your own Pixel, and Google does not allow them to be redistributed. So the installer downloads them straight from Google to this computer, for this phone, and Omarchy Mobile does not host them or pass them on.".into(),
                    "Before Google's download page hands out a factory image, it asks you to accept Google's terms. The installer asks you the same here, and downloads nothing until you accept.".into(),
                ];
                s.quote = Some(Quote {
                    intro: "From Google's download page (the link below). These are Google's words, not Omarchy Mobile's:".into(),
                    lines: GOOGLE_TERMS.iter().map(|l| l.to_string()).collect(),
                    after: vec![GOOGLE_TERMS_THIRD_PARTY.into()],
                    links: vec![
                        ScreenLink {
                            label: "Google Terms of Service".into(),
                            url: GOOGLE_TERMS_OF_SERVICE.into(),
                        },
                        ScreenLink {
                            label: "Google Privacy Policy".into(),
                            url: GOOGLE_PRIVACY_POLICY.into(),
                        },
                    ],
                });
                s.link = Some(build.page.clone());
                s.footer = vec![
                    "If you do not accept here, the installer downloads nothing, and you can still go on: you download the file from Google's page yourself and hand the saved file to the installer.".into(),
                    "Your answer stays in this run of the installer. It is sent nowhere.".into(),
                ];
            }
            Step::FactoryImage {
                build,
                same_pair,
                source,
                refused,
                trouble,
            } => {
                s.state = "factory-image";
                s.title = "Google's factory image".into();
                s.body = vec![
                    "The phone's Wi-Fi, Bluetooth, sound, camera and modem need files that only Google may hand out. They are in Google's \"factory image\" for the phone. Omarchy Mobile does not host them or pass them on.".into(),
                    format!("This phone's bootloader and baseband match Google's build {}, which release {} was tested with.", build.build, build.tested_with.as_deref().unwrap_or("")),
                ];
                if !same_pair.is_empty() {
                    s.body.push(format!(
                        "(Also in the list with the same two versions, which can't be told apart from here: {}. {} is the tested one, and the one used.)",
                        same_pair.join(", "),
                        build.build
                    ));
                }
                let by_hand = vec![
                    step("Open Google's page (the link below) and accept Google's terms there."),
                    step(format!("Find build {} in the list for \"{codename}\" and save its file. It is about {}: {}", build.build, about(build.size), build.zip)),
                    step("Hand the saved file to the installer. It checks the file's size and SHA-256 against the release's signed list."),
                ];
                match source {
                    ImageSource::Download => {
                        s.action = "download";
                        if let Some(terms) = &self.terms {
                            s.body.push(format!("You accepted Google's terms for build {} on {}. The installer keeps that for this run only and sends it nowhere.", terms.build, terms.when()));
                        }
                        s.body.push(format!("The installer downloads the image from Google's own server, dl.google.com, to this computer: {}, about {}. It then checks the file's size and SHA-256 against the release's signed list, and uses nothing of the file before that.", build.zip, about(build.size)));
                        s.body.push("If you already saved that file from Google's page, you can hand it to the installer instead.".into());
                        s.footer = vec!["The file comes from Google and stays on this computer. Nothing of it is sent anywhere.".into()];
                    }
                    ImageSource::OwnerFile(why) => {
                        s.action = "take-file";
                        s.body.push(match why {
                            OwnerFileWhy::SaidSo => "The installer downloads nothing in this run: it checks the file you saved from Google's page.".to_string(),
                            OwnerFileWhy::CannotFetch => CANNOT_FETCH_FROM_GOOGLE.to_string(),
                            OwnerFileWhy::TermsDeclined => "You did not accept Google's terms in the installer, so it downloads nothing. You download the image from Google's page yourself, where Google asks you to accept its terms, and the installer checks the file you saved.".to_string(),
                            OwnerFileWhy::Unavailable(u) => format!(
                                "The installer could not download the image from Google's server: {} That says nothing against the file. You can still go on: download the image from Google's page yourself, and the installer checks the file you saved.",
                                match u {
                                    Unavailable::NoAnswer => "this browser got no answer from dl.google.com that a web page may read. A browser does not tell a page why: either there is no connection to Google's server, or Google's server does not let a web page read its files.".to_string(),
                                    Unavailable::OffGoogle => "Google's server sent the installer on to another address, and the installer fetches Google's files from https://dl.google.com and nowhere else, so it did not follow.".to_string(),
                                    Unavailable::TooManyRedirects => "Google's server kept sending the installer on to another address, and the installer stopped following.".to_string(),
                                    Unavailable::Status(code) => format!("Google's server answered with HTTP status {code} instead of the file."),
                                    Unavailable::BadAnswer => "Google's server answered in a way that does not fit what the installer asked for.".to_string(),
                                }
                            ),
                        });
                        s.steps = by_hand;
                        s.footer = vec![
                            "The file stays on this computer. Nothing of it is sent anywhere."
                                .into(),
                        ];
                    }
                }
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
                }).or_else(|| trouble.as_ref().map(|t| match t {
                    DownloadTrouble::Interrupted { got, want, detail } => format!(
                        "The download from Google's server stopped before it was complete: {got} of {want} bytes had arrived{}. You can try again.",
                        detail.as_ref().map(|d| format!(" ({d})")).unwrap_or_default()
                    ),
                    DownloadTrouble::Storage(detail) => format!("The installer could not keep the download on this computer ({detail}). You can try again once that is put right."),
                }));
            }
            Step::Answers => {
                s.state = "answers";
                s.action = "answers";
                s.title = "About you and the phone".into();
                s.body = vec![
                    "A user name, a PIN or password to unlock the phone (digits only gives a PIN pad), the phone's name, the time zone and, if you like, a Wi-Fi network.".into(),
                ];
                s.extras = self.options.write && self.has_install_link();
                s.body.push(if self.options.write && self.has_install_link() {
                    "They are kept in this computer's memory only, and given to the phone over the install link at its first start, after the write.".into()
                } else if self.has_install_link() {
                    "An install gives these to the phone over the install link, after the write. This dry run checks them, and keeps them nowhere.".into()
                } else {
                    match (self.has_place_for_answers(), self.options.write) {
                    (true, true) => "The phone sets up your account from these at its first start: the installer puts them into the system image as it writes it. Your PIN or password goes there only as a hash, made on this computer; the Wi-Fi password goes as you type it, since the phone needs it to connect. The installer keeps them nowhere else and sends them nowhere but to the phone.".into(),
                    (true, false) => "An install puts these into the system image, for the phone's first start. This dry run checks them, and checks where in the system image they would go; it writes nothing.".into(),
                    (false, _) => format!("{NO_PLACE_FOR_ANSWERS} This dry run checks your answers all the same, and keeps them nowhere."),
                    }
                });
            }
            Step::DryRun => {
                s.state = "dry-run";
                s.action = "dry-run";
                s.title = "The dry run".into();
                s.body = if self.options.repair {
                    vec![
                        "The installer now asks the phone every question a repair starts with, checks every boot image of the release against its signed checksum, and works out exactly what it will write.".into(),
                        "This step sends no write: it reads. Nothing is written before you have seen the list and typed the phone's codename.".into(),
                    ]
                } else if self.options.write {
                    vec![
                        "The installer now asks the phone every question an install starts with, checks every image of the release against its signed checksum, and works out exactly what it will write.".into(),
                        "This step sends no write: it reads. Nothing is written before you have seen the list and typed the phone's codename.".into(),
                    ]
                } else {
                    vec![
                        "The installer now asks the phone every question an install starts with, and lists what an install would write.".into(),
                        "A dry run sends no write: it reads.".into(),
                    ]
                };
                s.footer = vec!["Keep the phone connected, in Fastboot Mode.".into()];
            }
            Step::Done(plan) => {
                s.state = "done";
                s.title = if plan.slot_check_skipped {
                    "The dry run finished with one check skipped. Nothing was written".into()
                } else {
                    "The dry run passed. Nothing was written".into()
                };
                s.body = vec![phone_line(&name, plan)];
                s.body.extend(factory_zip_line(plan));
                if plan.slot_b_bootloader_in_use {
                    s.body.push(slot_b_bootloader_line(plan));
                }
                if let Some(slot) = plan.running_slot {
                    s.body.push(running_slot_line(slot));
                }
                s.body.extend(plan.answers.as_ref().map(|a| {
                    format!(
                        "User {} ({}), phone name {}, time zone {}, Wi-Fi {}.",
                        a.username,
                        if a.pin { "PIN" } else { "password" },
                        a.hostname,
                        a.timezone,
                        if a.wifi { "set" } else { "not set" }
                    )
                }));
                s.body.extend([
                    if self.has_install_link() {
                        "An install would give these answers to the phone over the install link, after the write; the system image is written as the release signed it.".into()
                    } else if !self.has_place_for_answers() {
                        NO_PLACE_FOR_ANSWERS.into()
                    } else if plan.images_walked {
                        "An install would put these answers into the place the system image keeps for them, so the phone sets up your account at its first start. The dry run found that place where the release's signed manifest says.".into()
                    } else {
                        "An install would put these answers into the place the system image keeps for them, so the phone sets up your account at its first start.".into()
                    },
                    if plan.images_walked {
                        "Every image of the release was checked against its signed checksum and walked through the dry run. An install would send these writes, and only these:".into()
                    } else {
                        "The release's images were not fetched for this dry run, so the list is the signed plan's. An install would send these writes, and only these:".into()
                    },
                ]);
                s.plan = plan.would.clone();
                s.footer = vec![
                    "The dry run only asked the phone questions. It has written nothing to it.".into(),
                    "This run was a dry run, and it ends here: a dry run has no step that writes to a phone.".into(),
                ];
            }
            Step::Confirm(plan) if plan.repair => {
                s.state = "confirm";
                s.action = "confirm";
                s.title = "This is what the repair will write".into();
                let slot = plan.running_slot.map(|s| s.name()).unwrap_or("a");
                s.body = vec![
                    phone_line(&name, plan),
                    format!("The repair writes this release's boot images again to slot {slot}, the slot the phone runs from, and restarts the phone. It writes nothing else."),
                    "Every boot image of the release was checked against its signed checksum, and each fits its partition on this phone. The installer will write these partitions, in this order, and no other:".into(),
                ];
                s.plan = plan.writes.iter().map(PlannedWrite::line).collect();
                s.plan.push("then: restart the phone (reboot)".into());
                s.footer = vec![
                    "userdata is not written: your account, your files and everything else stored on the phone are kept.".into(),
                    format!("Slot {slot}'s boot partitions, listed above, are overwritten with this release's boot images."),
                    format!("No slot is made active: the phone keeps starting from slot {slot}. The other slot, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written."),
                    REPAIR_WAY_BACK.into(),
                    format!("To go ahead, type the phone's codename: {codename}"),
                    "Anything else stops the repair here, with nothing written.".into(),
                ];
            }
            Step::Confirm(plan) => {
                s.state = "confirm";
                s.action = "confirm";
                s.title = "This is what the installer will write".into();
                s.body = vec![phone_line(&name, plan)];
                s.body.extend(factory_zip_line(plan));
                if plan.slot_b_bootloader_in_use {
                    s.body.push(slot_b_bootloader_line(plan));
                }
                if let Some(slot) = plan.running_slot {
                    s.body.push(running_slot_line(slot));
                }
                s.body.extend(other_slot_lines(plan));
                s.body.extend(plan.answers.as_ref().map(|a| {
                    format!(
                        "Your answers: user {} ({}), phone name {}, time zone {}, Wi-Fi {}{}. The phone sets up your account from them at its first start.",
                        a.username,
                        if a.pin { "PIN" } else { "password" },
                        a.hostname,
                        a.timezone,
                        if a.wifi { "set" } else { "not set" },
                        if self.root_answers.as_ref().is_some_and(RootAnswers::has_ssh_key) {
                            ", and this computer's SSH public key"
                        } else {
                            ""
                        }
                    )
                }));
                s.body.push("Every image of the release was checked against its signed checksum, and each fits its partition on this phone. The installer will write these partitions, in this order, and no other:".into());
                if let Some(at) = self
                    .options
                    .owner_ssh_key
                    .as_ref()
                    .and(self.release.as_ref())
                    .and_then(|r| crate::fastboot::ramdisk_write(r.manifest()))
                {
                    let name = self
                        .release
                        .as_ref()
                        .map(|r| {
                            let slot = self
                                .report
                                .as_ref()
                                .and_then(|p| Slot::parse(&p.current_slot))
                                .filter(|_| r.manifest().on_running_slot())
                                .unwrap_or(Slot::B);
                            r.manifest().boot_writes[at]
                                .partition(slot)
                                .name()
                                .to_string()
                        })
                        .unwrap_or_default();
                    s.body.push(format!("{name} is written with this computer's SSH public key added to the initramfs it carries, after the release's own files, as Omarchy Mobile's install-phone adds it, so that the phone's rescue SSH lets this computer in. Every other byte written is the release's."));
                }
                s.plan = plan.writes.iter().map(PlannedWrite::line).collect();
                if plan.running_slot.is_none() {
                    s.plan
                        .push("then: make slot B the active slot (set_active:b)".into());
                }
                s.plan.push("then: restart the phone (reboot)".into());
                if let Some(slot) = plan.running_slot {
                    s.footer = vec![
                        "userdata will be ERASED: everything Android stored on the phone is lost.".into(),
                        format!("{ANDROID_REPLACED} Slot {slot}'s boot partitions, listed above, are the ones Android starts from now, and they are overwritten."),
                        if plan.other_slot_started {
                            format!("The other slot, slot {}, reads as a slot that has started: whatever is on it (an earlier Omarchy Mobile, or an Android) is left as it is, and the phone does not start from it.", slot.other())
                        } else {
                            format!("The other slot, slot {}, is left as it is.", slot.other())
                        },
                        format!("No slot is made active: the phone keeps starting from slot {slot}. The other slot, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written."),
                        GO_BACK_TO_ANDROID.into(),
                        if self.has_install_link() {
                            LINK_ANSWERS.into()
                        } else {
                            ANSWERS_AT_FIRST_START.into()
                        },
                    ];
                    if self
                        .root_answers
                        .as_ref()
                        .is_some_and(RootAnswers::has_ssh_key)
                    {
                        s.footer.push(SSH_KEY_WITH_THE_ANSWERS.into());
                    }
                    s.footer.extend([
                        format!("To go ahead, type the phone's codename: {codename}"),
                        "Anything else stops the install here, with nothing written.".into(),
                    ]);
                } else {
                    s.footer = vec![
                    "userdata will be ERASED: everything Android stored on the phone is lost, and Android on slot A needs a factory reset if it is ever started again.".into(),
                    if plan.current_slot == "b" {
                        "Slot B's boot partitions, listed above, are overwritten: this is a reinstall, and the Omarchy Mobile that is on slot B now is replaced. If slot B holds Android instead, that Android is overwritten.".into()
                    } else {
                        "Slot B's boot partitions, listed above, are overwritten.".into()
                    },
                    if plan.other_slot_measured.is_some() {
                        "Slot A itself, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written. The bootloader's slot flags were changed by the measurement, and the write changes them once more, to make slot B active.".into()
                    } else {
                        "Slot A itself, the bootloader, radio, persist, efs, modem, devinfo and vbmeta are not written.".into()
                    },
                    if self.has_install_link() {
                        LINK_ANSWERS.into()
                    } else {
                        ANSWERS_AT_FIRST_START.into()
                    },
                ];
                    if self
                        .root_answers
                        .as_ref()
                        .is_some_and(RootAnswers::has_ssh_key)
                    {
                        s.footer.push(SSH_KEY_WITH_THE_ANSWERS.into());
                    }
                    s.footer.extend([
                        format!("To go ahead, type the phone's codename: {codename}"),
                        "Anything else stops the install here, with nothing written.".into(),
                    ]);
                }
            }
            Step::Write(plan) => {
                s.state = "write";
                s.action = "write";
                s.title = "Writing to the phone".into();
                s.body = vec![
                    match plan.running_slot {
                        Some(slot) if plan.repair => format!(
                            "The installer now writes the {} boot images that were listed, to slot {slot}, the slot the phone runs from, then restarts the phone. It writes no userdata and makes no slot active.",
                            plan.writes.len()
                        ),
                        Some(slot) => format!(
                            "The installer now writes the {} partitions that were listed, to slot {slot}, the slot the phone runs from, then restarts the phone. It makes no slot active.",
                            plan.writes.len()
                        ),
                        None => format!(
                            "The installer now writes the {} partitions that were listed, then makes slot B the active slot and restarts the phone.",
                            plan.writes.len()
                        ),
                    },
                    "Keep the phone connected and in Fastboot Mode until it restarts by itself. Don't unplug it and don't press its keys.".into(),
                ];
                s.footer = vec!["Before the first write the installer asks the phone once more what it is, and checks each image against its partition again.".into()];
            }
            Step::Installed(plan) => {
                s.state = "installed";
                s.title = "Omarchy Mobile is written, and the phone is restarting".into();
                s.body = vec![
                    format!(
                        "Every image was written, and the phone confirmed each one: {}.",
                        plan.writes
                            .iter()
                            .map(|w| w.partition.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    "Slot B is the active slot, and the phone was told to restart. It now starts Omarchy Mobile from slot B. At its first start the system grows to fill the phone's storage and sets up your account from your answers, which can take a few minutes. Then it starts into your account, locked: unlock it with your PIN or password.".into(),
                    if self.options.after_write_steps {
                        "The radio backup and Google's files for Wi-Fi, Bluetooth, sound, camera and modem are not on the phone yet. They come next, with the release's own tools: the USB connection to the phone, its first start, the radio backup, the firmware from Google's factory image, a restart and a check. Wi-Fi connects to the network you gave once the firmware is on the phone.".into()
                    } else {
                        "The radio backup and Google's files for Wi-Fi, Bluetooth, sound, camera and modem are not on the phone: this version of the installer does nothing after the restart. So Wi-Fi does not connect yet; the network you gave is kept for when it does.".into()
                    },
                ];
                let restart = match plan.running_slot {
                    Some(slot) => format!("The phone was told to restart. It now starts Omarchy Mobile from slot {slot}, the slot it ran Android from; no slot was made active."),
                    None => "Slot B is the active slot, and the phone was told to restart. It now starts Omarchy Mobile from slot B.".into(),
                };
                if plan.running_slot.is_some() {
                    s.body[1] = format!("{restart} At its first start the system grows to fill the phone's storage and sets up your account from your answers, which can take a few minutes. Then it starts into your account, locked: unlock it with your PIN or password.");
                }
                if self.has_install_link() {
                    s.action = "link";
                    s.body.truncate(1);
                    s.body.push(format!("{restart} At its first start the system grows to fill the phone's storage, which can take a few minutes."));
                    s.body.push("This release finishes its setup over the same cable: at its first start the phone offers the install link, and the installer gives it your answers, saves its radio backup on this computer and sends it the factory image you have from Google, from which the phone installs its own firmware. Keep the phone connected.".into());
                }
                s.footer = if plan.running_slot.is_some() {
                    vec![
                        "Android is no longer on this phone: Omarchy Mobile replaced it.".into(),
                        GO_BACK_TO_ANDROID.into(),
                    ]
                } else {
                    vec![
                        "Android is still on slot A, unused. Its data was erased with userdata, so it needs a factory reset if it is ever started again.".into(),
                        "The install guide's \"Going back to Android\" has the steps to put the phone back as it was sold.".into(),
                    ]
                };
            }
            Step::Repaired(plan) => {
                s.state = "repaired";
                s.title = "The boot images are written, and the phone is restarting".into();
                let slot = plan.running_slot.map(|s| s.name()).unwrap_or("a");
                s.body = vec![
                    format!(
                        "Every boot image was written, and the phone confirmed each one: {}.",
                        plan.writes
                            .iter()
                            .map(|w| w.partition.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!("The phone was told to restart. It starts Omarchy Mobile from slot {slot}, the slot it ran from, with this release's boot images; no slot was made active."),
                    "userdata was not written: the phone starts into your account as it was, with everything that was stored on it.".into(),
                ];
                s.footer = vec![REPAIR_WAY_BACK.into(), GO_BACK_TO_ANDROID.into()];
            }
            Step::Stopped(stop) => {
                s.state = "stopped";
                s.stop = Some(stop.id());
                s.retry = Some(stop.retry);
                s.title = stop.title.clone();
                s.body = stop.message.clone();
                s.footer = stop.footer();
            }
        }
        s.fit_to_print();
        s
    }
}

impl Screen {
    /// The last thing done to a screen: every string of it through
    /// [`crate::text::plain`]. A phone's answers were made plain where they
    /// were read; this is for everything else a screen is built from (the
    /// name of a file the owner picked, what an HTTP client said of a
    /// server's answer, the release's own names), and for the phone's again.
    /// Public for the screens the command-line tool makes itself after a
    /// write (the steps a release's tools carry out), which pass through
    /// the same filter.
    pub fn fit_to_print(&mut self) {
        use crate::text::plain;
        let each = |list: &mut Vec<String>| list.iter_mut().for_each(|s| *s = plain(s));
        self.title = plain(&self.title);
        for text in [&mut self.notice, &mut self.link, &mut self.problem]
            .into_iter()
            .flatten()
        {
            *text = plain(text);
        }
        each(&mut self.body);
        each(&mut self.plan);
        each(&mut self.footer);
        for choice in &mut self.choices {
            choice.label = plain(&choice.label);
        }
        if let Some(quote) = &mut self.quote {
            quote.intro = plain(&quote.intro);
            each(&mut quote.lines);
            each(&mut quote.after);
            for link in &mut quote.links {
                link.label = plain(&link.label);
                link.url = plain(&link.url);
            }
        }
        for step in &mut self.steps {
            step.text = plain(&step.text);
            if let Some(command) = &mut step.command {
                *command = plain(command);
            }
        }
        if let Some(file) = &mut self.file {
            file.name = plain(&file.name);
            file.build = plain(&file.build);
        }
    }
}

/// The confirmation's lines about a measurement of the other slot: the
/// version on both copies, how it was read, and what it changed.
fn other_slot_lines(plan: &DryRunPlan) -> Vec<String> {
    let Some(other) = &plan.other_slot_measured else {
        return Vec::new();
    };
    let mut lines = vec![format!(
        "Bootloader {other} on both copies: {OTHER_SLOT_MEASURED}."
    )];
    lines.push(if plan.slot_flags_changed.is_empty() {
        "The slot flags the phone reports read the same after the measurement as before it."
            .to_string()
    } else {
        format!(
            "The measurement changed these slot flags: {}.",
            plan.slot_flags_changed.join("; ")
        )
    });
    lines
}

/// The first line of the dry run's last screen and of the confirmation:
/// the phone as the dry run found it.
fn phone_line(name: &str, plan: &DryRunPlan) -> String {
    format!(
        "The {name} ({}) is unlocked and runs from slot {}; bootloader {}{}.",
        plan.product,
        plan.current_slot,
        plan.bootloader,
        plan.baseband
            .as_ref()
            .map(|b| format!(", baseband {b}"))
            .unwrap_or_default()
    )
}

/// The line about the factory image that was checked: none in a Repair,
/// which takes none.
fn factory_zip_line(plan: &DryRunPlan) -> Option<String> {
    plan.factory_zip.as_ref().map(|z| {
        format!(
            "Google's factory image for build {} is the right file ({}).",
            z.build, z.name
        )
    })
}

/// The line of the dry run's last screen and of the confirmation that says
/// which slot a release on the running slot writes, and why no bootloader
/// copy is compared.
fn running_slot_line(slot: Slot) -> String {
    format!("This release is written to slot {slot}, the slot the phone runs from, and no slot is made active: the bootloader that answered is the one that starts it, so its second copy is not needed.")
}

/// The line of the dry run's last screen and of the confirmation that says
/// which bootloader copy a reinstall's check rests on.
fn slot_b_bootloader_line(plan: &DryRunPlan) -> String {
    format!(
        "Bootloader {}: {SLOT_B_BOOTLOADER_IN_USE}. The phone tells the version of that copy only; slot A's was not read, and an install that stays on slot B does not need it.",
        plan.bootloader
    )
}

/// The title, the message and whether a new run helps, for a measurement
/// that did not read the other slot. What it leaves of the slots is added by
/// the caller.
fn probe_stop_words(why: &ProbeTrouble, keys: &str) -> (&'static str, Vec<String>, Retry) {
    let ask = format!("To ask about this phone, open an issue: {ISSUES}");
    match why {
        ProbeTrouble::NotAsLookedAt(what) => (
            "The phone changed before the measurement",
            vec![
                format!("The measurement did not begin: {what}. Nothing was sent to the phone for it."),
                "Start the installer again, with the one phone connected.".into(),
            ],
            Retry::AfterFix,
        ),
        ProbeTrouble::SlotBStarted => (
            "Slot B reads as a slot that has started",
            vec![
                "This phone's slot B reads as a slot that has started and can start again: an earlier install of Omarchy Mobile sent back to slot A looks like that, and so does Android after a system update. The installer can't tell the two apart, so it measures neither, and nothing was sent to the phone for it.".into(),
                ask,
            ],
            Retry::StopsAgain,
        ),
        ProbeTrouble::SetActiveRefused(said) => (
            "The phone refused to make slot B active",
            vec![
                format!("The phone answered the request to make slot B active with: {said}. The installer made slot A active again."),
                ask,
            ],
            Retry::StopsAgain,
        ),
        ProbeTrouble::CurrentFollowsFlag => (
            "This phone's bootloader can't show which copy runs",
            vec![
                "Right after slot B was made active, before any restart, the phone already reported slot B as its current slot. On such a bootloader the installer can't tell from the phone whether it really restarted into slot B's bootloader, so it can't trust what it would read there.".into(),
                "The installer made slot A active again and did not restart the phone.".into(),
                ask,
            ],
            Retry::StopsAgain,
        ),
        ProbeTrouble::NeverLeft => (
            "The phone did not restart",
            vec![
                "The phone confirmed that it would restart into its bootloader, and it never left the USB bus. A phone that did not restart can't show the other slot's bootloader, so nothing was measured.".into(),
                "The installer made slot A active again where the phone still answered.".into(),
                "Check the cable, then start the installer again.".into(),
            ],
            Retry::AfterFix,
        ),
        ProbeTrouble::NotBack { into } if *into == 'b' => (
            "The phone did not come back from slot B's bootloader",
            vec![
                "The phone restarted into slot B's bootloader and did not come back to Fastboot Mode in time. It may have started an operating system from slot B (an older Android, or whatever slot B holds), or slot B's bootloader may not have started.".into(),
                format!("Bring it back to Fastboot Mode with its buttons: {keys}. If it does not get there, hold Power for about 30 seconds, then try the buttons again; if it still does not, ask at {ISSUES} before you do anything else."),
                "If an operating system starts and asks for anything (a factory reset, \"Try again\"), choose nothing: go to Fastboot Mode with the buttons instead.".into(),
            ],
            Retry::AfterFix,
        ),
        ProbeTrouble::NotBack { .. } => (
            "The phone did not come back to Fastboot Mode",
            vec![
                format!("The phone was set back to slot A and restarted, and it did not come back to Fastboot Mode in time. It may have started Android from slot A. Bring it back to Fastboot Mode with its buttons ({keys}) and start the installer again."),
            ],
            Retry::AfterFix,
        ),
        ProbeTrouble::OtherPhone => (
            "Another phone answered after the restart",
            vec![
                "After a restart, the phone that answered is not the one this install looked at, so the installer sent it nothing more.".into(),
                "Connect only the phone you mean to install, and start the installer again.".into(),
            ],
            Retry::AfterFix,
        ),
        ProbeTrouble::CameBackOnFirst => (
            "The phone came back on slot A",
            vec![
                "After slot B was made active and the phone restarted, it came back running slot A's bootloader, so slot B's was not read. Slot B's bootloader may not have started, and the phone went back to slot A by itself.".into(),
                "The installer made slot A active again.".into(),
                ask,
            ],
            Retry::StopsAgain,
        ),
        ProbeTrouble::PutBackRefused(said) => (
            "The phone refused to make slot A active again",
            vec![format!("On slot B's bootloader, the phone answered the request to make slot A active with: {said}.")],
            Retry::AfterFix,
        ),
        ProbeTrouble::BackOnOther => (
            "The phone came back on slot B",
            vec!["Slot A was made active and the phone restarted, and it came back still running slot B's bootloader.".into()],
            Retry::AfterFix,
        ),
        ProbeTrouble::FirstChanged { was, now } => (
            "Slot A's bootloader is not the version it was",
            vec![
                format!("Before the measurement slot A's bootloader was {was}; after it, the phone reports {now}. The installer goes no further with a phone whose readings it can't explain."),
                ask,
            ],
            Retry::StopsAgain,
        ),
        ProbeTrouble::FirstNotStartable(flags) => (
            "Slot A's flags say it can't start",
            vec![
                format!("After the measurement the phone's flags for slot A say it can't start ({flags}). Slot A holds the Android that is the phone's way back, so the installer writes nothing."),
                format!("Don't start the phone from the bootloader menu for now. {ask}"),
            ],
            Retry::StopsAgain,
        ),
        ProbeTrouble::Lost(e) => (
            "The link to the phone was lost during the measurement",
            vec![format!("The measurement stopped: {e}.")],
            Retry::AfterFix,
        ),
    }
}

/// The slot a plan at the confirmation or the write names, for a release
/// on the running slot.
fn plan_running(step: &Step) -> Option<Slot> {
    match step {
        Step::Confirm(plan) | Step::Write(plan) => plan.running_slot,
        _ => None,
    }
}

enum Judged {
    Stopped,
    /// The owner is asked whether to measure the other bootloader slot.
    OfferProbe,
    /// An earlier run's measurement left slot B active on this phone.
    OfferPutBack,
    /// The phone runs from slot B: the owner is asked what slot B holds.
    AskSlotB,
    Locked,
    /// A Repair: the phone passed every stop of one, and is unlocked.
    Repair,
    Ready {
        build: Box<Build>,
        same_pair: Vec<String>,
    },
}

#[cfg(test)]
mod tests;
