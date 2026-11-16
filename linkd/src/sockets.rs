// SPDX-License-Identifier: MIT
//! The daemon's two local sockets, each carrying fixed lines and nothing
//! else, each for one peer at a time whose uid the kernel vouches for
//! (`SO_PEERCRED`, unix(7)):
//!
//! - `agent`: the owner's session agent (INSTALL-LINK-PLAN §2.8). The peer
//!   must be the account first boot made. Agent to daemon: `show`, `hide`.
//!   Daemon to agent: `AgentLine::line()`.
//! - `screen`: the first-start screen program (§3.3). The peer must be
//!   root. Screen to daemon: `key` (a volume key was pressed). Daemon to
//!   screen: what to show. This socket is how the code reaches the screen:
//!   it is never in a file or a log.
use crate::daemon::Ev;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Agent,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Show,
    Hide,
    Key,
}

/// The longest line a peer may send.
const MAX_LINE: usize = 16;

fn parse(who: Who, line: &[u8]) -> Option<Line> {
    match (who, line) {
        (Who::Agent, b"show") => Some(Line::Show),
        (Who::Agent, b"hide") => Some(Line::Hide),
        (Who::Screen, b"key") => Some(Line::Key),
        _ => None,
    }
}

fn peer_uid(stream: &UnixStream) -> Option<u32> {
    rustix::net::sockopt::socket_peercred(stream)
        .ok()
        .map(|c| c.uid.as_raw())
}

/// Listen at `path`. `allowed` says whether a peer's uid is the one this
/// socket is for; it is asked anew for every connection.
pub fn listen(
    path: &Path,
    who: Who,
    mode: u32,
    allowed: impl Fn(u32) -> bool + Send + 'static,
    tx: Sender<Ev>,
) -> std::io::Result<()> {
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    let busy = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        let mut id = 0u64;
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // Who is it? The kernel says; nothing the peer sends is asked
            if !peer_uid(&stream).is_some_and(&allowed) {
                continue;
            }
            // One peer at a time
            if busy.swap(true, Ordering::SeqCst) {
                continue;
            }
            id += 1;
            let Ok(writer) = stream.try_clone() else {
                busy.store(false, Ordering::SeqCst);
                continue;
            };
            if tx.send(Ev::Peer(who, id, writer)).is_err() {
                return;
            }
            let (tx, busy) = (tx.clone(), busy.clone());
            std::thread::spawn(move || {
                read_lines(stream, who, id, &tx);
                busy.store(false, Ordering::SeqCst);
                let _ = tx.send(Ev::PeerGone(who, id));
            });
        }
    });
    Ok(())
}

/// Read the peer's lines until it leaves or says anything but its fixed
/// lines.
fn read_lines(mut stream: UnixStream, who: Who, id: u64, tx: &Sender<Ev>) {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => {
                let Some(l) = parse(who, &line) else { return };
                if tx.send(Ev::Line(who, id, l)).is_err() {
                    return;
                }
                line.clear();
            }
            Ok(1) if line.len() < MAX_LINE => line.push(byte[0]),
            _ => return,
        }
    }
}

/// One line to a peer. A peer that does not take it is left to its reader
/// thread to notice.
pub fn say(stream: &mut UnixStream, line: &str) {
    let _ = stream.write_all(format!("{line}\n").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_fixed_lines() {
        assert_eq!(parse(Who::Agent, b"show"), Some(Line::Show));
        assert_eq!(parse(Who::Agent, b"hide"), Some(Line::Hide));
        assert_eq!(parse(Who::Agent, b"key"), None);
        assert_eq!(parse(Who::Screen, b"key"), Some(Line::Key));
        assert_eq!(parse(Who::Screen, b"show"), None);
        assert_eq!(parse(Who::Agent, b"show "), None);
        assert_eq!(parse(Who::Agent, b""), None);
    }
}
