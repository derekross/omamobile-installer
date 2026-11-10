// SPDX-License-Identifier: MIT
//! Cuts a raw image, pushed in as it is decompressed, into Android sparse
//! images that each fit one fastboot download.
//!
//! Format: AOSP `system/core/libsparse/sparse_format.h` (Apache-2.0). Every
//! output is a complete sparse image of the whole partition image: a
//! DONT_CARE chunk skips to where its data starts, RAW and FILL chunks follow,
//! and a last DONT_CARE reaches the image's end, so the chunks always add up
//! to `total_blks`. That is the layout AOSP's `sparse_file_resparse` gives
//! `fastboot flash` for an image larger than `max-download-size`: flashing
//! the pieces one after another to the same partition writes the image.
//!
//! A block of one repeated 32-bit value becomes a FILL chunk, zeros included
//! (as `img2simg` does, which today's installer runs): zero blocks are
//! written, not skipped, so nothing stale stays on `userdata` inside the
//! image's extent.
//!
//! Memory: one segment (at most the limit given) and one block.
use crate::{Error, Result};

pub const BLOCK: usize = 4096;
const MAGIC: u32 = 0xed26_ff3a;
const FILE_HDR: usize = 28;
const CHUNK_HDR: usize = 12;
const RAW: u16 = 0xcac1;
const FILL: u16 = 0xcac2;
const DONT_CARE: u16 = 0xcac3;
/// Room for a header, both skips and one raw block.
pub const MIN_SEGMENT: u64 = (FILE_HDR + 3 * CHUNK_HDR + BLOCK) as u64;

#[derive(Clone, Copy, PartialEq)]
enum Open {
    None,
    /// A RAW chunk whose header is at this offset.
    Raw(usize),
    /// A FILL chunk whose header is at this offset, and its value.
    Fill(usize, [u8; 4]),
}

pub struct SparseSplitter {
    total_blocks: u32,
    limit: usize,
    /// Blocks consumed so far.
    pos: u32,
    partial: Vec<u8>,
    seg: Vec<u8>,
    chunks: u32,
    /// RAW and FILL chunks in this segment.
    data_chunks: u32,
    open: Open,
    open_blocks: u32,
}

fn chunk_header(kind: u16, blocks: u32, data_len: u32) -> [u8; CHUNK_HDR] {
    let mut h = [0u8; CHUNK_HDR];
    h[0..2].copy_from_slice(&kind.to_le_bytes());
    h[4..8].copy_from_slice(&blocks.to_le_bytes());
    h[8..12].copy_from_slice(&(CHUNK_HDR as u32 + data_len).to_le_bytes());
    h
}

impl SparseSplitter {
    /// `image_len`: the raw image's size in bytes, a multiple of 4096.
    /// `max_segment`: the largest sparse image to emit, in bytes (the phone's
    /// `max-download-size`, or less).
    pub fn new(image_len: u64, max_segment: u64) -> Result<Self> {
        if image_len == 0 || !image_len.is_multiple_of(BLOCK as u64) {
            return Err(Error::Sparse(format!("the image is {image_len} bytes, not a multiple of {BLOCK}")));
        }
        let total_blocks = u32::try_from(image_len / BLOCK as u64).map_err(|_| Error::Sparse("the image is too large".into()))?;
        if max_segment < MIN_SEGMENT {
            return Err(Error::Sparse(format!("a download limit of {max_segment} bytes is too small")));
        }
        // A sparse image's chunk sizes are 32-bit; stay well inside
        let limit = max_segment.min(u32::MAX as u64 / 2) as usize;
        let mut s = Self { total_blocks, limit, pos: 0, partial: Vec::with_capacity(BLOCK), seg: Vec::new(), chunks: 0, data_chunks: 0, open: Open::None, open_blocks: 0 };
        s.start_segment();
        Ok(s)
    }

    fn start_segment(&mut self) {
        self.seg = Vec::with_capacity(self.limit.min(1 << 20));
        self.seg.resize(FILE_HDR, 0);
        self.chunks = 0;
        self.data_chunks = 0;
        self.open = Open::None;
        self.open_blocks = 0;
        if self.pos > 0 {
            self.seg.extend_from_slice(&chunk_header(DONT_CARE, self.pos, 0));
            self.chunks += 1;
        }
    }

