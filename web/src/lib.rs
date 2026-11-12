// SPDX-License-Identifier: MIT
//! The installer core as a WebAssembly module for the web page.
//!
//! The same `omamobile-installer-core` as the CLI, and the same USB transport
//! (`omamobile-installer-usb`: nusb's WebUSB backend). This file only turns
//! the core's API into JavaScript classes; it holds no rule of its own.
//!
//! The page drives [`InstallFlow`], the core's one install flow: it renders
//! the screen the flow gives it and hands in what the person did. The flow
//! ends at the dry run, so nothing the page does through it can write to a
//! phone.
//!
//! The browser's part is what only a browser can do: the device chooser
//! (`navigator.usb.requestDevice`, which needs a click), fetching files and
//! reading the file the person picked. Bytes come in as `Uint8Array`s.
//!
//! [`Install`] is the older, direct way to a session, kept for the write
//! path's size and its self-test. On a real phone it is a dry run unless the
//! module is built with `enable-writes`, which nothing turns on. The write
//! path is compiled in either way (the flag is read at run time), so the
//! module's size is the real one.
use omamobile_installer_core::bundle::{
    self, ReleaseFiles, RootImageCheck, VerifiedRelease, VerifiedRootImage,
};
use omamobile_installer_core::fastboot::{
    PhoneId, PhoneOptions, Session, Transport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS,
};
use omamobile_installer_core::flow::{
    AnswersInput, FactoryZipCheck, Flow, FlowOptions, OwnerAnswers,
};
use omamobile_installer_core::install::UserdataStream;
use omamobile_installer_core::signature::TrustedKeys;
use omamobile_installer_usb::UsbTransport;
use serde_json::json;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

fn js<E: core::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}

/// A release whose signature, checksums and rules all passed.
#[wasm_bindgen]
pub struct Release {
    inner: VerifiedRelease,
}

/// Check a release's small files with the fingerprints built into this module.
#[wasm_bindgen]
pub fn verify_release(
    sha256sums: &[u8],
    sha256sums_sig: &[u8],
    keyring: &[u8],
    manifest_json: &[u8],
    manifest_txt: &[u8],
    device_conf: &[u8],
) -> Result<Release, JsError> {
    let files = ReleaseFiles {
        sha256sums,
        sha256sums_sig,
        keyring,
        manifest_json,
        manifest_txt,
        device_conf,
    };
    Ok(Release {
        inner: bundle::verify_release(&files, &TrustedKeys::omamobile()).map_err(js)?,
    })
}

#[wasm_bindgen]
impl Release {
    /// The release and its plan, as JSON for the page to show.
    pub fn describe(&self) -> String {
        describe(&self.inner)
    }

    /// Any bundle file against the signed manifest.
    pub fn check_file(&self, path: &str, bytes: &[u8]) -> Result<(), JsError> {
        self.inner.check_file(path, bytes).map_err(js)
    }

    /// Start hashing the compressed root image (from the cached download).
    pub fn root_check(&self) -> RootCheck {
        RootCheck {
            inner: Some(self.inner.root_image_check()),
        }
    }
}

#[wasm_bindgen]
pub struct RootCheck {
    inner: Option<RootImageCheck>,
}

#[wasm_bindgen]
impl RootCheck {
    pub fn update(&mut self, chunk: &[u8]) {
        if let Some(c) = self.inner.as_mut() {
            c.update(chunk);
        }
    }
    pub fn finish(&mut self) -> Result<RootProof, JsError> {
        let check = self
            .inner
            .take()
            .ok_or_else(|| JsError::new("already finished"))?;
        Ok(RootProof {
            inner: check.finish().map_err(js)?,
        })
    }
}

/// Proof that the root image matched the signed manifest.
#[wasm_bindgen]
pub struct RootProof {
    inner: VerifiedRootImage,
}

/// Where a phone is: a USB device the person picked in the browser's
/// chooser, or, in a self-test build, the tests' pretend phone in memory.
#[derive(Clone)]
enum Link {
    Usb(nusb::Device),
    #[cfg(feature = "selftest")]
    Fake(fake::FakePhone),
}

/// The transport for either kind of [`Link`].
enum AnyTransport {
    Usb(UsbTransport),
    #[cfg(feature = "selftest")]
    Fake(fake::FakePhone),
}

