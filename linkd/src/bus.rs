// SPDX-License-Identifier: MIT
//! The link's two directions as the daemon's flow control wants them
//! (INSTALL-LINK-PLAN §5.7): reads and writes are independent (a thread
//! each), at most [`MAX_QUEUED`] frames wait for the bus and reading stops
//! while that many wait, and a write the bus does not take in time ends
//! the session.
//!
//! The transport of stage L2 is a socket of type `SOCK_SEQPACKET` in place
//! of FunctionFS's endpoint files: one frame per read, as bulk transfers
//! give. Two one-byte packets stand in for FunctionFS's events: 1 is
//! `ENABLE`, 0 is `DISABLE`. The peer closing its end is the function gone.
use crate::daemon::Ev;
use omamobile_link::frame;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Frames that may wait for the bus (§5.7, item 3).
pub const MAX_QUEUED: usize = 8;
pub const ENABLE: u8 = 1;
pub const DISABLE: u8 = 0;

struct Queue {
    frames: VecDeque<Vec<u8>>,
    /// A frame is in the bus's hands.
    writing: bool,
    /// The daemon has carried out what the last thing read asked for: the
    /// next is read only then (§5.7, item 6: the disk holds the host back).
    handled: bool,
    /// A write was given up, and the daemon has not ended the session yet:
    /// what the machine still sends into that session is cancelled too. A
    /// frame let through here would reach the host numbered after the ones
    /// that were cancelled.
    stalled: bool,
}

impl Queue {
    fn waiting(&self) -> usize {
        self.frames.len() + usize::from(self.writing)
    }
}

struct Inner {
    q: Mutex<Queue>,
    cv: Condvar,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
}

impl Bus {
    /// Start reading and writing `sock`. Each write gets `write_timeout`.
    pub fn start(
        sock: UnixStream,
        max_data: u32,
        write_timeout: Duration,
        tx: Sender<Ev>,
    ) -> std::io::Result<Bus> {
        sock.set_write_timeout(Some(write_timeout))?;
        let inner = Arc::new(Inner {
            q: Mutex::new(Queue {
                frames: VecDeque::new(),
                writing: false,
                handled: true,
                stalled: false,
            }),
            cv: Condvar::new(),
        });
        let (reader, r_in, r_tx) = (sock.try_clone()?, inner.clone(), tx.clone());
        std::thread::spawn(move || read_loop(reader, max_data, &r_in, &r_tx));
        let w_in = inner.clone();
        std::thread::spawn(move || write_loop(sock, &w_in, &tx));
        Ok(Bus { inner })
    }

    /// One frame for the host: one write, in its turn.
    pub fn send(&self, frame: Vec<u8>) {
        let mut q = self.inner.lock();
        if !q.stalled {
            q.frames.push_back(frame);
        }
        drop(q);
        self.inner.cv.notify_all();
    }

    /// The daemon has ended the session a write was given up in: frames
    /// are taken again.
    pub fn session_ended(&self) {
        self.inner.lock().stalled = false;
    }

    /// Cancel the frames the bus has not taken yet (§5.3, §5.7).
    pub fn drop_unsent(&self) {
        self.inner.lock().frames.clear();
        self.inner.cv.notify_all();
    }

    /// Nothing waits for the bus.
    pub fn idle(&self) -> bool {
        self.inner.lock().waiting() == 0
    }

