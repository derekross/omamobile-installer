// SPDX-License-Identifier: MIT
//! The root image's path to `userdata`: compressed bytes in, sparse images
//! out, each no larger than the limit, with bounded memory.
//!
//! ```text
//! .img.zst bytes -> feed() -> ZstdPush -> Patcher -> SparseSplitter -> next_segment() -> Session::flash_userdata_segment
//! ```
//!
//! The patcher puts the owner's answers into the blocks the release's root
//! image keeps for them ([`RootAnswers`], docs/DESIGN.md §12): each block,
//! as the release built it, must have the SHA-256 the signed manifest
//! records, or the stream stops there, before that block or anything after
//! it is cut into a piece. Every other byte passes unchanged.
//!
//! The image's size comes from the zstd frame header (the published images
//! record it); a stream without one is refused, because every sparse image
//! states the whole image's block count up front.
use crate::bundle::VerifiedRootImage;
use crate::fastboot::{PieceReport, Session, Transport};
use crate::firstboot::{Region, RootAnswers};
use crate::hash::sha256_hex;
use crate::sparse::SparseSplitter;
use crate::zstd_push::{frame_info, ZstdPush};
use crate::{Error, Result};
use std::io::Read;

/// Swaps the answers' blocks into the raw image as it passes, in order.
struct Patcher {
    regions: Vec<Region>,
    /// The next region to reach.
    next: usize,
    /// Raw bytes passed so far.
    pos: u64,
    /// The bytes of the region being passed, as the release built them.
    held: Vec<u8>,
}

impl Patcher {
    fn push(&mut self, mut data: &[u8], out: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        while !data.is_empty() {
            let Some(r) = self.regions.get(self.next) else {
                self.pos += data.len() as u64;
                return out(data);
            };
            if self.pos < r.offset {
                let n = (r.offset - self.pos).min(data.len() as u64) as usize;
                out(&data[..n])?;
                self.pos += n as u64;
                data = &data[n..];
                continue;
            }
            let end = r.offset + r.bytes.len() as u64;
            let n = (end - self.pos).min(data.len() as u64) as usize;
            self.held.extend_from_slice(&data[..n]);
            self.pos += n as u64;
            data = &data[n..];
            if self.pos == end {
                if sha256_hex(&self.held) != r.original_sha256 {
                    return Err(Error::Refused(format!(
                        "the system image does not hold the place for {} where its signed manifest says: the owner's answers are not put there, and the install stops",
                        r.path
                    )));
                }
                self.held.clear();
                out(&r.bytes)?;
                self.next += 1;
            }
        }
        Ok(())
    }

    fn finish(&self) -> Result<()> {
        match self.regions.get(self.next) {
            None => Ok(()),
            Some(r) => Err(Error::Refused(format!(
                "the system image ended before the place for {}",
                r.path
            ))),
        }
    }
}

pub struct UserdataStream {
    zstd: ZstdPush,
    patcher: Option<Patcher>,
    sparse: Option<SparseSplitter>,
    max_segment: u64,
    head: Vec<u8>,
    image_len: Option<u64>,
    ready: Vec<Vec<u8>>,
    ended: bool,
}

impl UserdataStream {
    /// `max_segment`: the phone's `max-download-size`, or a smaller cap.
    pub fn new(max_segment: u64) -> Self {
        Self {
            zstd: ZstdPush::new(),
            patcher: None,
            sparse: None,
            max_segment,
            head: Vec::new(),
            image_len: None,
            ready: Vec::new(),
            ended: false,
        }
    }

    /// As [`Self::new`], with the owner's answers put into the blocks the
    /// root image keeps for them as it passes.
    pub fn with_answers(max_segment: u64, answers: &RootAnswers) -> Self {
        let mut s = Self::new(max_segment);
        s.patcher = Some(Patcher {
            regions: answers.regions.clone(),
            next: 0,
            pos: 0,
            held: Vec::new(),
        });
        s
    }

    /// The raw image's size, once the header has arrived.
    pub fn image_len(&self) -> Option<u64> {
        self.image_len
    }