impl Transport for AnyTransport {
    async fn send(&mut self, data: &[u8]) -> omamobile_installer_core::Result<usize> {
        match self {
            AnyTransport::Usb(t) => t.send(data).await,
            #[cfg(feature = "selftest")]
            AnyTransport::Fake(t) => t.send(data).await,
        }
    }
    async fn recv(&mut self) -> omamobile_installer_core::Result<Vec<u8>> {
        match self {
            AnyTransport::Usb(t) => t.recv().await,
            #[cfg(feature = "selftest")]
            AnyTransport::Fake(t) => t.recv().await,
        }
    }
}

/// A phone the person picked.
#[wasm_bindgen]
pub struct Phone {
    link: Link,
    serial: String,
}

impl Phone {
    async fn open(&self) -> Result<AnyTransport, JsError> {
        Ok(match &self.link {
            Link::Usb(device) => AnyTransport::Usb(UsbTransport::open(device).await.map_err(js)?),
            #[cfg(feature = "selftest")]
            Link::Fake(phone) => AnyTransport::Fake(phone.clone()),
        })
    }
    fn id(&self) -> Result<PhoneId, JsError> {
        PhoneId::of_serial(&self.serial).ok_or_else(|| {
            JsError::new("the phone tells no serial number, so an install can't be tied to it")
        })
    }
}

#[wasm_bindgen]
impl Phone {
    /// The same phone again, for the next step that looks at it.
    pub fn again(&self) -> Phone {
        Phone {
            link: self.link.clone(),
            serial: self.serial.clone(),
        }
    }
}

/// Show the browser's USB chooser, filtered to fastboot interfaces. Must be
/// called from a click. The page never sees a device the person didn't pick.
#[wasm_bindgen]
pub async fn request_phone() -> Result<Phone, JsError> {
    let usb = web_sys::window()
        .ok_or_else(|| JsError::new("no window"))?
        .navigator()
        .usb();
    if usb.is_undefined() {
        return Err(JsError::new(
            "this browser has no WebUSB (a Chromium-based browser is needed)",
        ));
    }
    let filter = web_sys::UsbDeviceFilter::new();
    filter.set_class_code(USB_CLASS);
    filter.set_subclass_code(USB_SUBCLASS);
    filter.set_protocol_code(USB_PROTOCOL);
    let options = web_sys::UsbDeviceRequestOptions::new(&[filter]);
    let picked = JsFuture::from(usb.request_device(&options))
        .await
        .map_err(|_| JsError::new("no phone was chosen"))?;
    let picked: web_sys::UsbDevice = picked
        .dyn_into()
        .map_err(|_| JsError::new("not a USB device"))?;
    let serial = picked.serial_number().unwrap_or_default();
    let device = nusb::Device::from_js(picked).await.map_err(js)?;
    Ok(Phone {
        link: Link::Usb(device),
        serial,
    })
}

/// The core's install flow, for the page: one screen at a time, as JSON
/// (`core/src/flow.rs`, [`omamobile_installer_core::flow::Screen`]). Every
/// decision and every word on a screen is the core's.
#[wasm_bindgen]
pub struct InstallFlow {
    flow: Flow,
    check: Option<FactoryZipCheck>,
    /// The self-test bundle's images are built in, to walk through a dry run.
    #[cfg(feature = "selftest")]
    built_in: bool,
}