    /// Wait until the bus has taken everything queued, at most `limit`.
    pub fn flush(&self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        let mut q = self.inner.lock();
        while q.waiting() > 0 {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            q = self
                .inner
                .cv
                .wait_timeout(q, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// The daemon has carried out what the last thing read asked for.
    pub fn handled(&self) {
        self.inner.lock().handled = true;
        self.inner.cv.notify_all();
    }
}

fn read_loop(mut sock: UnixStream, max_data: u32, inner: &Inner, tx: &Sender<Ev>) {
    let mut buf = vec![0u8; frame::read_buffer_len(max_data)];
    loop {
        {
            // Not while 8 frames wait for the bus, and not before the
            // frame before this one is dealt with
            let mut q = inner.lock();
            while !q.handled || q.waiting() >= MAX_QUEUED {
                q = inner.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            q.handled = false;
        }
        let ev = match sock.read(&mut buf) {
            Ok(0) => Ev::Gone,
            Ok(1) => Ev::Link(buf[0] == ENABLE),
            Ok(n) => Ev::Frame(buf[..n].to_vec()),
            Err(e) if e.kind() == ErrorKind::Interrupted => {
                inner.lock().handled = true;
                continue;
            }
            Err(_) => Ev::Gone,
        };
        let gone = matches!(ev, Ev::Gone);
        if tx.send(ev).is_err() || gone {
            return;
        }
    }
}

fn write_loop(mut sock: UnixStream, inner: &Inner, tx: &Sender<Ev>) {
    loop {
        let frame = {
            let mut q = inner.lock();
            loop {
                if let Some(f) = q.frames.pop_front() {
                    q.writing = true;
                    break f;
                }
                q = inner.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let res = sock.write(&frame);
        let mut q = inner.lock();
        q.writing = false;
        let mut ev = None;
        match res {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // The bus did not take it in time: a lost link. What was
                // queued behind it is cancelled with it.
                q.frames.clear();
                q.stalled = true;
                ev = Some(Ev::WriteStalled);
            }
            // The peer is gone: the reader says so
            Err(_) => q.frames.clear(),
        }
        if ev.is_none() && q.frames.is_empty() {
            ev = Some(Ev::QueueEmpty);
        }
        drop(q);
        inner.cv.notify_all();
        if let Some(ev) = ev {
            if tx.send(ev).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn pair() -> (UnixStream, UnixStream) {
        let (a, b) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        (UnixStream::from(a), UnixStream::from(b))
    }

    #[test]
    fn what_is_sent_into_a_session_whose_write_was_given_up_never_reaches_the_host() {
        let (ours, mut host) = pair();
        let (tx, rx) = mpsc::channel();
        let bus = Bus::start(ours, 1024, Duration::from_millis(50), tx).unwrap();
        // A host that does not read: the bus fills, and a write is given up
        let mut queued = 0u32;
        let stalled = loop {
            match rx.try_recv() {
                Ok(Ev::WriteStalled) => break true,
                Ok(_) => {}
                Err(mpsc::TryRecvError::Disconnected) => break false,
                Err(mpsc::TryRecvError::Empty) => {
                    if bus.inner.lock().waiting() < MAX_QUEUED {
                        bus.send(vec![b'a'; 600]);
                        queued += 1;
                    }
                }
            }
        };
        assert!(stalled);
        assert!(bus.idle(), "what was queued behind it is cancelled");
        // The machine has not heard of it yet and still answers something
        // in the old session: cancelled too
        bus.send(b"LATE: a frame of the session that ended".to_vec());
        assert!(bus.idle());
        // The daemon ends the session; the next one's frames go out
        bus.session_ended();
        bus.send(b"NEXT".to_vec());
        host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 2048];
        let mut read = 0u32;
        loop {
            let n = host.read(&mut buf).unwrap();
            if &buf[..n] == b"NEXT" {
                break;
            }
            assert_eq!(
                buf[..n],
                [b'a'; 600],
                "a frame of the ended session got out"
            );
            read += 1;
        }
        // The frame in the bus's hands when the write was given up was not
        // taken, nor the ones behind it
        assert!(read < queued, "{read} of {queued}");
    }

    #[test]
    fn reading_waits_for_the_frame_before_and_for_the_bus() {
        let (ours, mut host) = pair();
        let (tx, rx) = mpsc::channel();
        let bus = Bus::start(ours, 1024, Duration::from_secs(30), tx).unwrap();
        for n in 0..3u8 {
            host.write_all(&[n; 60]).unwrap();
        }
        let frame = |rx: &mpsc::Receiver<Ev>| loop {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(Ev::Frame(f)) => return Some(f[0]),
                Ok(_) => {}
                Err(_) => return None,
            }
        };
        // One frame is read; the next only when this one is dealt with
        assert_eq!(frame(&rx), Some(0));
        assert_eq!(frame(&rx), None, "read on before the frame was dealt with");
        bus.handled();
        assert_eq!(frame(&rx), Some(1));
        // One-byte packets are the link's events, not frames
        bus.handled();
        assert_eq!(frame(&rx), Some(2));
        host.write_all(&[ENABLE]).unwrap();
        bus.handled();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(Ev::Link(true))
        ));
        // The host's end closed: the function is gone
        bus.handled();
        drop(host);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(Ev::Gone)
        ));
    }
}