    /// (blocks done, blocks total) of the raw image.
    pub fn progress(&self) -> (u32, u32) {
        self.sparse.as_ref().map(|s| s.progress()).unwrap_or((0, 0))
    }

    /// Add compressed bytes. Then take segments with [`Self::next_segment`]
    /// until it gives `None`, before feeding more: that is what bounds
    /// memory to this chunk, one segment and one zstd block.
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        if self.sparse.is_some() {
            self.zstd.feed(data);
            return Ok(());
        }
        self.head.extend_from_slice(data);
        let Some(info) = frame_info(&self.head)? else {
            return Ok(());
        };
        let len = info.content_size.ok_or_else(|| {
            Error::Zstd("the root image's zstd header doesn't state its size".into())
        })?;
        self.image_len = Some(len);
        self.sparse = Some(SparseSplitter::new(len, self.max_segment)?);
        self.zstd.feed(&core::mem::take(&mut self.head));
        Ok(())
    }

    /// The next finished sparse image, or `None` when more input is needed
    /// (or, after [`Self::end`], when there are no more).
    pub fn next_segment(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if !self.ready.is_empty() {
                return Ok(Some(self.ready.remove(0)));
            }
            let Some(sparse) = self.sparse.as_mut() else {
                return Ok(None);
            };
            let ready = &mut self.ready;
            let stepped = match self.patcher.as_mut() {
                Some(patcher) => self
                    .zstd
                    .step(&mut |raw| patcher.push(raw, &mut |d| sparse.push(d, ready)))?,
                None => self.zstd.step(&mut |raw| sparse.push(raw, ready))?,
            };
            if !stepped {
                return Ok(None);
            }
        }
    }

    /// All input has been fed and every segment taken: check the stream
    /// ended whole and release the last segment to [`Self::next_segment`].
    pub fn end(&mut self) -> Result<u64> {
        if self.ended {
            return Err(Error::Zstd("the stream was already ended".into()));
        }
        while self.zstd.step(&mut |_| {
            Err(Error::Zstd(
                "segments were left untaken before the end".into(),
            ))
        })? {}
        let len = self.zstd.finish()?;
        if let Some(patcher) = &self.patcher {
            patcher.finish()?;
        }
        let sparse = self
            .sparse
            .take()
            .ok_or_else(|| Error::Zstd("the stream ended inside its header".into()))?;
        sparse.finish(&mut self.ready)?;
        self.ended = true;
        Ok(len)
    }
}

/// The cap on one sparse image of the root image that every front end
/// passes, below the phone's `max-download-size`: it bounds the memory one
/// piece takes (a phone may offer 256 MiB and more). One number for the
/// command-line tool and the page, so neither does arithmetic of its own.
pub const SEGMENT_CAP: u64 = 64 << 20;

/// How much compressed input is handed to the splitter at a time, however
/// much a caller hands in at once.
const FEED_CHUNK: usize = 256 << 10;

/// The root image's path to `userdata` with the compressed bytes pushed in,
/// for a front end that cannot block on a reader (a browser reading a file
/// a piece at a time). It is the one loop there is:
/// [`flash_userdata_with_progress`] reads its reader into this.
///
/// The bytes must be the ones `proof` was made from, and that is checked,
/// not trusted: every byte fed here goes to the session's hash as well as
/// to the splitter ([`Session::userdata_streamed`]), and the session
/// refuses to finish `userdata`, so never makes slot B active, unless what
/// was streamed is the image the signed manifest lists.
pub struct UserdataFeed {
    stream: UserdataStream,
    begun: bool,
    pieces: u32,
}

impl UserdataFeed {
    /// `segment_cap` bounds memory below the session's `max-download-size`;
    /// the size of a piece is never the caller's to choose beyond that. The
    /// session's answers ([`Session::answers`]), when it has them, go into
    /// their placeholders as the image streams.
    pub fn new<T: Transport>(session: &Session<T>, segment_cap: Option<u64>) -> Self {
        let limit = segment_cap.map_or(session.max_download_size(), |c| {
            c.min(session.max_download_size())
        });
        // The answers the flow gave the session, if any: the session checks
        // that every piece it sends carries them where they go, so every
        // front end that feeds the image (the tool's reader, the page's
        // file) puts them in the same way
        let stream = match session.answers() {
            Some(answers) => UserdataStream::with_answers(limit, answers),
            None => UserdataStream::new(limit),
        };
        Self {
            stream,
            begun: false,
            pieces: 0,
        }
    }

