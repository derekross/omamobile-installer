// SPDX-License-Identifier: MIT
//! FunctionFS, the link's transport on the phone (INSTALL-LINK-PLAN §4,
//! stage L3): the descriptors and strings the daemon writes to `ep0`, the
//! events it reads from there, and the endpoint files.
//!
//! The layout of every blob is upstream Linux's
//! (`include/uapi/linux/usb/functionfs.h`, `Documentation/usb/functionfs.rst`
//! and `functionfs-desc.rst`); nothing here is the phone's.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The interface (§4): vendor class, the link's subclass and protocol.
pub const CLASS: u8 = 0xff;
pub const SUBCLASS: u8 = 0x6f;
pub const PROTOCOL: u8 = 0x01;
/// `iInterface` (§4). The host finds the link by this and the three numbers.
pub const INTERFACE_STRING: &str = "Omarchy Mobile install link";
/// The bulk endpoints as declared; FunctionFS gives them their real
/// numbers. `ep1` is the first declared (OUT), `ep2` the second (IN).
pub const EP_OUT: u8 = 0x01;
pub const EP_IN: u8 = 0x82;
pub const EP_OUT_FILE: &str = "ep1";
pub const EP_IN_FILE: &str = "ep2";
/// `wMaxPacketSize` at full, high and super speed (§4).
pub const PACKET: [u16; 3] = [64, 512, 1024];
/// The Microsoft OS 1.0 compatible ID that makes Windows bind WinUSB (§4).
pub const COMPATIBLE_ID: &[u8; 8] = b"WINUSB\0\0";

const DESCRIPTORS_MAGIC_V2: u32 = 3;
const STRINGS_MAGIC: u32 = 2;
const HAS_FS_DESC: u32 = 1;
const HAS_HS_DESC: u32 = 2;
const HAS_SS_DESC: u32 = 4;
const HAS_MS_OS_DESC: u32 = 8;
const LANG_EN_US: u16 = 0x0409;

fn interface(out: &mut Vec<u8>) {
    // bLength, INTERFACE, bInterfaceNumber, bAlternateSetting,
    // bNumEndpoints, class, subclass, protocol, iInterface
    out.extend_from_slice(&[9, 4, 0, 0, 2, CLASS, SUBCLASS, PROTOCOL, 1]);
}

fn endpoint(out: &mut Vec<u8>, address: u8, packet: u16) {
    // bLength, ENDPOINT, bEndpointAddress, bmAttributes (bulk),
    // wMaxPacketSize, bInterval
    out.extend_from_slice(&[7, 5, address, 2]);
    out.extend_from_slice(&packet.to_le_bytes());
    out.push(0);
}

fn companion(out: &mut Vec<u8>) {
    // bLength, SS_ENDPOINT_COMP, bMaxBurst, bmAttributes, wBytesPerInterval
    out.extend_from_slice(&[6, 0x30, 0, 0, 0, 0]);
}

