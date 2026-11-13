// SPDX-License-Identifier: MIT
//! The install link's transport on a computer (nusb; Linux, Windows,
//! macOS): the phone's link interface found by its class, subclass,
//! protocol and string (INSTALL-LINK-PLAN §4), claimed, and its two bulk
//! endpoints. Nothing here decides what is sent: it moves the core's frames.
//!
//! The plan's §5.7, as this file keeps it:
//!
//! - **A read is always pending.** A thread of its own owns the IN endpoint
//!   and keeps one transfer submitted for as long as the link is up, also
//!   while a frame is being written, and hands each completed read over a
//!   channel. So the phone's `ack` is taken while the installer sends the
//!   image, and neither side waits on the other's write.
//! - **One frame per read.** The buffer holds the largest frame of the
//!   session (`frame::read_buffer_len`), so a frame, which ends in a short
//!   packet by the pad rule, completes one read.
//! - **30 s per write.** A write the phone has not taken by then is
//!   cancelled and reported as a lost link; the core says `hello` again.
//!
//! nusb's calls that block (listing, opening, claiming) are waited for in
//! place with `MaybeFuture::wait`: the tool has no async runtime.
use nusb::transfer::{Bulk, Direction, In, Out};
use nusb::{Endpoint, Interface, MaybeFuture};
use omamobile_installer_core::link::{
    read_buffer_len, LinkTransport, USB_CLASS, USB_INTERFACE, USB_PROTOCOL, USB_SUBCLASS,
    WRITE_WAIT_MS,
};
use omamobile_installer_core::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

fn usb<E: core::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> Error {
    move |e| Error::Transport(format!("{what}: {e}"))
}

struct Open {
    ep_out: Endpoint<Bulk, Out>,
    frames: Receiver<Result<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    _interface: Interface,
}

impl Drop for Open {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.ep_out.cancel_all();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

/// The phone's install link over USB, looked for until it is there.
#[derive(Default)]
pub struct UsbLink {
    open: Option<Open>,
}

/// Is this one of the link's interfaces? The string as the system cached
/// it, or asked of the device.
fn is_link(iface: &nusb::InterfaceInfo) -> bool {
    if (iface.class(), iface.subclass(), iface.protocol())
        != (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL)
    {
        return false;
    }
    let named = match iface.interface_string() {
        Some(s) => Some(s.to_string()),
        None => iface.get_interface_string().wait().ok().flatten(),
    };
    named.as_deref() == Some(USB_INTERFACE)
}

impl UsbLink {
    fn open(info: &nusb::DeviceInfo, number: u8) -> Result<Open> {
        let device = info.open().wait().map_err(usb("opening the phone"))?;
        let config = device
            .active_configuration()
            .map_err(usb("reading the configuration"))?;
        let mut eps = None;
        for alt in config.interface_alt_settings() {
            if alt.interface_number() != number || alt.alternate_setting() != 0 {
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
            if let (Some(o), Some(i)) = (bulk(Direction::Out), bulk(Direction::In)) {
                eps = Some((o, i));
            }
        }
        let (out, inp) = eps.ok_or_else(|| {
            Error::Transport("the install link's interface has no pair of bulk endpoints".into())
        })?;
        let interface = device
            .claim_interface(number)
            .wait()
            .map_err(usb("claiming the install link's interface"))?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(out)
            .map_err(usb("opening the OUT endpoint"))?;
        let mut ep_in = interface
            .endpoint::<Bulk, In>(inp)
            .map_err(usb("opening the IN endpoint"))?;
        let (tx, frames) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let reader = std::thread::spawn(move || {
            let packet = ep_in.max_packet_size().max(1);
            let len = read_buffer_len().div_ceil(packet) * packet;
            while !stopping.load(Ordering::SeqCst) {
                if ep_in.pending() == 0 {
                    let buffer = ep_in.allocate(len);
                    ep_in.submit(buffer);
                }
                // The transfer stays pending past this wait: it is only
                // how often the thread looks at the stop flag
                let Some(done) = ep_in.wait_next_complete(Duration::from_millis(250)) else {
                    continue;
                };
                let got = match done.status {
                    Ok(()) => Ok(done.buffer[..done.actual_len].to_vec()),
                    Err(e) => Err(Error::Transport(format!("receiving: {e}"))),
                };
                let failed = got.is_err();
                if tx.send(got).is_err() || failed {
                    break;
                }
            }
            ep_in.cancel_all();
        });
        Ok(Open {
            ep_out,
            frames,
            stop,
            reader: Some(reader),
            _interface: interface,
        })
    }
}

impl LinkTransport for UsbLink {
    async fn connect(&mut self) -> Result<bool> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(usb("listing USB devices"))?;
        for info in devices {
            let found = info
                .interfaces()
                .find(|i| is_link(i))
                .map(|i| i.interface_number());
            if let Some(number) = found {
                self.open = Some(Self::open(&info, number)?);
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn send(&mut self, frame: &[u8]) -> Result<()> {
        let open = self
            .open
            .as_mut()
            .ok_or_else(|| Error::Transport("the install link is not open".into()))?;
        open.ep_out.submit(frame.to_vec().into());
        match open
            .ep_out
            .wait_next_complete(Duration::from_millis(WRITE_WAIT_MS))
        {
            Some(done) => done.status.map_err(usb("sending")),
            None => {
                open.ep_out.cancel_all();
                Err(Error::Transport(
                    "the phone did not take a frame within 30 seconds".into(),
                ))
            }
        }
    }

    async fn recv(&mut self, wait_ms: u64) -> Result<Option<Vec<u8>>> {
        let open = self
            .open
            .as_mut()
            .ok_or_else(|| Error::Transport("the install link is not open".into()))?;
        match open.frames.recv_timeout(Duration::from_millis(wait_ms)) {
            Ok(got) => got.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(Error::Transport("the install link's reader ended".into()))
            }
        }
    }

    fn disconnect(&mut self) {
        self.open = None;
    }
}
