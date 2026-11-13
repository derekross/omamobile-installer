// SPDX-License-Identifier: MIT
//! The page's own fastboot transport over WebUSB, for the write preview
//! (cargo feature `web-write-preview`; never in the page as it ships).
//!
//! It implements the core's [`Transport`], the same two functions the
//! command-line tool's USB transport implements
//! (`omamobile-installer-usb`), and nothing else. It decides nothing about
//! what is sent: a command or a piece of a download arrives here only from
//! the core's command layer, after its gate, and one call of `send` is one
//! `transferOut` of exactly the bytes it was handed. The size of a download
//! and of each piece of it are the core's (`fastboot::SEND_CHUNK`, the
//! session's `max-download-size`, `install::SEGMENT_CAP`); this file has no
//! number of its own but the 256 bytes a fastboot answer is at most.
//!
//! Why the page does not write through the transport it reads with
//! (nusb's WebUSB backend): that one has no wait in a browser, so a phone
//! that stops answering would hold an install for good, with `userdata`
//! half written and no stop on the screen. Here every promise the browser
//! gives is raced against the wait the core sets for an answer
//! (`sparse::FLASH_WAIT_SECONDS`), and a wait that runs out, a transfer the
//! browser rejects (the cable was pulled) and a transfer that ends `stall`
//! or `babble` each come back as the core's `Error::Transport`, which the
//! flow turns into its stops. A link that failed once is over: nothing more
//! is handed to the device through it.
//!
//! Which device: only the one the person picked in the browser's own
//! chooser, shown with one filter, the fastboot interface (class 0xff,
//! subclass 0x42, protocol 0x03). The page lists no devices and remembers
//! none (`getDevices` is not called). The serial number the browser reports
//! for the picked device is what the core pins the install to: the phone
//! must answer `getvar:serialno` with the same one.
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use omamobile_installer_core::fastboot::{Transport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS};
use omamobile_installer_core::sparse::FLASH_WAIT_SECONDS;
use omamobile_installer_core::{Error, Result};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    UsbDevice, UsbDirection, UsbEndpointType, UsbInTransferResult, UsbOutTransferResult,
    UsbTransferStatus,
};

/// A fastboot answer is at most this many bytes (AOSP's fastboot README).
const ANSWER_MAX: u32 = 256;

fn lost(what: impl Into<String>) -> Error {
    Error::Transport(what.into())
}

/// A browser's error in words: its name and message (`NetworkError: The
/// device was disconnected.`), or whatever it prints as.
fn describe(error: &JsValue) -> String {
    let field = |name: &str| {
        Reflect::get(error, &JsValue::from_str(name))
            .ok()
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty())
    };
    match (field("name"), field("message")) {
        (Some(name), Some(message)) => format!("{name}: {message}"),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => error
            .as_string()
            .unwrap_or_else(|| "the browser gave no reason".into()),
    }
}

/// How one promise of the browser's ended.
enum Waited {
    Answer(JsValue),
    Rejected(JsValue),
    /// Nothing came within the core's wait.
    TimedOut,
}

/// Wait for `promise`, but no longer than the core's wait for an answer.
/// The timer is the environment's `setTimeout`; without one nothing is
/// waited for, because a wait that cannot end is what this file is for.
async fn within(promise: JsValue) -> Result<Waited> {
    let global = js_sys::global();
    let timer = |name: &str| {
        Reflect::get(&global, &JsValue::from_str(name))
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
            .ok_or_else(|| lost("this browser has no timer, so a wait could not be ended"))
    };
    let (set, clear) = (timer("setTimeout")?, timer("clearTimeout")?);
    // What the timer resolves with: an object nothing else holds
    let ran_out = JsValue::from(Object::new());
    let ms = JsValue::from_f64(FLASH_WAIT_SECONDS as f64 * 1000.0);
    let mut id = JsValue::UNDEFINED;
    let timeout = Promise::new(&mut |resolve, _reject| {
        id = set
            .call3(&global, &resolve, &ms, &ran_out)
            .unwrap_or(JsValue::UNDEFINED);
    });
    let first = Promise::race(&Array::of2(&promise, &timeout));
    let ended = JsFuture::from(first).await;
    let _ = clear.call1(&global, &id);
    Ok(match ended {
        Ok(value) if Object::is(&value, &ran_out) => Waited::TimedOut,
        Ok(value) => Waited::Answer(value),
        Err(error) => Waited::Rejected(error),
    })
}