/// The descriptors in FunctionFS's v2 format: one interface with a bulk OUT
/// and a bulk IN endpoint at each of the three speeds (SuperSpeed with its
/// companions), and the extended compatible ID `WINUSB` for interface 0.
pub fn descriptors() -> Vec<u8> {
    let mut fs = Vec::new();
    interface(&mut fs);
    endpoint(&mut fs, EP_OUT, PACKET[0]);
    endpoint(&mut fs, EP_IN, PACKET[0]);
    let mut hs = Vec::new();
    interface(&mut hs);
    endpoint(&mut hs, EP_OUT, PACKET[1]);
    endpoint(&mut hs, EP_IN, PACKET[1]);
    let mut ss = Vec::new();
    interface(&mut ss);
    endpoint(&mut ss, EP_OUT, PACKET[2]);
    companion(&mut ss);
    endpoint(&mut ss, EP_IN, PACKET[2]);
    companion(&mut ss);
    // One feature descriptor: the extended compatible ID (wIndex 4), with
    // one function section
    let mut os = Vec::new();
    os.push(0); // the interface the header is for
    os.extend_from_slice(&(11u32 + 24).to_le_bytes()); // dwLength
    os.extend_from_slice(&0x0100u16.to_le_bytes()); // bcdVersion
    os.extend_from_slice(&4u16.to_le_bytes()); // wIndex: extended compat ID
    os.extend_from_slice(&[1, 0]); // bCount, reserved
    os.extend_from_slice(&[0, 1]); // bFirstInterfaceNumber, reserved (1)
    os.extend_from_slice(COMPATIBLE_ID);
    os.extend_from_slice(&[0; 8]); // SubCompatibleID
    os.extend_from_slice(&[0; 6]);

    let counts = [3u32, 3, 5, 1];
    let mut out = Vec::new();
    out.extend_from_slice(&DESCRIPTORS_MAGIC_V2.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // length, below
    let flags = HAS_FS_DESC | HAS_HS_DESC | HAS_SS_DESC | HAS_MS_OS_DESC;
    out.extend_from_slice(&flags.to_le_bytes());
    for c in counts {
        out.extend_from_slice(&c.to_le_bytes());
    }
    for part in [&fs, &hs, &ss, &os] {
        out.extend_from_slice(part);
    }
    let len = out.len() as u32;
    out[4..8].copy_from_slice(&len.to_le_bytes());
    out
}

/// The one string, `iInterface`, in US English.
pub fn strings() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&STRINGS_MAGIC.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // str_count
    out.extend_from_slice(&1u32.to_le_bytes()); // lang_count
    out.extend_from_slice(&LANG_EN_US.to_le_bytes());
    out.extend_from_slice(INTERFACE_STRING.as_bytes());
    out.push(0);
    let len = out.len() as u32;
    out[4..8].copy_from_slice(&len.to_le_bytes());
    out
}

/// One event from `ep0` (`struct usb_functionfs_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Bind,
    Unbind,
    Enable,
    Disable,
    /// A control request for the function. The link answers none: each is
    /// stalled.
    Setup {
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    },
    Suspend,
    Resume,
    Unknown(u8),
}

impl Event {
    /// The name the journal gives it: FunctionFS's own.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Bind => "BIND",
            Event::Unbind => "UNBIND",
            Event::Enable => "ENABLE",
            Event::Disable => "DISABLE",
            Event::Setup { .. } => "SETUP",
            Event::Suspend => "SUSPEND",
            Event::Resume => "RESUME",
            Event::Unknown(_) => "UNKNOWN",
        }
    }
}

/// `sizeof(struct usb_functionfs_event)`.
pub const EVENT_LEN: usize = 12;

/// The events in what one read of `ep0` returned: whole events only.
pub fn parse_events(buf: &[u8]) -> Vec<Event> {
    buf.as_chunks::<EVENT_LEN>()
        .0
        .iter()
        .map(|e| match e[8] {
            0 => Event::Bind,
            1 => Event::Unbind,
            2 => Event::Enable,
            3 => Event::Disable,
            4 => Event::Setup {
                request_type: e[0],
                request: e[1],
                value: u16::from_le_bytes([e[2], e[3]]),
                index: u16::from_le_bytes([e[4], e[5]]),
                length: u16::from_le_bytes([e[6], e[7]]),
            },
            5 => Event::Suspend,
            6 => Event::Resume,
            other => Event::Unknown(other),
        })
        .collect()
}

/// The function's files, opened: `ep0` with the descriptors and strings
/// written to it, and the two bulk endpoints. Dropping them closes the
/// function: FunctionFS then takes the gadget off the bus (upstream
/// `ffs_closed()` unbinds it) unless the mount said `no_disconnect`.
pub struct Function {
    pub ep0: File,
    pub out: File,
    pub inp: File,
}

/// Open `ep0` of the FunctionFS mounted at `dir` and write the descriptors
/// and the strings. The endpoint files appear when this returns.
pub fn open_ep0(dir: &Path) -> io::Result<File> {
    let mut ep0 = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("ep0"))?;
    ep0.write_all(&descriptors())?;
    ep0.write_all(&strings())?;
    Ok(ep0)
}