    /// (blocks done, blocks total) of the raw image.
    pub fn progress(&self) -> (u32, u32) {
        self.stream.progress()
    }

    async fn drain<T: Transport>(
        &mut self,
        session: &mut Session<T>,
        progress: &mut dyn FnMut(u32, &PieceReport),
    ) -> Result<()> {
        while let Some(segment) = self.stream.next_segment()? {
            session.flash_userdata_segment(&segment).await?;
            self.pieces += 1;
            if let Some(piece) = session.pieces().last() {
                progress(self.pieces, piece);
            }
        }
        Ok(())
    }

    /// The next compressed bytes, in order from the file's first. Every
    /// sparse image they complete is handed to the session before this
    /// returns, which is what bounds memory: await it before reading more.
    pub async fn feed<T: Transport>(
        &mut self,
        session: &mut Session<T>,
        proof: &VerifiedRootImage,
        compressed: &[u8],
        progress: &mut dyn FnMut(u32, &PieceReport),
    ) -> Result<()> {
        for chunk in compressed.chunks(FEED_CHUNK) {
            // The same bytes to the hash and to the splitter
            session.userdata_streamed(chunk);
            self.stream.feed(chunk)?;
            if !self.begun {
                if let Some(len) = self.stream.image_len() {
                    session.begin_userdata(proof, len).await?;
                    self.begun = true;
                }
            }
            self.drain(session, progress).await?;
        }
        Ok(())
    }

    /// Every byte was fed: the last piece, and the session's check that
    /// what was streamed is the signed image. The raw image's size.
    pub async fn end<T: Transport>(
        mut self,
        session: &mut Session<T>,
        progress: &mut dyn FnMut(u32, &PieceReport),
    ) -> Result<u64> {
        let len = self.stream.end()?;
        self.drain(session, progress).await?;
        session.finish_userdata().await?;
        Ok(len)
    }
}

/// Stream a compressed root image from a reader to `userdata`. The reader
/// must give the bytes `proof` was made from (the verified, cached download),
/// and that is checked, not trusted: every byte read here goes to the
/// session's hash as well as to the splitter
/// ([`Session::userdata_streamed`]), and the session refuses to finish
/// `userdata`, so never makes slot B active, unless what was streamed is
/// the image the signed manifest lists. `proof` is what lets the first
/// write happen at all; the second hash is what binds the stream to it.
/// `segment_cap` bounds memory below the phone's `max-download-size`.
pub async fn flash_userdata_from_reader<T: Transport>(
    session: &mut Session<T>,
    proof: &VerifiedRootImage,
    reader: impl Read,
    segment_cap: Option<u64>,
) -> Result<u64> {
    flash_userdata_with_progress(session, proof, reader, segment_cap, &mut |_, _| {}).await
}

/// As [`flash_userdata_from_reader`]; `progress` is called after each sparse
/// image the session took, with how many it has taken so far and what that
/// one was ([`PieceReport`]: its size, the blocks its flash covered, how
/// long the flash took), so a front end can show that a long write is
/// moving, and how fast.
pub async fn flash_userdata_with_progress<T: Transport>(
    session: &mut Session<T>,
    proof: &VerifiedRootImage,
    mut reader: impl Read,
    segment_cap: Option<u64>,
    progress: &mut dyn FnMut(u32, &PieceReport),
) -> Result<u64> {
    let mut feed = UserdataFeed::new(session, segment_cap);
    let mut buf = vec![0u8; FEED_CHUNK];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Transport(format!("reading the root image: {e}")))?;
        if n == 0 {
            break;
        }
        feed.feed(session, proof, &buf[..n], progress).await?;
    }
    feed.end(session, progress).await
}
