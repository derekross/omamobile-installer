// SPDX-License-Identifier: MIT
//! Framing, byte for byte as INSTALL-LINK-PLAN §5.1 lays it out.
//!
//! One frame is one USB bulk transfer: a 56-byte header, the payload, and at
//! most one pad byte. All integers are little-endian.
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 4 | magic `4f 4d 49 4c` ("OMIL") |
//! | 4 | 1 | frame version, 1 |
//! | 5 | 1 | kind: 1 message (JSON), 2 data (raw bytes) |
//! | 6 | 2 | flags: bit 0 = a pad byte follows the payload |
//! | 8 | 4 | seq |
//! | 12 | 4 | len, the payload's bytes |
//! | 16 | 8 | offset (data), 0 (message) |
//! | 24 | 32 | SHA-256 of the payload |
//!
//! [`decode`] takes any bytes and never panics: every wrong frame is a
//! [`FrameError`] that names what is wrong.
use crate::digest::{ct_eq, sha256};
use core::fmt;

/// "OMIL".
pub const MAGIC: [u8; 4] = [0x4f, 0x4d, 0x49, 0x4c];
/// The frame version this crate writes and reads.
pub const FRAME_VERSION: u8 = 1;
/// The header's length.
pub const HEADER_LEN: usize = 56;
/// A message's payload is at most this long.
pub const MAX_MESSAGE: usize = 262_144;
/// The most a side may offer as `max_data`.
pub const MAX_DATA_LIMIT: u32 = 4_194_304;
/// The least a side may offer as `max_data` (this crate's choice, LINK.md D7).
pub const MIN_MAX_DATA: u32 = 1024;
/// The phone's `max_data` unless its shell says otherwise.
pub const DEFAULT_MAX_DATA: u32 = 1_048_576;
/// Flag bit 0: one zero byte follows the payload.
pub const FLAG_PAD: u16 = 1;

/// What a frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One JSON object (§5.2).
    Message,
    /// Raw bytes of the open stream.
    Data,
}

impl Kind {
    pub fn byte(self) -> u8 {
        match self {
            Kind::Message => 1,
            Kind::Data => 2,
        }
    }
}

/// A decoded frame, borrowing its payload from the bytes that arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub kind: Kind,
    pub seq: u32,
    /// Data: the payload's position in the open stream. Message: 0.
    pub offset: u64,
    pub payload: &'a [u8],
}

