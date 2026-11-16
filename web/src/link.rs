// SPDX-License-Identifier: MIT
//! The install link's screens in the page: the preview build only (cargo
//! feature `link-preview`; never in the page as it ships, which
//! `tools/check-shipping` holds).
//!
//! The same core as the command-line tool and the desktop app
//! ([`omamobile_installer_core::link::LinkFlow`]): the page draws its screens
//! and hands in what the owner did. What only a browser does is here: the
//! clock, `crypto.getRandomValues` for each `hello`'s nonce, reading the
//! owner's factory image from the `File` they picked a piece at a time
//! (`Blob.slice`), and the link's USB transport ([`crate::link_webusb`]).
//!
//! What is not built yet, and stops the link in the page where it is needed:
//! the radio backup's directory. The plan writes it through the File System
//! Access API's directory picker as a stream; until that is built the backup
//! step says so and the owner finishes with the desktop app or the command
//! line, which go on from the phone's stage. The page also reaches the link
//! only after a write, and the page as it is cannot write (the web-write
//! preview is on another branch): in this build the link is reached by the
//! self-test's pretend phone only.
use omamobile_installer_core::flow::Screen;
use omamobile_installer_core::link::{BackupFiles, LinkFlow, LinkRecord, LinkShell};
use omamobile_installer_core::{Error, Result};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

fn js<E: core::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}

/// Wait `ms` on the browser's timer.
async fn sleep_ms(ms: u64) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let global = js_sys::global();
        if let Ok(set) = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout")) {
            if let Ok(set) = set.dyn_into::<js_sys::Function>() {
                let _ = set.call2(&global, &resolve, &JsValue::from_f64(ms as f64));
            }
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// `crypto.getRandomValues`, the browser's source.
fn random(buf: &mut [u8]) {
    let global = js_sys::global();
    let crypto = js_sys::Reflect::get(&global, &JsValue::from_str("crypto")).ok();
    let get = crypto.as_ref().and_then(|c| {
        js_sys::Reflect::get(c, &JsValue::from_str("getRandomValues"))
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()
    });
    let (Some(crypto), Some(get)) = (crypto, get) else {
        // No nonce can be drawn: nothing may be said to a phone
        panic!("this browser has no crypto.getRandomValues");
    };
    let array = js_sys::Uint8Array::new_with_length(buf.len() as u32);
    get.call1(&crypto, &array)
        .expect("crypto.getRandomValues answered");
    array.copy_to(buf);
}

/// The page's side of the link: its clock, its randomness, the owner's
/// file, and the page's functions that draw a screen and a step's progress.
pub struct PageShell {
    start: f64,
    /// For the pretend phone: its clock in place of the browser's.
    #[cfg(feature = "selftest")]
    pretend: Option<omamobile_installer_core::link::pretend::Clock>,
    factory: Option<web_sys::Blob>,
    on_screen: js_sys::Function,
    on_progress: js_sys::Function,
    /// The page's record, in memory only (the plan's Q12: never in the
    /// browser's storage).
    record: Option<LinkRecord>,
    /// The self-test's backup, in memory: the page has no directory to
    /// write it to yet.
    #[cfg(feature = "selftest")]
    backup: std::collections::BTreeMap<String, Vec<u8>>,
    #[cfg(feature = "selftest")]
    finished: Option<BackupFiles>,
}

impl PageShell {
    fn no_backup() -> Error {
        Error::Refused("this preview of the page cannot write the radio backup yet: finish with the desktop app or the command line, which go on from here".into())
    }
}