/// One of the steps that open the device, to its end or the core's wait.
async fn settle(promise: JsValue, what: &str) -> Result<()> {
    match within(promise).await? {
        Waited::Answer(_) => Ok(()),
        Waited::Rejected(e) => Err(lost(format!("{what}: {}", describe(&e)))),
        Waited::TimedOut => Err(lost(format!("{what}: timed out"))),
    }
}

/// The fastboot interface of a device: its number, whether this page
/// already claimed it, its bulk endpoints (out, in) and the IN endpoint's
/// packet size.
struct Found {
    interface: u8,
    claimed: bool,
    out: u8,
    inp: u8,
    in_packet: u32,
}

fn find_fastboot(device: &UsbDevice) -> Result<Found> {
    let config = device
        .configuration()
        .ok_or_else(|| lost("reading the configuration: the device has none selected"))?;
    let interfaces = config.interfaces();
    for i in 0..interfaces.length() {
        let interface = interfaces.get(i);
        let alternates = interface.alternates();
        for a in 0..alternates.length() {
            let alt = alternates.get(a);
            if (
                alt.interface_class(),
                alt.interface_subclass(),
                alt.interface_protocol(),
            ) != (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL)
                || alt.alternate_setting() != 0
            {
                continue;
            }
            let endpoints = alt.endpoints();
            let bulk = |direction: UsbDirection| {
                (0..endpoints.length())
                    .map(|e| endpoints.get(e))
                    .find(|e| e.type_() == UsbEndpointType::Bulk && e.direction() == direction)
            };
            if let (Some(out), Some(inp)) = (bulk(UsbDirection::Out), bulk(UsbDirection::In)) {
                return Ok(Found {
                    interface: interface.interface_number(),
                    claimed: interface.claimed(),
                    out: out.endpoint_number(),
                    inp: inp.endpoint_number(),
                    in_packet: inp.packet_size(),
                });
            }
        }
    }
    Err(lost(
        "the device has no fastboot interface (is the phone in fastboot mode?)",
    ))
}

/// The browser's chooser, with the one filter: the fastboot interface. The
/// device the person picked and the serial number the browser reports for
/// it. Must be called from a click.
pub(crate) async fn choose() -> core::result::Result<(UsbDevice, String), JsError> {
    // `navigator.usb`, from whatever global there is: a window, or the
    // tests' stand-in for one
    let usb = Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))
        .and_then(|navigator| Reflect::get(&navigator, &JsValue::from_str("usb")))
        .unwrap_or(JsValue::UNDEFINED);
    if usb.is_undefined() || usb.is_null() {
        return Err(JsError::new(
            "this browser has no WebUSB (a Chromium-based browser is needed)",
        ));
    }
    let usb: web_sys::Usb = usb.unchecked_into();
    let filter = web_sys::UsbDeviceFilter::new();
    filter.set_class_code(USB_CLASS);
    filter.set_subclass_code(USB_SUBCLASS);
    filter.set_protocol_code(USB_PROTOCOL);
    let options = web_sys::UsbDeviceRequestOptions::new(&[filter]);
    let picked = JsFuture::from(Promise::from(JsValue::from(usb.request_device(&options))))
        .await
        .map_err(|_| JsError::new("no phone was chosen"))?;
    if !picked.is_object() {
        return Err(JsError::new("not a USB device"));
    }
    let device: UsbDevice = picked.unchecked_into();
    let serial = device.serial_number().unwrap_or_default();
    Ok((device, serial))
}

/// The picked device, opened, as the page's transport for a phone.
pub(crate) async fn open(device: &UsbDevice) -> core::result::Result<super::AnyTransport, JsError> {
    WebUsbTransport::open(device)
        .await
        .map(super::AnyTransport::WebUsb)
        .map_err(super::js)
}