/// What is wrong with a frame. The first eleven come from the bytes alone
/// ([`decode`]); the rest from where the session is (the state machines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than a header.
    Short { got: usize },
    /// The first four bytes are not "OMIL".
    Magic,
    /// A frame version this crate does not read.
    Version(u8),
    /// A kind that is neither message nor data.
    Kind(u8),
    /// A reserved flag bit is set.
    Flags(u16),
    /// `len` is over the limit for the kind.
    TooLong { kind: Kind, len: u32, max: u32 },
    /// `len` (and the pad rule) do not give the number of bytes that arrived.
    Length { expected: u64, got: usize },
    /// 56 + `len` is a multiple of 64 and the pad flag is not set, or it is
    /// not and the flag is set.
    PadFlag { len: u32, set: bool },
    /// The pad byte is not zero.
    PadByte(u8),
    /// A message frame whose offset is not 0.
    MessageOffset(u64),
    /// A data frame with no payload, or one whose end is past what a u64 holds.
    DataRange { offset: u64, len: u32 },
    /// The payload is not what the header's SHA-256 says.
    Digest,
    /// Not the next sequence number.
    Seq { expected: u32, got: u32 },
    /// A data frame with no stream open in that direction.
    NoStream,
    /// A data frame that is not at the stream's position.
    StreamOffset { expected: u64, got: u64 },
    /// A data frame that runs past the stream's size.
    StreamOverrun { size: u64, end: u64 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Short { got } => {
                write!(f, "{got} bytes, less than a {HEADER_LEN}-byte header")
            }
            FrameError::Magic => write!(f, "the magic is not OMIL"),
            FrameError::Version(v) => write!(f, "frame version {v}, not {FRAME_VERSION}"),
            FrameError::Kind(k) => write!(f, "kind {k} is neither message (1) nor data (2)"),
            FrameError::Flags(x) => write!(f, "reserved flag bits set ({x:#06x})"),
            FrameError::TooLong { kind, len, max } => {
                write!(
                    f,
                    "a {kind:?} payload of {len} bytes, over the limit of {max}"
                )
            }
            FrameError::Length { expected, got } => {
                write!(
                    f,
                    "the header gives a frame of {expected} bytes, {got} arrived"
                )
            }
            FrameError::PadFlag { len, set: true } => {
                write!(
                    f,
                    "the pad flag is set, and a payload of {len} bytes takes no pad"
                )
            }
            FrameError::PadFlag { len, set: false } => {
                write!(
                    f,
                    "a payload of {len} bytes needs the pad byte, and the flag is not set"
                )
            }
            FrameError::PadByte(b) => write!(f, "the pad byte is {b:#04x}, not zero"),
            FrameError::MessageOffset(o) => write!(f, "a message frame with offset {o}"),
            FrameError::DataRange { offset, len } => {
                write!(f, "a data frame of {len} bytes at offset {offset}")
            }
            FrameError::Digest => write!(f, "the payload does not match the header's SHA-256"),
            FrameError::Seq { expected, got } => write!(f, "seq {got}, expected {expected}"),
            FrameError::NoStream => write!(f, "a data frame with no stream open"),
            FrameError::StreamOffset { expected, got } => {
                write!(
                    f,
                    "a data frame at offset {got}, the stream is at {expected}"
                )
            }
            FrameError::StreamOverrun { size, end } => {
                write!(
                    f,
                    "a data frame ending at {end}, past the stream's {size} bytes"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Does a payload of `len` bytes take the pad byte? It does when the frame
/// would otherwise be a whole number of 64-byte packets, which covers 512 and
/// 1024 too: every transfer ends in a short packet.
pub fn needs_pad(len: usize) -> bool {
    (HEADER_LEN + len).is_multiple_of(64)
}

/// The read buffer §5.1 asks for: at least 56 + `max_data` + 1 bytes, rounded
/// up to a multiple of 1024.
pub fn read_buffer_len(max_data: u32) -> usize {
    let need = HEADER_LEN + (max_data as usize).max(MAX_MESSAGE) + 1;
    need.div_ceil(1024) * 1024
}

fn limit(kind: Kind, max_data: u32) -> u32 {
    match kind {
        Kind::Message => MAX_MESSAGE as u32,
        Kind::Data => max_data.min(MAX_DATA_LIMIT),
    }
}

/// One frame as the bytes of one transfer. Refuses a payload over the limit
/// (`max_data` is the session's, for data), an empty data payload and a
/// message with an offset, so nothing is written that [`decode`] would refuse.
pub fn encode(
    kind: Kind,
    seq: u32,
    offset: u64,
    payload: &[u8],
    max_data: u32,
) -> Result<Vec<u8>, FrameError> {
    let max = limit(kind, max_data);
    if payload.len() > max as usize {
        let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        return Err(FrameError::TooLong { kind, len, max });
    }
    let len = payload.len() as u32;
    match kind {
        Kind::Message if offset != 0 => return Err(FrameError::MessageOffset(offset)),
        Kind::Data if len == 0 || offset.checked_add(len as u64).is_none() => {
            return Err(FrameError::DataRange { offset, len })
        }
        _ => {}
    }
    let pad = needs_pad(payload.len());
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + pad as usize);
    out.extend_from_slice(&MAGIC);
    out.push(FRAME_VERSION);
    out.push(kind.byte());
    out.extend_from_slice(&(if pad { FLAG_PAD } else { 0 }).to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&sha256(payload));
    out.extend_from_slice(payload);
    if pad {
        out.push(0);
    }
    Ok(out)
}

/// The bytes of one transfer as a frame. `max_data` is the limit for a data
/// payload: the session's once `hello` has agreed one, the reader's own
/// before. The sequence number is the caller's to check (it knows the last).
pub fn decode(bytes: &[u8], max_data: u32) -> Result<Frame<'_>, FrameError> {
    let Some((head, rest)) = bytes.split_first_chunk::<HEADER_LEN>() else {
        return Err(FrameError::Short { got: bytes.len() });
    };
    if head[0..4] != MAGIC {
        return Err(FrameError::Magic);
    }
    if head[4] != FRAME_VERSION {
        return Err(FrameError::Version(head[4]));
    }
    let kind = match head[5] {
        1 => Kind::Message,
        2 => Kind::Data,
        k => return Err(FrameError::Kind(k)),
    };
    let flags = u16::from_le_bytes([head[6], head[7]]);
    if flags & !FLAG_PAD != 0 {
        return Err(FrameError::Flags(flags));
    }
    let seq = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
    let len = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
    let mut o = [0u8; 8];
    o.copy_from_slice(&head[16..24]);
    let offset = u64::from_le_bytes(o);
    let max = limit(kind, max_data);
    if len > max {
        return Err(FrameError::TooLong { kind, len, max });
    }
    let padded = flags & FLAG_PAD != 0;
    if padded != needs_pad(len as usize) {
        return Err(FrameError::PadFlag { len, set: padded });
    }
    let expected = len as usize + padded as usize;
    if rest.len() != expected {
        return Err(FrameError::Length {
            expected: (HEADER_LEN + expected) as u64,
            got: bytes.len(),
        });
    }
    let payload = &rest[..len as usize];
    if padded && rest[len as usize] != 0 {
        return Err(FrameError::PadByte(rest[len as usize]));
    }
    match kind {
        Kind::Message if offset != 0 => return Err(FrameError::MessageOffset(offset)),
        Kind::Data if len == 0 || offset.checked_add(len as u64).is_none() => {
            return Err(FrameError::DataRange { offset, len })
        }
        _ => {}
    }
    if !ct_eq(&sha256(payload), &head[24..56]) {
        return Err(FrameError::Digest);
    }
    Ok(Frame {
        kind,
        seq,
        offset,
        payload,
    })
}
