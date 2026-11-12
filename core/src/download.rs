// SPDX-License-Identifier: MIT
//! Google's factory image, downloaded for the owner (the maintainer's
//! decision of 2026-11-12).
//!
//! The core does no I/O, so a front end does the fetching, and decides
//! nothing about it. [`crate::flow::Flow::factory_download`] hands it a
//! [`FactoryDownload`]; from then on:
//!
//! ```text
//! front end                                core
//! ---------                                ----
//! bytes it kept of an earlier try     -->  kept            hashed from the first byte
//! request()                           <--  FetchRequest    the address, where to go on from, the size
//! one HTTP GET, redirects not followed
//! status and headers                  -->  response        Fetch (a redirect, checked), Body, or End
//! the body, a piece at a time         -->  body            counted, hashed; false: stop reading
//! Flow::factory_download_done         -->  DownloadEnd     Verified, Kept or Discard
//! ```
//!
//! What holds here, whatever a front end or a server does:
//!
//! - **The address is the signed index's.** A [`FetchRequest`] can only be
//!   made here, from the `url` of a build in a verified release's index or
//!   from a redirect that passed the same rule.
//! - **`https://dl.google.com/` and nothing else,**
//!   ([`crate::build_index::is_download_url`]), on the index entry and again
//!   on every redirect. A redirect anywhere else is not followed.
//! - **The size is the index's,** before the transfer (what the server says
//!   it will send) and during it (not one byte more is taken).
//! - **The SHA-256 is over the whole file,** from its first byte, also when
//!   the download went on from bytes kept earlier: those are fed in first and
//!   hashed like the rest.
//! - **Bytes kept on disk are deleted only when they can't be part of the
//!   right file:** there are more of them than the file has, the file is
//!   complete and its SHA-256 is another, or more bytes arrived after them
//!   than the file has. An answer that only *announces* another size (a
//!   `Content-Length`, a `Content-Range` total) ends the try before a byte
//!   of its body is read and still stops the install, but it says nothing
//!   about what an earlier try kept, so that stays for the next run, where
//!   it is hashed from its first byte like any kept bytes.
use crate::build_index::is_download_url;
use crate::hash::StreamHasher;
use crate::{Error, Result};

/// How many redirects in a row a download follows.
pub const MAX_REDIRECTS: u8 = 5;

/// One HTTP GET for the front end to carry out. Made only by the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    url: String,
    from: u64,
    size: u64,
}

impl FetchRequest {
    /// On `https://dl.google.com/`.
    pub fn url(&self) -> &str {
        &self.url
    }
    /// The first byte to ask for. Above zero, the request carries
    /// [`Self::range`].
    pub fn from(&self) -> u64 {
        self.from
    }
    /// The whole file's size, from the signed index.
    pub fn size(&self) -> u64 {
        self.size
    }
    /// The `Range` header's value, when the download goes on from bytes kept
    /// earlier.
    pub fn range(&self) -> Option<String> {
        (self.from > 0).then(|| format!("bytes={}-", self.from))
    }
}

/// The server's answer to a [`FetchRequest`], before its body.
#[derive(Debug, Clone, Default)]
pub struct FetchResponse<'a> {
    pub status: u16,
    /// The `Location` header of a redirect.
    pub location: Option<&'a str>,
    /// For a client that can't be told not to follow redirects (a browser's
    /// `fetch`): the address the answer came from in the end.
    pub final_url: Option<&'a str>,
    pub content_length: Option<u64>,
    /// The `Content-Range` header, as sent.
    pub content_range: Option<&'a str>,
}

/// What the front end does with an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchNext {
    /// A redirect that stays on `https://dl.google.com/`: fetch this instead.
    Fetch(FetchRequest),
    /// The body follows: feed it to [`FactoryDownload::body`]. `at`: where
    /// its first byte belongs in the file. Zero when bytes were kept and the
    /// server sent the whole file all the same: what was kept is dropped.
    Body { at: u64 },
    /// Read no body. [`crate::flow::Flow::factory_download_done`] says why.
    End,
}

