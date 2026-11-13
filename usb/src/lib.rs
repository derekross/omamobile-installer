// SPDX-License-Identifier: MIT
//! The fastboot transport over USB with nusb (Apache-2.0 OR MIT, pure Rust):
//! usbfs on Linux, WinUSB on Windows, IOKit on macOS and WebUSB in a browser,
//! behind one API. This file is the whole transport for every platform; only
//! how a device is found differs (enumeration natively, the browser's chooser
//! on the web), and that is the front end's part.
//!
//! Nothing here decides what is sent: it moves the core's bytes.
#[cfg(not(target_arch = "wasm32"))]
mod link;
#[cfg(not(target_arch = "wasm32"))]
pub use link::UsbLink;

use nusb::transfer::{Bulk, Direction, In, Out};
use nusb::{Device, Endpoint, Interface};
use omamobile_installer_core::fastboot::{Transport, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS};
use omamobile_installer_core::{Error, Result};

fn usb<E: core::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> Error {
    move |e| Error::Transport(format!("{what}: {e}"))
}

pub struct UsbTransport {
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    /// Keeps the claim for as long as the endpoints are used.
    _interface: Interface,
}

/// The numbers of the fastboot interface and its bulk endpoints (out, in).
fn find_fastboot(device: &Device) -> Result<(u8, u8, u8)> {
    let config = device
        .active_configuration()
        .map_err(usb("reading the configuration"))?;
    for alt in config.interface_alt_settings() {
        if (alt.class(), alt.subclass(), alt.protocol()) != (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL)
            || alt.alternate_setting() != 0
        {
            continue;
        }
        let bulk = |dir: Direction| {
            alt.endpoints()
                .find(|e| {
                    e.transfer_type() == nusb::descriptors::TransferType::Bulk
                        && e.direction() == dir
                })
                .map(|e| e.address())
        };
        if let (Some(out), Some(inp)) = (bulk(Direction::Out), bulk(Direction::In)) {
            return Ok((alt.interface_number(), out, inp));
        }
    }
    Err(Error::Transport(
        "the device has no fastboot interface (is the phone in fastboot mode?)".into(),
    ))
}

impl UsbTransport {
    /// Claim the fastboot interface of an opened device.
    pub async fn open(device: &Device) -> Result<Self> {
        let (number, out, inp) = find_fastboot(device)?;
        // Claiming blocks on a syscall. nusb runs that on an async runtime's
        // blocking pool when awaited, and the tool has no runtime (the
        // futures are driven by pollster), so on a computer it is waited
        // for in place; the browser has no blocking call and awaits.
        #[cfg(not(target_arch = "wasm32"))]
        let claimed = nusb::MaybeFuture::wait(device.claim_interface(number));
        #[cfg(target_arch = "wasm32")]
        let claimed = device.claim_interface(number).await;
        let interface = claimed.map_err(usb("claiming the fastboot interface"))?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(out)
            .map_err(usb("opening the OUT endpoint"))?;
        let ep_in = interface
            .endpoint::<Bulk, In>(inp)
            .map_err(usb("opening the IN endpoint"))?;
        Ok(Self {
            ep_out,
            ep_in,
            _interface: interface,
        })
    }
}

/// How long the phone may take to answer. A flash of one sparse image is
/// the slow case, and the core bounds it: no piece covers more than
/// `sparse::MAX_PIECE_BLOCKS` blocks (256 MiB), which a phone writing at
/// `sparse::MIN_WRITE_KIB_PER_SECOND` or faster finishes inside this wait.
/// The number is the core's, so the two cannot drift apart.
#[cfg(not(target_arch = "wasm32"))]
const TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(omamobile_installer_core::sparse::FLASH_WAIT_SECONDS);

impl Transport for UsbTransport {
    async fn send(&mut self, data: &[u8]) -> Result<usize> {
        self.ep_out.submit(data.to_vec().into());
        #[cfg(not(target_arch = "wasm32"))]
        let done = match self.ep_out.wait_next_complete(TIMEOUT) {
            Some(done) => done,
            None => {
                self.ep_out.cancel_all();
                return Err(Error::Transport("timed out sending to the phone".into()));
            }
        };
        // A browser has no blocking wait; a timeout there is the page's (it
        // can drop the future and close the device).
        #[cfg(target_arch = "wasm32")]
        let done = self.ep_out.next_complete().await;
        done.status.map_err(usb("sending"))?;
        Ok(done.actual_len)
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        // A fastboot answer is at most 256 bytes; an IN request must be a
        // multiple of the packet size.
        let packet = self.ep_in.max_packet_size().max(1);
        let len = 256usize.div_ceil(packet) * packet;
        let buffer = self.ep_in.allocate(len);
        self.ep_in.submit(buffer);
        #[cfg(not(target_arch = "wasm32"))]
        let done = match self.ep_in.wait_next_complete(TIMEOUT) {
            Some(done) => done,
            None => {
                self.ep_in.cancel_all();
                return Err(Error::Transport("timed out waiting for the phone".into()));
            }
        };
        #[cfg(target_arch = "wasm32")]
        let done = self.ep_in.next_complete().await;
        done.status.map_err(usb("receiving"))?;
        Ok(done.buffer[..done.actual_len].to_vec())
    }
}
