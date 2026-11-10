// SPDX-License-Identifier: MIT
//! zstd decompression fed in pieces, as a download or a file read delivers
//! them, for callers that can't block on a reader (a browser).
//!
//! The decoder is ruzstd (MIT, pure Rust), so the same code builds natively
//! and to WebAssembly with no C toolchain. ruzstd decodes whole blocks from a
//! reader; this wrapper does the framing (RFC 8878 §3.1: the frame header,
//! then 3-byte block headers) so it only ever hands ruzstd complete blocks.
//!
//! One frame per stream, as `zstd` writes for a root image. Memory: the
//! frame's window (refused above [`MAX_WINDOW`]) plus one block.
use crate::{Error, Result};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
use std::io::Read;

/// The largest window accepted: what `zstd --long=27` or `--ultra -22` asks.
pub const MAX_WINDOW: u64 = 128 << 20;
const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const MAX_BLOCK: usize = 128 << 10;

/// What a frame header says before any data is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub header_len: usize,
    /// The decompressed size, when the compressor recorded it.
    pub content_size: Option<u64>,
    pub has_checksum: bool,
}

/// Parse a frame header. `Ok(None)`: more bytes are needed (at most 18).
pub fn frame_info(data: &[u8]) -> Result<Option<FrameInfo>> {
    if data.len() < 5 {
        return Ok(None);
    }
    if data[..4] != MAGIC {
        return Err(Error::Zstd("not a zstd stream".into()));
    }
    let fhd = data[4];
    if fhd & 0x08 != 0 {
        return Err(Error::Zstd("reserved bit set in the frame header".into()));
    }
    let single_segment = fhd & 0x20 != 0;
    let dict_len = [0usize, 1, 2, 4][(fhd & 3) as usize];
    let fcs_len = match fhd >> 6 {
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let fcs_at = 5 + usize::from(!single_segment) + dict_len;
    let header_len = fcs_at + fcs_len;
    if data.len() < header_len {
        return Ok(None);
    }
    if dict_len != 0 {
        return Err(Error::Zstd("the stream needs a dictionary".into()));
    }
    let f = &data[fcs_at..header_len];
    let content_size = match fcs_len {
        0 => None,
        1 => Some(f[0] as u64),
        2 => Some(u16::from_le_bytes([f[0], f[1]]) as u64 + 256),
        4 => Some(u32::from_le_bytes(f.try_into().unwrap()) as u64),
        _ => Some(u64::from_le_bytes(f.try_into().unwrap())),
    };
    Ok(Some(FrameInfo { header_len, content_size, has_checksum: fhd & 0x04 != 0 }))
}

pub struct ZstdPush {
    dec: FrameDecoder,
    buf: Vec<u8>,
    /// Bytes of `buf` already consumed.
    off: usize,
    info: Option<FrameInfo>,
    finished: bool,
    produced: u64,
    scratch: Vec<u8>,
}

impl Default for ZstdPush {
    fn default() -> Self {
        Self::new()
    }
}

impl ZstdPush {
    pub fn new() -> Self {
        let mut dec = FrameDecoder::new();
        dec.set_max_window_size(MAX_WINDOW);
        Self { dec, buf: Vec::new(), off: 0, info: None, finished: false, produced: 0, scratch: vec![0; MAX_BLOCK] }
    }

    /// Known once the first bytes have been pushed.
    pub fn info(&self) -> Option<FrameInfo> {
        self.info
    }

    pub fn produced(&self) -> u64 {
        self.produced
    }

    fn drain(&mut self, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        loop {
            let n = self.dec.read(&mut self.scratch).map_err(|e| Error::Zstd(e.to_string()))?;
            if n == 0 {
                return Ok(());
            }
            self.produced += n as u64;
            sink(&self.scratch[..n])?;
        }
    }

    /// Add compressed bytes. Nothing is decoded until [`Self::step`].
    pub fn feed(&mut self, data: &[u8]) {
        // Drop what was consumed, so the buffer holds only what is pending
        if self.off > 0 {
            self.buf.drain(..self.off);
            self.off = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Decode at most one block (128 KiB of output) from what was fed.
    /// `Ok(false)`: more input is needed, or the frame has ended. One block
    /// per call keeps the caller in control: a megabyte of compressed zeros
    /// is gigabytes of image.
    pub fn step(&mut self, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<bool> {
        if self.finished {
            return if self.off == self.buf.len() { Ok(false) } else { Err(Error::Zstd("data after the end of the frame".into())) };
        }
        if self.info.is_none() {
            let Some(info) = frame_info(&self.buf)? else { return Ok(false) };
            self.dec.init(&self.buf[..info.header_len]).map_err(|e| Error::Zstd(e.to_string()))?;
            self.off = info.header_len;
            self.info = Some(info);
        }
        let has_checksum = self.info.is_some_and(|i| i.has_checksum);
        let avail = &self.buf[self.off..];
        if avail.len() < 3 {
            return Ok(false);
        }
        let h = u32::from_le_bytes([avail[0], avail[1], avail[2], 0]);
        let last = h & 1 != 0;
        let size = (h >> 3) as usize;
        let body = match (h >> 1) & 3 {
            0 | 2 => size, // raw, compressed
            1 => 1,        // RLE: one byte, repeated
            _ => return Err(Error::Zstd("reserved block type".into())),
        };
        if body > MAX_BLOCK {
            return Err(Error::Zstd("a block larger than the format allows".into()));
        }
        let need = 3 + body + if last && has_checksum { 4 } else { 0 };
        if avail.len() < need {
            return Ok(false);
        }
        let done = self.dec.decode_blocks(&avail[..need], BlockDecodingStrategy::UptoBlocks(1)).map_err(|e| Error::Zstd(e.to_string()))?;
        self.off += need;
        self.drain(sink)?;
        if done != last {
            return Err(Error::Zstd("the decoder and the block headers disagree on the frame's end".into()));
        }
        if done {
            self.finished = true;
            if has_checksum && self.dec.get_checksum_from_data() != self.dec.get_calculated_checksum() {
                return Err(Error::Zstd("the frame's checksum doesn't match its content".into()));
            }
            if self.off != self.buf.len() {
                return Err(Error::Zstd("data after the end of the frame".into()));
            }
        }
        Ok(true)
    }

    /// Feed and decode everything that is complete. For small inputs and
    /// tests; a large stream should use [`Self::feed`] and [`Self::step`].
    pub fn push(&mut self, data: &[u8], sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        self.feed(data);
        while self.step(sink)? {}
        Ok(())
    }

    /// The stream must have ended on a frame boundary, with the size its
    /// header promised.
    pub fn finish(&self) -> Result<u64> {
        if !self.finished {
            return Err(Error::Zstd("the stream ended inside a frame".into()));
        }
        if let Some(want) = self.info.and_then(|i| i.content_size) {
            if want != self.produced {
                return Err(Error::Zstd(format!("decompressed to {} bytes, not the {want} its header states", self.produced)));
            }
        }
        Ok(self.produced)
    }
}