#[wasm_bindgen]
impl InstallFlow {
    /// Start on a release's small files, checked with the fingerprints built
    /// into this module. `index`: the release's `factory-builds.json`, when
    /// it has one. A release that fails its checks gives a flow whose first
    /// screen says so.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        sha256sums: &[u8],
        sha256sums_sig: &[u8],
        keyring: &[u8],
        manifest_json: &[u8],
        manifest_txt: &[u8],
        device_conf: &[u8],
        index: Option<Vec<u8>>,
        reinstall: bool,
    ) -> InstallFlow {
        let files = ReleaseFiles {
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
                &TrustedKeys::omamobile(),
                // The maintainer's bootloader-slot override is not offered
                // here: the page as it ships has no way to set it
                FlowOptions {
                    reinstall,
                    ..Default::default()
                },
            ),
            check: None,
            #[cfg(feature = "selftest")]
            built_in: false,
        }
    }

    /// The current screen, as JSON.
    pub fn screen(&self) -> String {
        serde_json::to_string(&self.flow.screen()).unwrap_or_default()
    }

    /// The verified release, as JSON, or nothing when it did not verify.
    pub fn release(&self) -> Option<String> {
        self.flow.release().map(describe)
    }

    pub fn acknowledge_warning(&mut self) -> Result<(), JsError> {
        self.flow.acknowledge_warning().map_err(js)
    }
    pub fn decline_warning(&mut self) -> Result<(), JsError> {
        self.flow.decline_warning().map_err(js)
    }
    /// The person chose no phone, or none is in Fastboot Mode.
    pub fn no_phone(&mut self) -> Result<(), JsError> {
        self.flow.no_phone().map_err(js)
    }

    /// Ask the phone what it is. Sends `getvar` and, to a locked phone about
    /// to be shown the unlock steps, the one question
    /// `flashing get_unlock_ability`. Both read; nothing else is sent.
    pub async fn read_phone(&mut self, phone: Phone) -> Result<(), JsError> {
        let id = phone.id()?;
        let mut transport = phone.open().await?;
        self.flow.read_phone(&mut transport, &id).await.map_err(js)
    }

    /// The file the person picked as the factory image: its name and size.
    /// `true`: feed its bytes to [`Self::zip_update`], then [`Self::zip_done`].
    /// `false`: not taken; the screen says why.
    pub fn zip_begin(&mut self, name: &str, size: f64) -> Result<bool, JsError> {
        self.check = self.flow.factory_zip(name, size as u64).map_err(js)?;
        Ok(self.check.is_some())
    }
    /// The next piece of the file, as the browser reads it.
    pub fn zip_update(&mut self, chunk: &[u8]) {
        if let Some(check) = self.check.as_mut() {
            check.update(chunk);
        }
    }
    pub fn zip_done(&mut self) -> Result<(), JsError> {
        let check = self
            .check
            .take()
            .ok_or_else(|| JsError::new("no file is being checked"))?;
        self.flow.factory_zip_done(check).map_err(js)
    }

    /// The owner's answers. Returns the problems as JSON, `[]` when they
    /// were taken. Nothing typed is kept by this module.
    #[allow(clippy::too_many_arguments)]
    pub fn answers(
        &mut self,
        username: &str,
        password: &str,
        password_again: &str,
        hostname: &str,
        timezone: &str,
        wifi_ssid: &str,
        wifi_psk: &str,
    ) -> Result<String, JsError> {
        let input = AnswersInput {
            username,
            password,
            password_again,
            hostname,
            timezone,
            wifi_ssid,
            wifi_psk,
        };
        match OwnerAnswers::check(&input) {
            Ok(answers) => {
                self.flow.answers(&answers).map_err(js)?;
                Ok("[]".into())
            }
            Err(problems) => Ok(serde_json::to_string(&problems).unwrap_or_default()),
        }
    }

    /// The dry run: every check an install starts with, and no write.
    pub async fn dry_run(&mut self, phone: Phone) -> Result<(), JsError> {
        let transport = phone.open().await?;
        let Some(session) = self.flow.dry_run(transport).await.map_err(js)? else {
            return Ok(());
        };
        #[cfg(feature = "selftest")]
        let session = if self.built_in {
            let mut session = session;
            if let Err(e) = walk_built_in(&mut session, self.flow.release()).await {
                return self.flow.dry_run_failed(&e).map_err(js);
            }
            session
        } else {
            session
        };
        self.flow.dry_run_done(session).map(|_| ()).map_err(js)
    }
}

fn describe(release: &VerifiedRelease) -> String {
    let m = release.manifest();
    json!({
        "version": m.version, "label": m.label, "codename": m.codename, "device": m.device_name,
        "kernel": m.kernel_release, "signed_by": release.signer().primary, "signing_key": release.signer().signing_key,
        "boot_images": m.boot_writes.iter().map(|w| json!({"partition": w.partition.name(), "source": w.source, "size": w.size})).collect::<Vec<_>>(),
        "root_image": {"partition": m.root_image.partition.name(), "name": m.root_image.name, "size": m.root_image.size},
        "build_index": release.has_build_index(),
    })
    .to_string()
}

/// An install session on one phone: the core's fixed sequence.
#[wasm_bindgen]
pub struct Install {
    session: Session<UsbTransport>,
    release: VerifiedRelease,
    stream: Option<UserdataStream>,
    root: Option<VerifiedRootImage>,
    begun: bool,
}

