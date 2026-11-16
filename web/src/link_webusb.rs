// SPDX-License-Identifier: MIT
//! The install link's transport over WebUSB, for the page's preview build
//! only (cargo feature `link-preview`). It follows the approach of the web
//! write preview's fastboot transport (branch `web-write`, its
//! `web/src/webusb.rs`): `web_sys` directly, every promise of the browser
//! raced against a timer, a link that failed once is over. It moves the
//! core's frames and decides nothing.
//!
//! The plan's §5.7 in a browser: one `transferIn` is always outstanding
//! while the link is up (a loop of its own on the page's event loop, which
//! queues each completed read), so the phone's `ack` is taken while the image
//! is being sent; and a `transferOut` the phone has not taken in 30 s is a
//! lost link.
//!
//! Which device: only one the person picked in the browser's own chooser,
//! with one filter, the link's interface (class 0xff, subclass 0x6f,
//! protocol 0x01). That needs a click, so the page hands the device in
//! ([`WebUsbLink::set_device`]); until it has, `connect` says the link is
//! not there. **Never run against a real phone or a real browser's USB.**
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use omamobile_installer_core::link::{
    read_buffer_len, LinkTransport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS, WRITE_WAIT_MS,
};
use omamobile_installer_core::{Error, Result};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{UsbDevice, UsbDirection, UsbEndpointType, UsbInTransferResult, UsbTransferStatus};

fn lost(what: impl Into<String>) -> Error {
    Error::Transport(what.into())
}

/// A promise, or a timer of `ms`: `None` when the timer won.
async fn within(promise: Promise, ms: f64) -> core::result::Result<Option<JsValue>, JsValue> {
    let global = js_sys::global();
    let set: Function = Reflect::get(&global, &JsValue::from_str("setTimeout"))?.dyn_into()?;
    let ran_out = JsValue::from(Object::new());
    let marker = ran_out.clone();
    let timeout = Promise::new(&mut |resolve, _| {
        let _ = set.call3(&global, &resolve, &JsValue::from_f64(ms), &marker);
    });
    let first = JsFuture::from(Promise::race(&Array::of2(&promise, &timeout))).await?;
    Ok((!Object::is(&first, &ran_out)).then_some(first))
}

/// The link interface's number and its two bulk endpoints (out, in).
fn find_link(device: &UsbDevice) -> Option<(u8, u8, u8)> {
    let config = device.configuration()?;
    let interfaces = config.interfaces();
    for i in 0..interfaces.length() {
        let interface: web_sys::UsbInterface = interfaces.get(i).dyn_into().ok()?;
        let alternates = interface.alternates();
        for a in 0..alternates.length() {
            let alt: web_sys::UsbAlternateInterface = alternates.get(a).dyn_into().ok()?;
            if (
                alt.interface_class(),
                alt.interface_subclass(),
                alt.interface_protocol(),
            ) != (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL)
            {
                continue;
            }
            let eps = alt.endpoints();
            let mut out = None;
            let mut inp = None;
            for e in 0..eps.length() {
                let ep: web_sys::UsbEndpoint = eps.get(e).dyn_into().ok()?;
                if ep.type_() != UsbEndpointType::Bulk {
                    continue;
                }
                match ep.direction() {
                    UsbDirection::Out => out = Some(ep.endpoint_number()),
                    UsbDirection::In => inp = Some(ep.endpoint_number()),
                    _ => {}
                }
            }
            if let (Some(o), Some(i)) = (out, inp) {
                return Some((interface.interface_number(), o, i));
            }
        }
    }
    None
}

/// What the reader loop hands over.
type Inbox = Rc<RefCell<(VecDeque<Result<Vec<u8>>>, bool)>>;

#[derive(Default)]
pub struct WebUsbLink {
    device: Option<UsbDevice>,
    open: Option<(u8, Inbox)>,
}

impl WebUsbLink {
    /// The device the person picked in the browser's chooser.
    #[allow(dead_code)]
    pub fn set_device(&mut self, device: UsbDevice) {
        self.device = Some(device);
    }
}

impl LinkTransport for WebUsbLink {
    async fn connect(&mut self) -> Result<bool> {
        let Some(device) = self.device.clone() else {
            return Ok(false);
        };
        if !device.opened() && JsFuture::from(device.open()).await.is_err() {
            return Ok(false);
        }
        if device.configuration().is_none() {
            JsFuture::from(device.select_configuration(1))
                .await
                .map_err(|_| lost("selecting the configuration"))?;
        }
        let Some((number, out, inp)) = find_link(&device) else {
            return Ok(false);
        };
        JsFuture::from(device.claim_interface(number))
            .await
            .map_err(|_| lost("claiming the install link's interface"))?;
        let inbox: Inbox = Rc::new(RefCell::new((VecDeque::new(), false)));
        let reader = inbox.clone();
        let len = read_buffer_len() as u32;
        // The read that is always pending
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                if reader.borrow().1 {
                    break;
                }
                let got = JsFuture::from(device.transfer_in(inp, len)).await;
                let item = match got {
                    Ok(v) => {
                        let r: UsbInTransferResult = v.unchecked_into();
                        match (r.status(), r.data()) {
                            (UsbTransferStatus::Ok, Some(view)) => {
                                Ok(Uint8Array::new_with_byte_offset_and_length(
                                    &view.buffer(),
                                    view.byte_offset() as u32,
                                    view.byte_length() as u32,
                                )
                                .to_vec())
                            }
                            _ => Err(lost("the phone's transfer did not end well")),
                        }
                    }
                    Err(_) => Err(lost("the browser lost the device")),
                };
                let failed = item.is_err();
                reader.borrow_mut().0.push_back(item);
                if failed {
                    break;
                }
            }
        });
        self.open = Some((out, inbox));
        Ok(true)
    }

    async fn send(&mut self, frame: &[u8]) -> Result<()> {
        let (Some(device), Some((out, _))) = (self.device.clone(), self.open.as_ref()) else {
            return Err(lost("the install link is not open"));
        };
        let promise = device
            .transfer_out_with_u8_slice(*out, frame)
            .map_err(|_| lost("sending"))?;
        match within(promise.unchecked_into(), WRITE_WAIT_MS as f64).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(lost("the phone did not take a frame within 30 seconds")),
            Err(_) => Err(lost("the browser lost the device")),
        }
    }

    async fn recv(&mut self, wait_ms: u64) -> Result<Option<Vec<u8>>> {
        let Some((_, inbox)) = self.open.as_ref() else {
            return Err(lost("the install link is not open"));
        };
        let inbox = inbox.clone();
        let mut waited = 0;
        loop {
            if let Some(item) = inbox.borrow_mut().0.pop_front() {
                return item.map(Some);
            }
            if waited >= wait_ms {
                return Ok(None);
            }
            let step = (wait_ms - waited).min(10);
            let _ = within(Promise::new(&mut |_, _| {}), step as f64).await;
            waited += step;
        }
    }

    fn disconnect(&mut self) {
        if let Some((_, inbox)) = self.open.take() {
            inbox.borrow_mut().1 = true;
        }
        if let Some(device) = &self.device {
            let _ = device.close();
        }
    }
}
