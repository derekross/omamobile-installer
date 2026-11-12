// SPDX-License-Identifier: MIT
//! The install link's protocol: what a freshly installed Omarchy Mobile
//! phone and the installer say to each other over USB at the phone's first
//! start. Protocol 1, as omarchy-mobile's `docs/INSTALL-LINK-PLAN.md` §5
//! lays it out; `docs/LINK.md` in this repository says how the crate is
//! meant to be used and where it had to differ from that document.
//!
//! The crate does no I/O, reads no clock and draws no random numbers. It
//! turns bytes into frames and messages and back ([`frame`], [`msg`]),
//! checks the owner's answers by first boot's rules ([`answers`]), and holds
//! the two sides' state machines ([`phone`], [`host`]): each takes what
//! happened (a frame, the time, the outcome of something its shell did) and
//! gives back, in order, the frames to send and the things for the shell to
//! do. The phone's daemon and the installer are shells around them.
#![forbid(unsafe_code)]

pub mod answers;
pub mod digest;
pub mod frame;
pub mod json;
pub mod msg;
pub mod phone;
#[cfg(feature = "sim")]
pub mod sim;

/// The protocol number this crate speaks.
pub const PROTOCOL: u32 = 1;

/// Random bytes, from the caller: the crate never draws its own, so a test
/// decides the code, the token and the nonce. The phone's daemon fills this
/// from `getrandom`.
pub trait Entropy {
    fn fill(&mut self, buf: &mut [u8]);
}