pub(crate) struct WebUsbTransport {
    device: UsbDevice,
    out: u8,
    inp: u8,
    /// What one IN request asks for: a whole number of packets, at least
    /// one answer's worth.
    in_len: u32,
    /// Why this link is over. Once set, nothing more is handed to the device.
    over: Option<String>,
}

impl WebUsbTransport {
    /// Open the picked device, if it is not open, and claim its fastboot
    /// interface, if this page has not claimed it already.
    async fn open(device: &UsbDevice) -> Result<Self> {
        if !device.opened() {
            settle(device.open().into(), "opening the phone").await?;
        }
        if device.configuration().is_none() {
            settle(
                device.select_configuration(1).into(),
                "selecting the phone's configuration",
            )
            .await?;
        }
        let found = find_fastboot(device)?;
        if !found.claimed {
            settle(
                device.claim_interface(found.interface).into(),
                "claiming the fastboot interface",
            )
            .await?;
        }
        let packet = found.in_packet.max(1);
        Ok(Self {
            device: device.clone(),
            out: found.out,
            inp: found.inp,
            in_len: ANSWER_MAX.div_ceil(packet) * packet,
            over: None,
        })
    }

    fn alive(&self) -> Result<()> {
        match &self.over {
            Some(why) => Err(lost(format!(
                "the connection to the phone ended earlier ({why})"
            ))),
            None => Ok(()),
        }
    }

    fn end<V>(&mut self, why: String) -> Result<V> {
        self.over = Some(why.clone());
        Err(lost(why))
    }

    /// A wait ran out with a transfer still pending. WebUSB cannot cancel
    /// one transfer; closing the device ends them all. Not waited for: a
    /// device that answers nothing may not answer this either.
    fn abandon(&self) {
        let closing = Promise::from(JsValue::from(self.device.close()));
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(closing).await;
        });
    }
}

fn status_word(status: UsbTransferStatus) -> &'static str {
    match status {
        UsbTransferStatus::Stall => "stall",
        UsbTransferStatus::Babble => "babble",
        _ => "an unknown status",
    }
}

impl Transport for WebUsbTransport {
    async fn send(&mut self, data: &[u8]) -> Result<usize> {
        self.alive()?;
        // A copy the browser keeps for the transfer: a view into the wasm
        // memory would be detached if the memory grew meanwhile
        let bytes = Uint8Array::from(data);
        let transfer = match self.device.transfer_out_with_u8_array(self.out, &bytes) {
            Ok(promise) => JsValue::from(promise),
            Err(e) => return self.end(format!("sending: {}", describe(&e))),
        };
        match within(transfer).await? {
            Waited::Answer(result) => {
                let result: UsbOutTransferResult = result.unchecked_into();
                match result.status() {
                    // Fewer bytes than were handed over is the core's to
                    // judge: it knows what they were
                    UsbTransferStatus::Ok => Ok(result.bytes_written() as usize),
                    other => self.end(format!(
                        "sending: the transfer ended with {}",
                        status_word(other)
                    )),
                }
            }
            Waited::Rejected(e) => self.end(format!("sending: {}", describe(&e))),
            Waited::TimedOut => {
                self.abandon();
                self.end("timed out sending to the phone".into())
            }
        }
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        self.alive()?;
        let transfer = JsValue::from(self.device.transfer_in(self.inp, self.in_len));
        match within(transfer).await? {
            Waited::Answer(result) => {
                let result: UsbInTransferResult = result.unchecked_into();
                match result.status() {
                    UsbTransferStatus::Ok => Ok(result.data().map_or_else(Vec::new, |view| {
                        Uint8Array::new_with_byte_offset_and_length(
                            &view.buffer(),
                            view.byte_offset() as u32,
                            view.byte_length() as u32,
                        )
                        .to_vec()
                    })),
                    other => self.end(format!(
                        "receiving: the transfer ended with {}",
                        status_word(other)
                    )),
                }
            }
            Waited::Rejected(e) => self.end(format!("receiving: {}", describe(&e))),
            Waited::TimedOut => {
                self.abandon();
                self.end("timed out waiting for the phone".into())
            }
        }
    }
}