/// The two bulk endpoints. Non-blocking: a request on an endpoint that is
/// not enabled fails at once (`EAGAIN`) rather than waiting for `ENABLE`
/// inside `io_submit`, where no time limit reaches it.
pub fn open_endpoints(dir: &Path, ep0: File) -> io::Result<Function> {
    let open = |name: &str, write: bool| {
        OpenOptions::new()
            .read(!write)
            .write(write)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(dir.join(name))
    };
    Ok(Function {
        out: open(EP_OUT_FILE, false)?,
        inp: open(EP_IN_FILE, true)?,
        ep0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptors_are_functionfs_v2_with_three_speeds_and_the_os_descriptor() {
        let d = descriptors();
        assert_eq!(u32::from_le_bytes(d[0..4].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize,
            d.len()
        );
        assert_eq!(u32::from_le_bytes(d[8..12].try_into().unwrap()), 0b1111);
        // counts: full 3, high 3, super 5, OS 1
        let counts: Vec<u32> = (0..4)
            .map(|i| u32::from_le_bytes(d[12 + 4 * i..16 + 4 * i].try_into().unwrap()))
            .collect();
        assert_eq!(counts, [3, 3, 5, 1]);
        // Walk the descriptors by their bLength: 3 + 3 + 5 of them, each of
        // the kind and size the count says
        let mut at = 28;
        let mut packets = Vec::new();
        for (speed, n) in [(0, 3), (1, 3), (2, 5)] {
            for _ in 0..n {
                let (len, kind) = (d[at] as usize, d[at + 1]);
                match kind {
                    4 => assert_eq!(&d[at + 5..at + 8], &[CLASS, SUBCLASS, PROTOCOL]),
                    5 => {
                        packets.push((speed, d[at + 2], u16::from_le_bytes([d[at + 4], d[at + 5]])))
                    }
                    0x30 => assert_eq!(len, 6),
                    other => panic!("descriptor type {other}"),
                }
                at += len;
            }
        }
        assert_eq!(
            packets,
            [
                (0, EP_OUT, 64),
                (0, EP_IN, 64),
                (1, EP_OUT, 512),
                (1, EP_IN, 512),
                (2, EP_OUT, 1024),
                (2, EP_IN, 1024)
            ]
        );
        // The OS descriptor: 11 bytes of header, 24 of the compatible ID
        assert_eq!(d.len() - at, 35);
        assert_eq!(&d[at + 5..at + 9], &[0x00, 0x01, 4, 0]);
        assert_eq!(&d[at + 13..at + 21], COMPATIBLE_ID);
    }

    #[test]
    fn the_strings_carry_the_interface_name() {
        let s = strings();
        assert_eq!(&s[0..4], &2u32.to_le_bytes());
        assert_eq!(
            u32::from_le_bytes(s[4..8].try_into().unwrap()) as usize,
            s.len()
        );
        assert_eq!(&s[16..18], &0x0409u16.to_le_bytes());
        assert_eq!(&s[18..s.len() - 1], INTERFACE_STRING.as_bytes());
        assert_eq!(s[s.len() - 1], 0);
    }

    #[test]
    fn events_are_read_twelve_bytes_at_a_time() {
        let mut buf = Vec::new();
        for t in [2u8, 3, 5, 6, 1, 0, 9] {
            let mut e = [0u8; EVENT_LEN];
            e[8] = t;
            buf.extend_from_slice(&e);
        }
        let mut setup = [0xc0u8, 0xcd, 0x00, 0x00, 0x04, 0x00, 0x10, 0x00, 4, 0, 0, 0];
        buf.extend_from_slice(&setup);
        setup[8] = 2;
        buf.extend_from_slice(&setup[..7]); // a part event is not one
        assert_eq!(
            parse_events(&buf),
            [
                Event::Enable,
                Event::Disable,
                Event::Suspend,
                Event::Resume,
                Event::Unbind,
                Event::Bind,
                Event::Unknown(9),
                Event::Setup {
                    request_type: 0xc0,
                    request: 0xcd,
                    value: 0,
                    index: 4,
                    length: 16
                }
            ]
        );
    }
}