/// Open the phone and run every read-only check. A phone that reports only
/// one slot's bootloader version is refused: nobody's word is taken for the
/// other slot.
#[wasm_bindgen]
pub async fn connect(release: Release, phone: Phone, reinstall: bool) -> Result<Install, JsError> {
    // The only kind of link in a build without the pretend phone
    #[allow(clippy::infallible_destructuring_match)]
    let device = match &phone.link {
        Link::Usb(device) => device,
        #[cfg(feature = "selftest")]
        Link::Fake(_) => return Err(JsError::new("not a USB phone")),
    };
    let transport = UsbTransport::open(device).await.map_err(js)?;
    let options = PhoneOptions {
        reinstall,
        bootloader_slots_confirmed: None,
        dry_run: !cfg!(feature = "enable-writes"),
        // Never on a path that can be built to write
        maintainer_skip_bootloader_slot_check: false,
    };
    let session = Session::connect(transport, &release.inner, &phone.serial, options)
        .await
        .map_err(js)?;
    Ok(Install {
        session,
        release: release.inner,
        stream: None,
        root: None,
        begun: false,
    })
}

#[wasm_bindgen]
impl Install {
    pub fn facts(&self) -> String {
        let f = self.session.facts();
        json!({"product": f.product, "current_slot": f.current_slot, "bootloader": f.bootloader, "baseband": f.baseband, "max_download_size": f.max_download_size}).to_string()
    }
    pub fn plan(&self) -> Vec<String> {
        self.session.plan()
    }
    pub fn log(&self) -> Vec<String> {
        self.session.log().to_vec()
    }

    /// The plan's boot image number `index`, as bytes. Checked here, again.
    pub async fn flash_boot(&mut self, index: usize, bytes: Vec<u8>) -> Result<(), JsError> {
        let write = self
            .release
            .manifest()
            .boot_writes
            .get(index)
            .ok_or_else(|| JsError::new("no such boot image in the plan"))?
            .clone();
        let image = self.release.verify_boot_image(&write, &bytes).map_err(js)?;
        self.session.flash_boot(&image).await.map_err(js)
    }

    /// `segment_cap`: the largest sparse image to hold in memory, in bytes.
    pub fn userdata_start(&mut self, proof: RootProof, segment_cap: f64) {
        let cap = (segment_cap as u64).min(self.session.max_download_size());
        self.stream = Some(UserdataStream::new(cap));
        self.root = Some(proof.inner);
        self.begun = false;
    }

    /// The next chunk of the compressed root image; flashes what is ready.
    pub async fn userdata_feed(&mut self, chunk: Vec<u8>) -> Result<f64, JsError> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| JsError::new("userdata_start first"))?;
        stream.feed(&chunk).map_err(js)?;
        if !self.begun {
            if let (Some(len), Some(root)) = (stream.image_len(), self.root.as_ref()) {
                self.session.begin_userdata(root, len).await.map_err(js)?;
                self.begun = true;
            }
        }
        while let Some(segment) = stream.next_segment().map_err(js)? {
            self.session
                .flash_userdata_segment(&segment)
                .await
                .map_err(js)?;
        }
        let (done, total) = stream.progress();
        Ok(if total == 0 {
            0.0
        } else {
            done as f64 / total as f64
        })
    }

    pub async fn userdata_end(&mut self) -> Result<(), JsError> {
        let mut stream = self
            .stream
            .take()
            .ok_or_else(|| JsError::new("userdata_start first"))?;
        stream.end().map_err(js)?;
        while let Some(segment) = stream.next_segment().map_err(js)? {
            self.session
                .flash_userdata_segment(&segment)
                .await
                .map_err(js)?;
        }
        self.session.finish_userdata().await.map_err(js)
    }

    /// Make slot B active and restart.
    pub async fn finish(&mut self) -> Result<(), JsError> {
        self.session.activate_and_reboot().await.map_err(js)
    }
}

/// Whether this build can write to a real phone.
#[wasm_bindgen]
pub fn writes_enabled() -> bool {
    cfg!(feature = "enable-writes")
}

#[cfg(feature = "selftest")]
#[path = "../../core/tests/support/fake.rs"]
mod fake;

#[cfg(feature = "selftest")]
macro_rules! fx {
    ($name:literal) => {
        include_bytes!(concat!("../../fixtures/selftest/", $name)).as_slice()
    };
}

#[cfg(feature = "selftest")]
const BUILT_IN_IMAGES: [&[u8]; 5] = [
    fx!("images/boot.img"),
    fx!("images/init_boot.img"),
    fx!("images/vendor_boot.img"),
    fx!("images/vendor_kernel_boot.img"),
    fx!("images/dtbo.img"),
];

