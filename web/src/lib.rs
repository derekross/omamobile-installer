// SPDX-License-Identifier: MIT
//! Spike: the installer core as a WebAssembly module for the web flasher.
//!
//! The same `omamobile-installer-core` as the CLI, and the same USB transport
//! (`omamobile-installer-usb`: nusb's WebUSB backend). This file only turns
//! the core's API into JavaScript classes; it holds no rule of its own.
//!
//! The browser's part is what only a browser can do: the device chooser
//! (`navigator.usb.requestDevice`, which needs a click), fetching files and
//! storing the download. Bytes come in as `Uint8Array`s.
//!
//! As in the CLI, a session on a real phone is a dry run unless the module is
//! built with `enable-writes`: the spike can't deliver the owner's first-boot
//! answers yet. The write path is compiled in either way (the flag is read at
//! run time), so the module's size is the real one.
use omamobile_installer_core::bundle::{
    self, ReleaseFiles, RootImageCheck, VerifiedRelease, VerifiedRootImage,
};
use omamobile_installer_core::fastboot::{
    PhoneOptions, Session, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS,
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
        let m = self.inner.manifest();
        json!({
            "version": m.version, "label": m.label, "codename": m.codename, "device": m.device_name,
            "kernel": m.kernel_release, "signed_by": self.inner.signer().primary, "signing_key": self.inner.signer().signing_key,
            "boot_images": m.boot_writes.iter().map(|w| json!({"partition": w.partition.name(), "source": w.source, "size": w.size})).collect::<Vec<_>>(),
            "root_image": {"partition": m.root_image.partition.name(), "name": m.root_image.name, "size": m.root_image.size},
        })
        .to_string()
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

/// A phone the person picked in the browser's chooser.
#[wasm_bindgen]
pub struct Phone {
    device: nusb::Device,
    serial: String,
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
    Ok(Phone { device, serial })
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

/// Open the phone and run every read-only check.
#[wasm_bindgen]
pub async fn connect(
    release: Release,
    phone: Phone,
    reinstall: bool,
    bootloader_slots_confirmed: Option<String>,
) -> Result<Install, JsError> {
    let transport = UsbTransport::open(&phone.device).await.map_err(js)?;
    let options = PhoneOptions {
        reinstall,
        bootloader_slots_confirmed,
        dry_run: !cfg!(feature = "enable-writes"),
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

/// A whole install inside the browser against the tests' fake phone, with a
/// signed test bundle built in, and a zstd timing. No USB. JSON out.
#[cfg(feature = "selftest")]
#[wasm_bindgen]
pub async fn selftest() -> Result<String, JsError> {
    use fake::{FakePhone, SERIAL};
    use omamobile_installer_core::install::flash_userdata_from_reader;
    use omamobile_installer_core::zstd_push::ZstdPush;
    macro_rules! fx {
        ($name:literal) => {
            include_bytes!(concat!("../../fixtures/selftest/", $name)).as_slice()
        };
    }
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
    let images = [
        fx!("images/boot.img"),
        fx!("images/init_boot.img"),
        fx!("images/vendor_boot.img"),
        fx!("images/vendor_kernel_boot.img"),
        fx!("images/dtbo.img"),
    ];
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
        .push(root_zst, &mut |d| Ok(raw.extend_from_slice(d)))
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
            .push(fixture, &mut |d| Ok(bytes += d.len() as u64))
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