    fn close_chunk(&mut self) {
        match self.open {
            Open::None => {}
            Open::Raw(at) => {
                let h = chunk_header(RAW, self.open_blocks, self.open_blocks * BLOCK as u32);
                self.seg[at..at + CHUNK_HDR].copy_from_slice(&h);
            }
            Open::Fill(at, _) => {
                let h = chunk_header(FILL, self.open_blocks, 4);
                self.seg[at..at + CHUNK_HDR].copy_from_slice(&h);
            }
        }
        self.open = Open::None;
        self.open_blocks = 0;
    }

    fn finish_segment(&mut self, out: &mut Vec<Vec<u8>>) {
        self.close_chunk();
        if self.pos < self.total_blocks {
            self.seg.extend_from_slice(&chunk_header(DONT_CARE, self.total_blocks - self.pos, 0));
            self.chunks += 1;
        }
        let mut h = [0u8; FILE_HDR];
        h[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        h[4..6].copy_from_slice(&1u16.to_le_bytes()); // major
        h[6..8].copy_from_slice(&0u16.to_le_bytes()); // minor
        h[8..10].copy_from_slice(&(FILE_HDR as u16).to_le_bytes());
        h[10..12].copy_from_slice(&(CHUNK_HDR as u16).to_le_bytes());
        h[12..16].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        h[16..20].copy_from_slice(&self.total_blocks.to_le_bytes());
        h[20..24].copy_from_slice(&self.chunks.to_le_bytes());
        // image_checksum stays 0: "not used", as img2simg writes it
        self.seg[..FILE_HDR].copy_from_slice(&h);
        debug_assert!(self.seg.len() <= self.limit);
        out.push(core::mem::take(&mut self.seg));
    }

    fn add_block(&mut self, block: &[u8], out: &mut Vec<Vec<u8>>) -> Result<()> {
        if self.pos >= self.total_blocks {
            return Err(Error::Sparse("more data than the image's stated size".into()));
        }
        let first: [u8; 4] = block[..4].try_into().unwrap();
        let fill = block.chunks_exact(4).all(|w| w == first);
        // What this block adds to the segment
        let extends = match self.open {
            Open::Raw(_) => !fill,
            Open::Fill(_, v) => fill && v == first,
            Open::None => false,
        };
        let need = match (extends, fill) {
            (true, true) => 0,
            (true, false) => BLOCK,
            (false, true) => CHUNK_HDR + 4,
            (false, false) => CHUNK_HDR + BLOCK,
        };
        // Always keep room for the closing skip
        if self.seg.len() + need + CHUNK_HDR > self.limit {
            self.finish_segment(out);
            self.start_segment();
            return self.add_block(block, out);
        }
        if !extends {
            self.close_chunk();
            let at = self.seg.len();
            self.seg.extend_from_slice(&[0u8; CHUNK_HDR]);
            self.chunks += 1;
            self.data_chunks += 1;
            if fill {
                self.seg.extend_from_slice(&first);
                self.open = Open::Fill(at, first);
            } else {
                self.open = Open::Raw(at);
            }
        }
        if !fill {
            self.seg.extend_from_slice(block);
        }
        self.open_blocks += 1;
        self.pos += 1;
        Ok(())
    }

    /// Feed raw image bytes. Finished segments are appended to `out`; send
    /// each to the phone before pushing more to keep memory bounded.
    pub fn push(&mut self, mut data: &[u8], out: &mut Vec<Vec<u8>>) -> Result<()> {
        if !self.partial.is_empty() {
            let take = (BLOCK - self.partial.len()).min(data.len());
            self.partial.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.partial.len() < BLOCK {
                return Ok(());
            }
            let block = core::mem::take(&mut self.partial);
            self.add_block(&block, out)?;
            self.partial = block;
            self.partial.clear();
        }
        let mut blocks = data.chunks_exact(BLOCK);
        for block in &mut blocks {
            self.add_block(block, out)?;
        }
        self.partial.extend_from_slice(blocks.remainder());
        Ok(())
    }

    /// Blocks taken so far, and the image's total.
    pub fn progress(&self) -> (u32, u32) {
        (self.pos, self.total_blocks)
    }

    /// The image must have arrived whole; emits the last segment.
    pub fn finish(mut self, out: &mut Vec<Vec<u8>>) -> Result<()> {
        if !self.partial.is_empty() || self.pos != self.total_blocks {
            let got = self.pos as u64 * BLOCK as u64 + self.partial.len() as u64;
            return Err(Error::Sparse(format!("the image ended at {got} bytes, short of its stated {}", self.total_blocks as u64 * BLOCK as u64)));
        }
        if self.data_chunks > 0 {
            self.finish_segment(out);
        }
        Ok(())
    }
}
