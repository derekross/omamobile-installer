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
//! **A piece is bounded in what it makes the phone write, not only in its
//! own size.** A FILL chunk is sixteen bytes however many blocks it covers,
//! so by size alone one small download could tell the phone to write the
//! rest of the image: gigabytes behind a single `flash:`, with the host
//! waiting on one answer. So a piece also ends once its RAW and FILL
//! chunks cover [`MAX_PIECE_BLOCKS`] blocks, and the next one goes on from
//! there. [`cover`] reads a finished piece back, for the session, which
//! refuses one that covers more whoever cut it.
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
/// The most blocks one piece's RAW and FILL chunks may cover: 256 MiB of
/// the partition, so that every `flash:` is bounded in the work it asks of
/// the phone. The number is tied to how long a transport waits for an
/// answer ([`FLASH_WAIT_SECONDS`]): a phone that writes at
/// [`MIN_WRITE_KIB_PER_SECOND`] or faster answers inside the wait.
pub const MAX_PIECE_BLOCKS: u32 = 65536;
/// How long the USB transport waits for one answer: `omamobile-installer-usb`
/// takes its wait from this constant, so the two cannot drift apart.
pub const FLASH_WAIT_SECONDS: u64 = 300;
/// The slowest write for which the largest piece is still answered inside
/// the wait: 256 MiB in 300 s, rounded up. Flash storage in a phone writes
/// on the order of a hundred times faster.
pub const MIN_WRITE_KIB_PER_SECOND: u64 =
    (MAX_PIECE_BLOCKS as u64 * (BLOCK as u64 / 1024)).div_ceil(FLASH_WAIT_SECONDS);

/// What one sparse image covers of the image it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cover {
    /// The whole image's size in blocks, as the header states it.
    pub total_blocks: u32,
    /// The blocks its RAW and FILL chunks write: the work one `flash:` of
    /// it asks of the phone.
    pub written_blocks: u32,
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Read a sparse image as this module writes them, and say what it covers.
/// Anything else is refused: another block size or header size, a chunk
/// type that is not RAW, FILL or DONT_CARE, a chunk whose size is not its
/// type's, chunks that do not add up to the stated total, bytes left over.
pub fn cover(image: &[u8]) -> Result<Cover> {
    let bad = |why: &str| Error::Sparse(format!("not a sparse image this installer cuts: {why}"));
    if image.len() < FILE_HDR || le32(image) != MAGIC {
        return Err(bad("no sparse header"));
    }
    if le16(&image[4..]) != 1
        || le16(&image[8..]) as usize != FILE_HDR
        || le16(&image[10..]) as usize != CHUNK_HDR
        || le32(&image[12..]) as usize != BLOCK
    {
        return Err(bad("another version, header size or block size"));
    }
    let total_blocks = le32(&image[16..]);
    let chunks = le32(&image[20..]);
    let (mut at, mut seen, mut written) = (FILE_HDR, 0u64, 0u64);
    for _ in 0..chunks {
        let Some(header) = image.get(at..at + CHUNK_HDR) else {
            return Err(bad("a chunk header past its end"));
        };
        let blocks = le32(&header[4..]) as u64;
        let data = match le16(header) {
            RAW => blocks * BLOCK as u64,
            FILL => 4,
            DONT_CARE => 0,
            _ => return Err(bad("a chunk of a type it does not write")),
        };
        if le32(&header[8..]) as u64 != CHUNK_HDR as u64 + data {
            return Err(bad("a chunk whose size is not its type's"));
        }
        if le16(header) != DONT_CARE {
            written += blocks;
        }
        seen += blocks;
        at = usize::try_from(at as u64 + CHUNK_HDR as u64 + data)
            .ok()
            .filter(|end| *end <= image.len())
            .ok_or_else(|| bad("a chunk past its end"))?;
    }
    if at != image.len() || seen != total_blocks as u64 {
        return Err(bad("its chunks do not add up to the image"));
    }
    Ok(Cover {
        total_blocks,
        written_blocks: written as u32,
    })
}

