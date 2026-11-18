// SPDX-License-Identifier: MIT
//! The flow's write steps for the page, in a build made with the cargo
//! feature `web-write-preview` and no other. The page as it ships has none
//! of this (tools/check-shipping), and nothing here has met a real phone
//! or a real browser (docs/WEB-WRITE.md).
//!
//! It is the page's counterpart of the command-line tool's `run_flow` and
//! `walk_images`, and like them it holds no rule:
//!
//! - **One way to a write.** The session that writes is the one
//!   `Flow::begin_write` opens, after the dry run of the same flow and the
//!   codename typed by the owner. This file opens no session of its own and
//!   cannot make what a writing session needs (DESIGN §11).
//! - **One gate.** Every command leaves through the core's command layer.
//!   Nothing here, and nothing this module exports, takes a command or a
//!   partition name.
//! - **The core's sizes.** A download is as large as the session's sparse
//!   splitter cut it (`install::UserdataFeed`, capped by the session's
//!   `max-download-size` and `install::SEGMENT_CAP`) and is handed to the
//!   transport in the core's pieces. The page reads a file in whatever
//!   pieces the browser gives it; how the bytes are cut for the phone is
//!   not its to say.
//! - **The core's stops.** A session that returns an error is handed back
//!   to the flow at once (`write_failed`, `dry_run_failed`), with the error
//!   as it came, so the screen says how far the phone confirmed the writes.
//!   The page cannot forget to: it is done here, not in its script.
//! - **Not the owner's statement.** A phone that tells one bootloader
//!   slot's version stops this flow as it stops every flow of the page; the
//!   statement that passes that stop is the command line's (DESIGN §6).
//!
//! Every file is checked before a session is opened: each boot image
//! against the signed manifest and the root image's SHA-256. The page does
//! that twice in a run that writes, once before the dry run's session and
//! once, after the codename, before the session that writes; a file that is
//! not the release's is found with the phone as it was. Each boot image is
//! checked once more as it is sent, and the session hashes the root image
//! again as it is streamed (DESIGN §11, rule 10).
use super::{js, AnyTransport, InstallFlow, Phone};
use omamobile_installer_core::bundle::{RootImageCheck, VerifiedRootImage};
use omamobile_installer_core::fastboot::{PieceReport, Session};
use omamobile_installer_core::flow::{Flow, FlowOptions, Step};
use omamobile_installer_core::install::{UserdataFeed, SEGMENT_CAP};
use omamobile_installer_core::Error;
use serde_json::json;
use wasm_bindgen::prelude::*;

/// What a build with this file says of itself, for the page's first line.
/// It is here, not in the page's script, so that the sentence and the code
/// it describes are in a build together or not at all.
const STATEMENT: &str = "This is a write preview, made for an attended test and not for use. Unlike the page that ships, this build can write to a phone: after a dry run, a list of what will be written and the phone's codename typed by you, it erases userdata and writes slot B's boot partitions. It has never been run against a real phone.";

/// The preview's first line. Only a build with the write preview has it.
#[wasm_bindgen]
pub fn web_write_preview() -> String {
    STATEMENT.into()
}

/// The browser's USB chooser, filtered to fastboot interfaces, as in the
/// page that ships; the phone it gives is reached through the page's own
/// transport (`webusb.rs`), which has the core's wait for an answer.
#[wasm_bindgen]
pub async fn request_phone() -> Result<Phone, JsError> {
    let (device, serial) = super::webusb::choose().await?;
    Ok(Phone {
        link: super::Link::WebUsb(device),
        serial,
    })
}

/// The session the flow opened, while the page hands it the release's files.
struct Open {
    session: Session<AnyTransport>,
    feed: Option<UserdataFeed>,
}

/// The write preview's part of an [`InstallFlow`].
#[derive(Default)]
pub(crate) struct Preview {
    /// Which boot images were checked against the signed manifest since
    /// the last session ended.
    boot_checked: Vec<bool>,
    root_check: Option<RootImageCheck>,
    /// The root image's proof: its SHA-256 was the signed manifest's.
    root: Option<VerifiedRootImage>,
    open: Option<Open>,
}