impl LinkShell for PageShell {
    fn now_ms(&mut self) -> u64 {
        #[cfg(feature = "selftest")]
        if let Some(c) = &self.pretend {
            return c.now();
        }
        (js_sys::Date::now() - self.start).max(0.0) as u64
    }
    fn wall_clock_s(&mut self) -> u64 {
        (js_sys::Date::now() / 1000.0) as u64
    }
    async fn sleep(&mut self, ms: u64) {
        #[cfg(feature = "selftest")]
        if let Some(c) = &self.pretend {
            c.pass(ms);
            return;
        }
        sleep_ms(ms).await;
    }
    fn random(&mut self, buf: &mut [u8]) {
        random(buf);
    }
    fn go_on(&mut self) -> bool {
        true
    }
    fn screen(&mut self, screen: &Screen) {
        let json = serde_json::to_string(screen).unwrap_or_default();
        let _ = self
            .on_screen
            .call1(&JsValue::NULL, &JsValue::from_str(&json));
    }
    fn progress(&mut self, what: &'static str, done: u64, total: u64) {
        let _ = self.on_progress.call3(
            &JsValue::NULL,
            &JsValue::from_str(what),
            &JsValue::from_f64(done as f64),
            &JsValue::from_f64(total as f64),
        );
    }
    fn keep(&mut self, record: &LinkRecord) -> Result<()> {
        self.record = Some(record.clone());
        Ok(())
    }
    async fn backup_open(&mut self, _name: &str, _size: u64) -> Result<u64> {
        #[cfg(feature = "selftest")]
        return Ok(self.backup.get(_name).map_or(0, |f| f.len() as u64));
        #[allow(unreachable_code)]
        Err(Self::no_backup())
    }
    async fn backup_write(&mut self, _name: &str, _offset: u64, _data: &[u8]) -> Result<()> {
        #[cfg(feature = "selftest")]
        {
            let f = self.backup.entry(_name.to_string()).or_default();
            f.truncate(_offset as usize);
            f.extend_from_slice(_data);
            return Ok(());
        }
        #[allow(unreachable_code)]
        Err(Self::no_backup())
    }
    async fn backup_digest(&mut self, _name: &str) -> Result<[u8; 32]> {
        #[cfg(feature = "selftest")]
        if let Some(f) = self.backup.get(_name) {
            return Ok(omamobile_link::digest::sha256(f));
        }
        Err(Self::no_backup())
    }
    async fn backup_discard(&mut self, _name: &str) -> Result<()> {
        #[cfg(feature = "selftest")]
        {
            self.backup.remove(_name);
            return Ok(());
        }
        #[allow(unreachable_code)]
        Err(Self::no_backup())
    }
    async fn backup_finish(&mut self, _files: &BackupFiles) -> Result<()> {
        #[cfg(feature = "selftest")]
        {
            self.finished = Some(_files.clone());
            return Ok(());
        }
        #[allow(unreachable_code)]
        Err(Self::no_backup())
    }
    async fn factory_read(&mut self, offset: u64, len: u32) -> Result<Vec<u8>> {
        let blob = self
            .factory
            .as_ref()
            .ok_or_else(|| Error::Refused("no factory image was picked".into()))?;
        let piece = blob
            .slice_with_f64_and_f64(offset as f64, (offset + len as u64) as f64)
            .map_err(|_| Error::Refused("the file could not be read".into()))?;
        let bytes = JsFuture::from(piece.array_buffer())
            .await
            .map_err(|_| Error::Refused("the file could not be read".into()))?;
        let out = js_sys::Uint8Array::new(&bytes).to_vec();
        if out.len() != len as usize {
            return Err(Error::Refused("the file is shorter than it was".into()));
        }
        Ok(out)
    }
}

/// Which way the link goes: the phone's USB function, or the self-test's
/// pretend phone.
enum Way {
    #[allow(dead_code)]
    Usb(crate::link_webusb::WebUsbLink),
    #[cfg(feature = "selftest")]
    Pretend(Box<omamobile_installer_core::link::pretend::PretendPhone>),
}

/// The install link's flow, for the page.
#[wasm_bindgen]
pub struct LinkPreview {
    flow: LinkFlow,
    way: Way,
    shell: PageShell,
}

#[wasm_bindgen]
impl LinkPreview {
    /// The current screen, as JSON (the core's Screen).
    pub fn screen(&self) -> String {
        serde_json::to_string(&self.flow.screen()).unwrap_or_default()
    }

    /// Drive the link until the owner is needed or it ended. Each new screen
    /// while the phone works goes to `on_screen`, each step's progress to
    /// `on_progress`.
    pub async fn run(&mut self) -> core::result::Result<(), JsError> {
        match &mut self.way {
            Way::Usb(t) => self.flow.run(t, &mut self.shell).await.map_err(js),
            #[cfg(feature = "selftest")]
            Way::Pretend(t) => self.flow.run(t.as_mut(), &mut self.shell).await.map_err(js),
        }
    }