/// What one sparse image says goes into one block of the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockIn<'a> {
    /// The image does not cover the block (a DONT_CARE chunk).
    Skipped,
    Raw(&'a [u8]),
    Fill([u8; 4]),
}

/// What `image`, a sparse image [`cover`] took, writes to block `block`.
pub fn block_in(image: &[u8], block: u64) -> Result<BlockIn<'_>> {
    cover(image)?;
    let chunks = le32(&image[20..]);
    let (mut at, mut start) = (FILE_HDR, 0u64);
    for _ in 0..chunks {
        let header = &image[at..at + CHUNK_HDR];
        let blocks = le32(&header[4..]) as u64;
        let size = le32(&header[8..]) as usize;
        if (start..start + blocks).contains(&block) {
            let data = &image[at + CHUNK_HDR..at + size];
            return Ok(match le16(header) {
                RAW => {
                    let from = ((block - start) as usize) * BLOCK;
                    BlockIn::Raw(&data[from..from + BLOCK])
                }
                FILL => BlockIn::Fill(data[..4].try_into().unwrap()),
                _ => BlockIn::Skipped,
            });
        }
        start += blocks;
        at += size;
    }
    Err(Error::Sparse(format!(
        "block {block} is past the image's end"
    )))
}

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
    /// Blocks those chunks cover: at most [`MAX_PIECE_BLOCKS`].
    seg_blocks: u32,
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
            return Err(Error::Sparse(format!(
                "the image is {image_len} bytes, not a multiple of {BLOCK}"
            )));
        }
        let total_blocks = u32::try_from(image_len / BLOCK as u64)
            .map_err(|_| Error::Sparse("the image is too large".into()))?;
        if max_segment < MIN_SEGMENT {
            return Err(Error::Sparse(format!(
                "a download limit of {max_segment} bytes is too small"
            )));
        }
        // A sparse image's chunk sizes are 32-bit; stay well inside
        let limit = max_segment.min(u32::MAX as u64 / 2) as usize;
        let mut s = Self {
            total_blocks,
            limit,
            pos: 0,
            partial: Vec::with_capacity(BLOCK),
            seg: Vec::new(),
            chunks: 0,
            data_chunks: 0,
            seg_blocks: 0,
            open: Open::None,
            open_blocks: 0,
        };
        s.start_segment();
        Ok(s)
    }

    fn start_segment(&mut self) {
        self.seg = Vec::with_capacity(self.limit.min(1 << 20));
        self.seg.resize(FILE_HDR, 0);
        self.chunks = 0;
        self.data_chunks = 0;
        self.seg_blocks = 0;
        self.open = Open::None;
        self.open_blocks = 0;
        if self.pos > 0 {
            self.seg
                .extend_from_slice(&chunk_header(DONT_CARE, self.pos, 0));
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
            self.seg
                .extend_from_slice(&chunk_header(DONT_CARE, self.total_blocks - self.pos, 0));
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
            return Err(Error::Sparse(
                "more data than the image's stated size".into(),
            ));
        }
        let first: [u8; 4] = block[..4].try_into().unwrap();
        let fill = block.as_chunks::<4>().0.iter().all(|w| *w == first);
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
        // Always keep room for the closing skip. And a piece ends once it
        // covers as many blocks as one flash may: a FILL chunk grows by
        // no bytes, so the size alone would never end it
        if self.seg.len() + need + CHUNK_HDR > self.limit || self.seg_blocks >= MAX_PIECE_BLOCKS {
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
        self.seg_blocks += 1;
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
        let (blocks, rest) = data.as_chunks::<BLOCK>();
        for block in blocks {
            self.add_block(block, out)?;
        }
        self.partial.extend_from_slice(rest);
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
            return Err(Error::Sparse(format!(
                "the image ended at {got} bytes, short of its stated {}",
                self.total_blocks as u64 * BLOCK as u64
            )));
        }
        if self.data_chunks > 0 {
            self.finish_segment(out);
        }
        Ok(())
    }
}
