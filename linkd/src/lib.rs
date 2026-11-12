// SPDX-License-Identifier: MIT
//! The install link's daemon on the phone, `omamobile-linkd`, and the
//! command beside it, `omamobile-install-link status|reopen`: stage L2 of
//! Omarchy Mobile's INSTALL-LINK-PLAN. The protocol, both state machines
//! and the start-up rules are the crate `omamobile-link`; this is the shell
//! around its phone machine. docs/LINKD.md says what is here and what is
//! not.
//!
//! For Linux only: on any other system the crate is empty, so that the
//! workspace still builds there.
#![forbid(unsafe_code)]
#![cfg(target_os = "linux")]

pub mod bus;
pub mod command;
pub mod daemon;
pub mod descriptor;
pub mod durable;
pub mod found;
pub mod paths;
pub mod programs;
pub mod sockets;
