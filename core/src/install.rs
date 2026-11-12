// SPDX-License-Identifier: MIT
//! The root image's path to `userdata`: compressed bytes in, sparse images
//! out, each no larger than the limit, with bounded memory.
//!
//! ```text
//! .img.zst bytes -> feed() -> ZstdPush -> SparseSplitter -> next_segment() -> Session::flash_userdata_segment
//! ```
//!
//! The image's size comes from the zstd frame header (the published images
//! record it); a stream without one is refused, because every sparse image
//! states the whole image's block count up front.
use crate::bundle::VerifiedRootImage;
use crate::fastboot::{PieceReport, Session, Transport};
use crate::sparse::SparseSplitter;
use crate::zstd_push::{frame_info, ZstdPush};
use crate::{Error, Result};
use std::io::Read;

pub struct UserdataStream {
    zstd: ZstdPush,
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
            sparse: None,
            max_segment,
            head: Vec::new(),
            image_len: None,
            ready: Vec::new(),
            ended: false,
        }
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
            if !self.zstd.step(&mut |raw| sparse.push(raw, ready))? {
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
        let sparse = self
            .sparse
            .take()
            .ok_or_else(|| Error::Zstd("the stream ended inside its header".into()))?;
        sparse.finish(&mut self.ready)?;
        self.ended = true;
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
    let limit = segment_cap.map_or(session.max_download_size(), |c| {
        c.min(session.max_download_size())
    });
    let mut stream = UserdataStream::new(limit);
    let mut buf = vec![0u8; 256 << 10];
    let (mut begun, mut pieces) = (false, 0u32);
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Transport(format!("reading the root image: {e}")))?;
        if n == 0 {
            break;
        }
        // The same bytes to the hash and to the splitter
        session.userdata_streamed(&buf[..n]);
        stream.feed(&buf[..n])?;
        if !begun {
            if let Some(len) = stream.image_len() {
                session.begin_userdata(proof, len).await?;
                begun = true;
            }
        }
        while let Some(segment) = stream.next_segment()? {
            session.flash_userdata_segment(&segment).await?;
            pieces += 1;
            if let Some(piece) = session.pieces().last() {
                progress(pieces, piece);
            }
        }
    }
    let len = stream.end()?;
    while let Some(segment) = stream.next_segment()? {
        session.flash_userdata_segment(&segment).await?;
        pieces += 1;
        if let Some(piece) = session.pieces().last() {
            progress(pieces, piece);
        }
    }
    session.finish_userdata().await?;
    Ok(len)
}