/// The pretend phone, in one of the states the tests know
/// (`core/tests/support/fake.rs`). Self-test builds only.
#[cfg(feature = "selftest")]
#[wasm_bindgen]
pub fn fake_phone(scenario: &str) -> Result<Phone, JsError> {
    let phone =
        fake::FakePhone::scenario(scenario).ok_or_else(|| JsError::new("no such pretend phone"))?;
    Ok(Phone {
        link: Link::Fake(phone),
        serial: fake::SERIAL.into(),
    })
}

#[cfg(feature = "selftest")]
#[wasm_bindgen]
pub fn fake_phone_scenarios() -> Vec<String> {
    fake::FakePhone::SCENARIOS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(feature = "selftest")]
#[wasm_bindgen]
impl Phone {
    /// Change one of the pretend phone's fastboot variables (its pretend
    /// owner unlocked it, or swapped it for another phone).
    pub fn fake_set(&self, name: &str, value: &str) {
        if let Link::Fake(phone) = &self.link {
            phone.edit(|s| {
                s.vars.insert(name.into(), value.into());
            });
        }
    }
    /// Another pretend phone takes this one's place on the cable.
    pub fn fake_swap(&mut self) {
        self.serial = "ANOTHERPHONE0002".into();
        self.fake_set("serialno", "ANOTHERPHONE0002");
    }
    /// How the pretend phone answers `flashing get_unlock_ability` from now
    /// on: this line, whatever it says.
    pub fn fake_unlock_answer(&self, line: &str) {
        if let Link::Fake(phone) = &self.link {
            phone.edit(|s| s.unlock_ability = Some(line.into()));
        }
    }
    /// What reached the pretend phone, as JSON: how many commands, how often
    /// it was asked `flashing get_unlock_ability`, which commands were
    /// neither that nor `getvar`, and which partitions were written.
    pub fn fake_report(&self) -> String {
        let Link::Fake(phone) = &self.link else {
            return "{}".into();
        };
        let log = phone.log();
        json!({
            "commands": log.len(),
            "unlock_questions": phone.unlock_questions(),
            "not_reads": phone.not_reads(),
            "written": phone.written(),
            "active_slot": phone.0.borrow().active_slot,
        })
        .to_string()
    }
}

#[cfg(feature = "selftest")]
#[wasm_bindgen]
impl InstallFlow {
    /// A flow on the signed test bundle built into this module, trusted by
    /// its test key. `tamper`: `manifest` or `index` changes one byte of
    /// that file first, to show the refusal. `slot_override`: the
    /// maintainer's override for the bootloader-slot stop, which the page
    /// has no control for; the self-test sets it to see the page say so.
    pub fn selftest(
        reinstall: bool,
        tamper: &str,
        slot_override: bool,
    ) -> Result<InstallFlow, JsError> {
        let flip = |data: &[u8], on: bool| {
            let mut v = data.to_vec();
            if on {
                let at = v.len() / 2;
                v[at] ^= 1;
            }
            v
        };
        let manifest = flip(fx!("manifest.json"), tamper == "manifest");
        let index = flip(fx!("factory-builds.json"), tamper == "index");
        let files = ReleaseFiles {
            sha256sums: fx!("SHA256SUMS"),
            sha256sums_sig: fx!("SHA256SUMS.sig"),
            keyring: fx!("test-key.gpg"),
            manifest_json: &manifest,
            manifest_txt: fx!("MANIFEST"),
            device_conf: fx!("device.conf"),
        };
        let trusted =
            TrustedKeys::new(&[core::str::from_utf8(fx!("test-key.fingerprint")).map_err(js)?])
                .map_err(js)?;
        Ok(InstallFlow {
            flow: Flow::begin(
                &files,
                Some(&index),
                &trusted,
                FlowOptions {
                    reinstall,
                    maintainer_skip_bootloader_slot_check: slot_override,
                    ..Default::default()
                },
            ),
            check: None,
            built_in: true,
        })
    }
}

/// The stand-in for Google's factory zip that the test bundle's index names.
#[cfg(feature = "selftest")]
#[wasm_bindgen]
pub fn selftest_factory_zip() -> Vec<u8> {
    fx!("factory/cheetah-test.260101.001-factory-31c2322a.zip").to_vec()
}

