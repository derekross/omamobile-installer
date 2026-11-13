// SPDX-License-Identifier: MIT
//! Spike: the shared core of the Omarchy Mobile installer.
//!
//! One crate, built natively (the CLI, a desktop app) and to WebAssembly (the
//! web flasher). It holds every rule that decides what may be written to a
//! phone, so no front end can widen them:
//!
//! - [`manifest`], [`device_conf`]: `manifest.json` and `device.conf` as data,
//!   checked against the same rules as `tools/install-phone`;
//! - [`signature`], [`bundle`]: the release key's signature on `SHA256SUMS`,
//!   then every checksum, giving a [`bundle::VerifiedRelease`];
//! - [`fastboot`]: the protocol over a [`fastboot::Transport`], reachable only
//!   through [`fastboot::Session`], which can write nothing but the verified
//!   release's `<image>_b` partitions and `userdata`;
//! - [`sparse`], [`zstd_push`], [`install`]: the root image, decompressed and
//!   cut into Android sparse images no larger than the phone's
//!   `max-download-size`, without holding the image in memory.
//!
//! - [`download`]: Google's factory image, fetched by a front end from the
//!   address in the signed build index and decided here, answer by answer.
//! - [`text`]: what a phone, a server or a file name says, made fit to print
//!   before it is kept, compared or put on a screen.
//! - [`flow`]: the install as one state machine for every front end, with
//!   the write path behind a typed confirmation;
//! - [`firstboot`]: the owner's answers as the files a first start reads,
//!   and as the blocks of the root image's placeholders that
//!   [`install::UserdataStream`] swaps in (docs/DESIGN.md §12).
//!
//! Nothing here does I/O on its own: bytes come in from the caller, and the
//! only ways out are the transport trait and, for Google's factory image, a
//! typed request the front end carries out ([`download::FetchRequest`]).

// The tests' support files (core/tests/support) name this crate from the
// outside; the unit tests that borrow them see it under the same name.
#[cfg(test)]
extern crate self as omamobile_installer_core;

pub mod build_index;
pub mod bundle;
pub mod device_conf;
pub mod download;
pub mod error;
pub mod fastboot;
pub mod firstboot;
pub mod flow;
pub mod hash;
pub mod install;
pub mod manifest;
pub mod signature;
pub mod sparse;
pub mod text;
pub mod zstd_push;

pub use error::Error;
pub type Result<T> = core::result::Result<T, Error>;