/// What kept the front end from carrying a request out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchFailure {
    /// A browser's `fetch` was rejected before any answer. A page is not
    /// told why: no connection, or a server that does not allow a page on
    /// another origin to read its files (CORS).
    NoAnswer,
    /// The connection could not be made, or broke.
    Network(String),
    /// The transfer went quiet: no byte of the body arrived for this many
    /// seconds, and the front end ended the try. A front end that fetches
    /// must have such a limit, or a server that stops sending holds it for
    /// good. What had arrived is kept, as after any transfer that broke off.
    Stalled { seconds: u64 },
    /// The bytes could not be kept on this computer.
    Storage(String),
}

/// Why the installer can't download the file, so that the owner fetches it
/// by hand. Nothing here says the file is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// [`FetchFailure::NoAnswer`].
    NoAnswer,
    /// A redirect to anything but `https://dl.google.com/`. Not followed.
    OffGoogle,
    TooManyRedirects,
    /// Neither the file nor a redirect.
    Status(u16),
    /// An answer that does not fit the request.
    BadAnswer,
}

/// Why what the server sent is not the file the signed index describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The server said the file has another size.
    AnnouncedSize { got: u64 },
    /// More bytes arrived than the file has.
    TooManyBytes,
    /// Every byte arrived, and the SHA-256 is another.
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ending {
    Unavailable(Unavailable),
    Mismatch(Mismatch),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The bytes kept earlier are being fed in; no answer yet.
    Start,
    Body,
    Over,
}

/// One try at downloading the factory image. See the module's text.
pub struct FactoryDownload {
    pub(crate) release: String,
    pub(crate) build: String,
    pub(crate) name: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
    url: String,
    /// Bytes of an earlier try that are fed in before anything is fetched.
    kept: u64,
    hasher: StreamHasher,
    redirects: u8,
    phase: Phase,
    ending: Option<Ending>,
}

/// `bytes FIRST-LAST/TOTAL`.
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    let number = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    Some((number(first)?, number(last)?, number(total)?))
}

impl FactoryDownload {
    /// `have`: how many bytes the front end kept of an earlier try. More than
    /// the file has can't be part of it, and is not used.
    pub(crate) fn new(
        release: String,
        build: &crate::build_index::Build,
        have: u64,
    ) -> Result<Self> {
        // Checked when the index was read, and here again, where it is used
        if !is_download_url(&build.url) {
            return Err(Error::Refused(
                "the factory image's address is not on https://dl.google.com/".into(),
            ));
        }
        Ok(FactoryDownload {
            release,
            build: build.build.clone(),
            name: build.zip.clone(),
            sha256: build.sha256.clone(),
            size: build.size,
            url: build.url.clone(),
            kept: if have > build.size { 0 } else { have },
            hasher: StreamHasher::new(),
            redirects: 0,
            phase: Phase::Start,
            ending: None,
        })
    }

    /// How many of the bytes kept earlier to feed to [`Self::kept`], from
    /// the file's first byte. Zero: start the file afresh.
    pub fn resume_from(&self) -> u64 {
        self.kept
    }

    /// The next piece of the bytes kept earlier, in order from the first.
    /// They are hashed like the bytes that arrive later.
    pub fn kept(&mut self, chunk: &[u8]) -> Result<()> {
        if self.phase != Phase::Start || self.hasher.len() + chunk.len() as u64 > self.kept {
            return Err(Error::Refused(
                "more kept bytes than this download goes on from".into(),
            ));
        }
        self.hasher.update(chunk);
        Ok(())
    }

    fn request_now(&self) -> FetchRequest {
        FetchRequest {
            url: self.url.clone(),
            from: self.kept,
            size: self.size,
        }
    }

    /// What to fetch. `None`: nothing, every byte was kept (or the try is
    /// over); go to [`crate::flow::Flow::factory_download_done`].
    pub fn request(&self) -> Option<FetchRequest> {
        (self.phase == Phase::Start && self.kept < self.size).then(|| self.request_now())
    }