/// Walk the built-in bundle's images through a dry-run session.
#[cfg(feature = "selftest")]
async fn walk_built_in<T: Transport>(
    session: &mut Session<T>,
    release: Option<&VerifiedRelease>,
) -> omamobile_installer_core::Result<()> {
    use omamobile_installer_core::install::flash_userdata_from_reader;
    let release = release.ok_or_else(|| {
        omamobile_installer_core::Error::Refused("the flow has no release".into())
    })?;
    for (write, bytes) in release.manifest().boot_writes.iter().zip(BUILT_IN_IMAGES) {
        session
            .flash_boot(&release.verify_boot_image(write, bytes)?)
            .await?;
    }
    let root_zst = fx!("root.img.zst");
    let mut check = release.root_image_check();
    check.update(root_zst);
    let root = check.finish()?;
    flash_userdata_from_reader(session, &root, root_zst, None).await?;
    session.activate_and_reboot().await
}

/// A whole install inside the browser against the tests' fake phone, with a
/// signed test bundle built in, and a zstd timing. No USB. JSON out.
#[cfg(feature = "selftest")]
#[wasm_bindgen]
pub async fn selftest() -> Result<String, JsError> {
    use fake::{FakePhone, SERIAL};
    use omamobile_installer_core::install::flash_userdata_from_reader;
    use omamobile_installer_core::zstd_push::ZstdPush;
    let files = ReleaseFiles {
        sha256sums: fx!("SHA256SUMS"),
        sha256sums_sig: fx!("SHA256SUMS.sig"),
        keyring: fx!("test-key.gpg"),
        manifest_json: fx!("manifest.json"),
        manifest_txt: fx!("MANIFEST"),
        device_conf: fx!("device.conf"),
    };
    let trusted =
        TrustedKeys::new(&[core::str::from_utf8(fx!("test-key.fingerprint")).map_err(js)?])
            .map_err(js)?;
    // The built-in fingerprints must not accept the test key
    let refused_by_release_keys =
        bundle::verify_release(&files, &TrustedKeys::omamobile()).is_err();
    let release = bundle::verify_release(&files, &trusted).map_err(js)?;
    let images = BUILT_IN_IMAGES;
    let root_zst = fx!("root.img.zst");

    let mut check = release.root_image_check();
    check.update(root_zst);
    let root = check.finish().map_err(js)?;
    let phone = FakePhone::new("cheetah");
    let options = PhoneOptions {
        bootloader_slots_confirmed: Some("fake-1.0".into()),
        ..Default::default()
    };
    let mut session = Session::connect(phone.clone(), &release, SERIAL, options)
        .await
        .map_err(js)?;
    for (write, bytes) in release.manifest().boot_writes.iter().zip(images) {
        session
            .flash_boot(&release.verify_boot_image(write, bytes).map_err(js)?)
            .await
            .map_err(js)?;
    }
    let raw_len = flash_userdata_from_reader(&mut session, &root, root_zst, None)
        .await
        .map_err(js)?;
    session.activate_and_reboot().await.map_err(js)?;

    // What the fake phone holds afterwards
    let mut raw = Vec::new();
    ZstdPush::new()
        .push(root_zst, &mut |d| {
            raw.extend_from_slice(d);
            Ok(())
        })
        .map_err(js)?;
    let userdata_ok = phone.partition("userdata")[..raw.len()] == raw[..];
    let boot_ok = phone.partition("boot_b")[..images[0].len()] == *images[0];
    let wrong_phone = Session::connect(
        FakePhone::new("panther"),
        &release,
        SERIAL,
        PhoneOptions::default(),
    )
    .await
    .is_err();

    // zstd in WebAssembly: the zstd-made fixture (3 MiB raw), many times
    let fixture = include_bytes!("../../fixtures/zstd/pattern.img.zst");
    let (rounds, mut bytes) = (40, 0u64);
    let start = js_sys::Date::now();
    for _ in 0..rounds {
        ZstdPush::new()
            .push(fixture, &mut |d| {
                bytes += d.len() as u64;
                Ok(())
            })
            .map_err(js)?;
    }
    let ms = js_sys::Date::now() - start;

    Ok(json!({
        "install_done": session.is_done(), "raw_image_bytes": raw_len, "userdata_matches": userdata_ok, "boot_b_matches": boot_ok,
        "partitions_written": phone.written(), "active_slot": phone.0.borrow().active_slot, "commands": phone.log().len(),
        "wrong_phone_refused": wrong_phone, "test_key_refused_by_release_keys": refused_by_release_keys,
        "zstd_mib": bytes >> 20, "zstd_ms": ms, "zstd_mib_per_s": (bytes as f64 / 1048576.0) / (ms / 1000.0),
    })
    .to_string())
}