    pub fn pair(&mut self, code: &str) -> core::result::Result<(), JsError> {
        self.flow.pair(code).map_err(js)
    }
    pub fn retry(&mut self) -> core::result::Result<(), JsError> {
        self.flow.retry().map_err(js)
    }
}

#[cfg(feature = "selftest")]
#[wasm_bindgen]
impl LinkPreview {
    /// The self-test's link: a pretend phone at its first start after a
    /// write (`omamobile_installer_core::link::pretend`), on a clock of its
    /// own, and the self-test bundle's factory image as `factory`, the File
    /// the page read it from.
    pub fn pretend(
        factory: web_sys::Blob,
        on_screen: js_sys::Function,
        on_progress: js_sys::Function,
    ) -> core::result::Result<LinkPreview, JsError> {
        use omamobile_installer_core::link::pretend::{Clock, PretendPhone};
        use omamobile_installer_core::link::LinkStart;
        use omamobile_link::msg::FactoryBegin;
        let zip = include_bytes!(
            "../../fixtures/selftest/factory/cheetah-test.260101.001-factory-31c2322a.zip"
        );
        let clock = Clock::default();
        let phone = PretendPhone::first_start("FAKESERIAL0001", "cheetah", clock.clone(), 1);
        let answers = omamobile_installer_core::flow::OwnerAnswers::check(
            &omamobile_installer_core::flow::AnswersInput {
                username: "alex",
                password: "481516",
                password_again: "481516",
                hostname: "",
                timezone: "Europe/Paris",
                wifi_ssid: "",
                wifi_psk: "",
            },
        )
        .map_err(|_| JsError::new("the made-up answers"))?;
        let valid = omamobile_installer_core::link::link_answers(
            &answers,
            &Default::default(),
            "saltsaltsaltsalt",
        )
        .map_err(js)?;
        let flow = LinkFlow::new(LinkStart {
            serial: "FAKESERIAL0001".into(),
            device: "cheetah".into(),
            device_name: "Pixel 7 Pro".into(),
            release: "2026.01.02".into(),
            factory: FactoryBegin {
                size: zip.len() as u64,
                sha256: omamobile_link::digest::sha256(zip),
                build: "TEST.260101.001".into(),
                name: "cheetah-test.260101.001-factory-31c2322a.zip".into(),
            },
            answers: Some(valid),
            token: None,
            backup_place: true,
            version: "0.0.1".into(),
        });
        Ok(LinkPreview {
            flow,
            way: Way::Pretend(Box::new(phone)),
            shell: PageShell {
                start: js_sys::Date::now(),
                pretend: Some(clock),
                factory: Some(factory),
                on_screen,
                on_progress,
                record: None,
                backup: Default::default(),
                finished: None,
            },
        })
    }

    /// What the pretend phone's screen shows.
    pub fn pretend_shows(&self) -> Option<String> {
        match &self.way {
            Way::Pretend(p) => p.shows(),
            Way::Usb(_) => None,
        }
    }

    /// What the self-test checks at the end, as JSON: the backup's names,
    /// whether its files were written, how the pretend phone's link ended.
    pub fn pretend_report(&self) -> String {
        let Way::Pretend(p) = &self.way else {
            return "{}".into();
        };
        serde_json::json!({
            "backup": self.shell.backup.keys().collect::<Vec<_>>(),
            "finished_files": self.shell.finished.is_some(),
            "done": p.sim.done,
            "restarts": p.sim.restarts,
            "provision_starts": p.sim.provision_starts,
            "record_kept": self.shell.record.is_some(),
        })
        .to_string()
    }
}

/// Unused outside a build that reaches the link from a write: kept so the
/// shell's real path (the browser's clock and USB) is compiled and checked.
#[allow(dead_code)]
fn usb_shell(on_screen: js_sys::Function, on_progress: js_sys::Function) -> PageShell {
    PageShell {
        start: js_sys::Date::now(),
        #[cfg(feature = "selftest")]
        pretend: None,
        factory: None,
        on_screen,
        on_progress,
        record: None,
        #[cfg(feature = "selftest")]
        backup: Default::default(),
        #[cfg(feature = "selftest")]
        finished: None,
    }
}