/// As the page ships it, but for the one thing this build is for.
fn write_options(reinstall: bool) -> FlowOptions {
    let mut options = super::page_options(reinstall);
    options.write = true;
    options
}

fn no_session() -> JsError {
    JsError::new("no session is open: the dry run or the write has not been begun")
}

#[wasm_bindgen]
impl InstallFlow {
    /// As [`InstallFlow::begin`], for a flow that may write: after the dry
    /// run it lists what will be written and waits for the codename.
    /// Everything else is as the page ships it: it cannot fetch from
    /// Google, and it states nothing about the phone's other bootloader
    /// slot.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_write_preview(
        sha256sums: &[u8],
        sha256sums_sig: &[u8],
        keyring: &[u8],
        manifest_json: &[u8],
        manifest_txt: &[u8],
        device_conf: &[u8],
        index: Option<Vec<u8>>,
        reinstall: bool,
    ) -> InstallFlow {
        let files = omamobile_installer_core::bundle::ReleaseFiles {
            sha256sums,
            sha256sums_sig,
            keyring,
            manifest_json,
            manifest_txt,
            device_conf,
        };
        InstallFlow {
            flow: Flow::begin(
                &files,
                index.as_deref(),
                &omamobile_installer_core::signature::TrustedKeys::omamobile(),
                write_options(reinstall),
            ),
            check: None,
            download: None,
            #[cfg(feature = "selftest")]
            built_in: false,
            preview: Preview::default(),
        }
    }

    /// The same on the signed test bundle, trusted by its test key: for
    /// the preview's tests, in a build that also has the self-test.
    #[cfg(feature = "selftest")]
    pub fn selftest_write_preview(reinstall: bool) -> Result<InstallFlow, JsError> {
        let files = omamobile_installer_core::bundle::ReleaseFiles {
            sha256sums: fx!("SHA256SUMS"),
            sha256sums_sig: fx!("SHA256SUMS.sig"),
            keyring: fx!("test-key.gpg"),
            manifest_json: fx!("manifest.json"),
            manifest_txt: fx!("MANIFEST"),
            device_conf: fx!("device.conf"),
        };
        let trusted =
            omamobile_installer_core::signature::TrustedKeys::new(&[core::str::from_utf8(fx!(
                "test-key.fingerprint"
            ))
            .map_err(js)?])
            .map_err(js)?;
        Ok(InstallFlow {
            flow: Flow::begin(
                &files,
                Some(fx!("factory-builds.json")),
                &trusted,
                write_options(reinstall),
            ),
            check: None,
            download: None,
            // The page hands the files in, as it would a real release's
            built_in: false,
            preview: Preview::default(),
        })
    }

    /// The owner's answer to the list of what will be written: what they
    /// typed, as typed. The core takes the phone's codename or stops.
    pub fn confirm_install(&mut self, typed: &str) -> Result<(), JsError> {
        self.flow.confirm_install(typed).map_err(js)
    }

    /// One boot image of the release, by its place in the plan: checked
    /// against the signed manifest, and nothing else is done with it.
    pub fn images_check_boot(&mut self, index: usize, bytes: &[u8]) -> Result<(), JsError> {
        let release = self
            .flow
            .release()
            .ok_or_else(|| JsError::new("the flow has no release"))?;
        let writes = &release.manifest().boot_writes;
        let write = writes
            .get(index)
            .ok_or_else(|| JsError::new("the release has no such boot image"))?;
        release.verify_boot_image(write, bytes).map_err(js)?;
        self.preview.boot_checked.resize(writes.len(), false);
        self.preview.boot_checked[index] = true;
        Ok(())
    }

    /// Start hashing the root image (`.img.zst`), from its first byte.
    pub fn images_root_begin(&mut self) -> Result<(), JsError> {
        let release = self
            .flow
            .release()
            .ok_or_else(|| JsError::new("the flow has no release"))?;
        self.preview.root = None;
        self.preview.root_check = Some(release.root_image_check());
        Ok(())
    }
    /// The next piece of the root image, as the browser reads the file.
    pub fn images_root_update(&mut self, chunk: &[u8]) {
        if let Some(check) = self.preview.root_check.as_mut() {
            check.update(chunk);
        }
    }
    /// Whether the bytes were the root image the signed manifest lists.
    pub fn images_root_done(&mut self) -> Result<(), JsError> {
        let check = self
            .preview
            .root_check
            .take()
            .ok_or_else(|| JsError::new("no root image is being checked"))?;
        self.preview.root = Some(check.finish().map_err(js)?);
        Ok(())
    }

    /// Open the session of the step the flow is on: the dry run's, which
    /// holds every write back, or, once the codename was typed, the one
    /// that writes. Either is the flow's to open, with every check it
    /// makes; this function chooses nothing. Refused unless every file of
    /// the release was checked first. `false`: the flow stopped, and its
    /// screen says why.
    pub async fn session_open(&mut self, phone: Phone) -> Result<bool, JsError> {
        if self.preview.open.is_some() {
            return Err(JsError::new("a session is already open"));
        }
        let boots = self
            .flow
            .release()
            .map_or(0, |r| r.manifest().boot_writes.len());
        let all_checked = self.preview.boot_checked.len() == boots
            && self.preview.boot_checked.iter().all(|c| *c)
            && self.preview.root.is_some();
        if !all_checked {
            return Err(JsError::new(
                "every file of the release is checked before a session is opened: nothing was sent to the phone",
            ));
        }
        let writing = match self.flow.step() {
            Step::DryRun => false,
            Step::Write(_) => true,
            _ => return Err(JsError::new(
                "the flow is at neither its dry run nor its write: nothing was sent to the phone",
            )),
        };
        let transport = phone.open().await?;
        let session = if writing {
            self.flow.begin_write(transport).await.map_err(js)?
        } else {
            self.flow.dry_run(transport).await.map_err(js)?
        };
        let Some(mut session) = session else {
            self.forget_checks();
            return Ok(false);
        };
        if writing {
            // The core has no clock: the browser's times each flash of userdata
            session.time_flashes_with(Box::new(|| {
                core::time::Duration::from_secs_f64(js_sys::Date::now().max(0.0) / 1000.0)
            }));
        }
        self.preview.open = Some(Open {
            session,
            feed: None,
        });
        Ok(true)
    }

    /// The next boot image of the plan through the open session: checked
    /// against the signed manifest once more, then sent (or, in a dry run,
    /// recorded as held back). `false`: the session returned an error, the
    /// flow has it, and its screen says how far the writes got.
    pub async fn session_boot(&mut self, index: usize, bytes: &[u8]) -> Result<bool, JsError> {
        let mut open = self.preview.open.take().ok_or_else(no_session)?;
        let result = async {
            let release = self
                .flow
                .release()
                .ok_or_else(|| Error::Refused("the flow has no release".into()))?;
            let write = release
                .manifest()
                .boot_writes
                .get(index)
                .ok_or_else(|| Error::Refused("the release has no such boot image".into()))?;
            let image = release.verify_boot_image(write, bytes)?;
            open.session.flash_boot(&image).await
        }
        .await;
        self.settle(open, result)
    }

    /// The next piece of the root image, in order from its first byte.
    /// Every sparse image it completes is sent before this returns: await
    /// it before reading more of the file. `false`: as [`Self::session_boot`].
    pub async fn session_root_feed(&mut self, chunk: &[u8]) -> Result<bool, JsError> {
        let mut open = self.preview.open.take().ok_or_else(no_session)?;
        let result = match self.preview.root.as_ref() {
            Some(proof) => {
                let Open { session, feed } = &mut open;
                let feed =
                    feed.get_or_insert_with(|| UserdataFeed::new(session, Some(SEGMENT_CAP)));
                feed.feed(session, proof, chunk, &mut |_, _| {}).await
            }
            None => Err(Error::Refused("the root image was not checked".into())),
        };
        self.settle(open, result)
    }

    /// The root image's last byte was fed: its last piece, the session's
    /// own check that what was streamed is the signed image, then
    /// `set_active:b` and `reboot` (in a dry run, recorded as held back),
    /// and the session goes back to the flow. `false`: as
    /// [`Self::session_boot`]. `true`: the flow's next screen is the dry
    /// run's end, or "installed".
    pub async fn session_finish(&mut self) -> Result<bool, JsError> {
        let mut open = self.preview.open.take().ok_or_else(no_session)?;
        let result = async {
            let feed = open
                .feed
                .take()
                .ok_or_else(|| Error::Refused("userdata has not been written".into()))?;
            feed.end(&mut open.session, &mut |_, _| {}).await?;
            open.session.activate_and_reboot().await
        }
        .await;
        if !self.settle(open, result)? {
            return Ok(false);
        }
        let open = self.preview.open.take().ok_or_else(no_session)?;
        self.forget_checks();
        if open.session.is_dry_run() {
            self.flow
                .dry_run_done(open.session)
                .map(|_| ())
                .map_err(js)?;
        } else {
            self.flow.write_done(open.session).map(|_| ()).map_err(js)?;
        }
        Ok(true)
    }

    /// The page cannot go on (a file of the release could not be read any
    /// more): the open session goes back to the flow as failed, and its
    /// screen says how far the writes got.
    pub fn session_abandon(&mut self, why: &str) -> Result<(), JsError> {
        let open = self.preview.open.take().ok_or_else(no_session)?;
        let error = Error::Transport(format!("reading the release's files: {why}"));
        self.settle::<()>(open, Err(error)).map(|_| ())
    }

    /// Where the open session is, as JSON, for a progress line and for the
    /// first run's timing questions: whether it writes, the commands sent
    /// so far (in a dry run, with each write held back), the root image's
    /// blocks done and in all, and each piece of `userdata` it took: the
    /// bytes downloaded, the blocks its flash covered, and how long the
    /// phone took over that flash (milliseconds; nothing in a dry run, and
    /// nothing for a piece that was sent and not confirmed).
    pub fn session_progress(&self) -> String {
        let Some(open) = self.preview.open.as_ref() else {
            return "null".into();
        };
        let piece = |p: &PieceReport| {
            json!({
                "bytes": p.bytes as f64,
                "blocks": p.blocks,
                "flash_ms": p.flash.map(|d| d.as_secs_f64() * 1000.0),
            })
        };
        let (done, total) = open.feed.as_ref().map_or((0, 0), |f| f.progress());
        json!({
            "writes": !open.session.is_dry_run(),
            "commands": open.session.log().len(),
            "blocks_done": done,
            "blocks": total,
            "pieces": open.session.pieces().iter().map(piece).collect::<Vec<_>>(),
        })
        .to_string()
    }
}

impl InstallFlow {
    /// The checks are for one session: the next one needs them again.
    fn forget_checks(&mut self) {
        self.preview.boot_checked.clear();
        self.preview.root_check = None;
        self.preview.root = None;
    }

    /// After a step through the open session: keep it open, or, on an
    /// error, hand it back to the flow at once, which stops and says how
    /// far the phone confirmed the writes.
    fn settle<V>(
        &mut self,
        open: Open,
        result: omamobile_installer_core::Result<V>,
    ) -> Result<bool, JsError> {
        match result {
            Ok(_) => {
                self.preview.open = Some(open);
                Ok(true)
            }
            Err(e) => {
                self.forget_checks();
                if open.session.is_dry_run() {
                    self.flow.dry_run_failed(&e).map_err(js)?;
                } else {
                    self.flow
                        .write_failed(open.session, &e)
                        .map(|_| ())
                        .map_err(js)?;
                }
                Ok(false)
            }
        }
    }
}