    fn end(&mut self, ending: Ending) -> FetchNext {
        self.ending = Some(ending);
        self.phase = Phase::Over;
        FetchNext::End
    }

    /// The server's answer to the last request. Refused until every kept
    /// byte has been fed in.
    pub fn response(&mut self, answer: &FetchResponse<'_>) -> Result<FetchNext> {
        if self.phase != Phase::Start || self.kept >= self.size {
            return Err(Error::Refused("no answer is expected here".into()));
        }
        if self.hasher.len() != self.kept {
            return Err(Error::Refused(
                "the bytes kept earlier were not all handed in before the download went on".into(),
            ));
        }
        let unavailable = |u| Ending::Unavailable(u);
        if answer.final_url.is_some_and(|u| !is_download_url(u)) {
            return Ok(self.end(unavailable(Unavailable::OffGoogle)));
        }
        let rest = self.size - self.kept;
        match answer.status {
            301 | 302 | 303 | 307 | 308 => {
                let Some(location) = answer.location else {
                    return Ok(self.end(unavailable(Unavailable::BadAnswer)));
                };
                // The same host by its path alone, or a whole address
                let to = if location.starts_with('/') && !location.starts_with("//") {
                    format!("https://dl.google.com{location}")
                } else {
                    location.to_string()
                };
                if !is_download_url(&to) {
                    return Ok(self.end(unavailable(Unavailable::OffGoogle)));
                }
                if self.redirects == MAX_REDIRECTS {
                    return Ok(self.end(unavailable(Unavailable::TooManyRedirects)));
                }
                self.redirects += 1;
                self.url = to;
                Ok(FetchNext::Fetch(self.request_now()))
            }
            200 => {
                if let Some(got) = answer.content_length.filter(|n| *n != self.size) {
                    return Ok(self.end(Ending::Mismatch(Mismatch::AnnouncedSize { got })));
                }
                // The whole file, whatever was asked for: what was kept is
                // dropped, and so is its part of the hash
                self.kept = 0;
                self.hasher = StreamHasher::new();
                self.phase = Phase::Body;
                Ok(FetchNext::Body { at: 0 })
            }
            206 if self.kept > 0 => {
                let Some((first, last, total)) = answer.content_range.and_then(content_range)
                else {
                    return Ok(self.end(unavailable(Unavailable::BadAnswer)));
                };
                if total != self.size {
                    return Ok(self.end(Ending::Mismatch(Mismatch::AnnouncedSize { got: total })));
                }
                if first != self.kept
                    || last != self.size - 1
                    || answer.content_length.is_some_and(|n| n != rest)
                {
                    return Ok(self.end(unavailable(Unavailable::BadAnswer)));
                }
                self.phase = Phase::Body;
                Ok(FetchNext::Body { at: self.kept })
            }
            206 => Ok(self.end(unavailable(Unavailable::BadAnswer))),
            other => Ok(self.end(unavailable(Unavailable::Status(other)))),
        }
    }

    /// The next piece of the body. `false`: stop reading. More bytes than
    /// the file has are not taken.
    pub fn body(&mut self, chunk: &[u8]) -> bool {
        if self.phase != Phase::Body {
            return false;
        }
        if self.hasher.len() + chunk.len() as u64 > self.size {
            self.end(Ending::Mismatch(Mismatch::TooManyBytes));
            return false;
        }
        self.hasher.update(chunk);
        true
    }

    /// Bytes of the file in hand (kept and arrived), and its size.
    pub fn progress(&self) -> (u64, u64) {
        (self.hasher.len(), self.size)
    }

    /// The file's name, as Google publishes it.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn ending(&self) -> Option<&Ending> {
        self.ending.as_ref()
    }
    /// Whether the kept bytes the download counted on were all fed in.
    pub(crate) fn kept_fed(&self) -> bool {
        self.phase != Phase::Start || self.hasher.len() == self.kept
    }
    /// The digest in hex and the number of bytes hashed.
    pub(crate) fn finish(self) -> (String, u64) {
        self.hasher.finish()
    }
}
