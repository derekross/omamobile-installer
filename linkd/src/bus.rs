// SPDX-License-Identifier: MIT
//! The link's two directions as the daemon's flow control wants them
//! (INSTALL-LINK-PLAN §5.7): reads and writes are independent (a thread
//! each), at most [`MAX_QUEUED`] frames wait for the bus and reading stops
//! while that many wait, and a write the bus does not take in time ends
//! the session.
//!
//! Two transports keep these rules the same way:
//!
//! - **FunctionFS** (stage L3, the phone): the bulk OUT endpoint is read
//!   and the bulk IN endpoint written with native asynchronous I/O, one
//!   request at a time each, so that a write the host has not taken after
//!   its time is cancelled (`io_cancel`, which dequeues the USB request)
//!   rather than left in the controller's queue. A third thread reads the
//!   function's events from `ep0`. One completed read is one frame:
//!   FunctionFS completes an OUT request at the short packet that ends a
//!   transfer, and the pad rule (§5.1) gives every frame one.
//! - **A socket of type `SOCK_SEQPACKET`** (stage L2, the tests): one frame
//!   per read, as bulk transfers give. Two one-byte packets stand in for
//!   FunctionFS's events: 1 is `ENABLE`, 0 is `DISABLE`. The peer closing
//!   its end is the function gone.
use crate::aio::{self, Done};
use crate::daemon::Ev;
use crate::ffs::{self, Function};
use omamobile_link::frame;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
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
    /// FunctionFS: the function is enabled (between `ENABLE` and `DISABLE`
    /// or `UNBIND`). The reader asks for a frame only then.
    enabled: bool,
    /// The daemon is leaving: the threads let go of their files.
    stop: bool,
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
    fn wait<'a>(&self, q: MutexGuard<'a, Queue>, limit: Duration) -> MutexGuard<'a, Queue> {
        self.cv
            .wait_timeout(q, limit)
            .unwrap_or_else(|e| e.into_inner())
            .0
    }
}

#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
    /// FunctionFS's threads, joined by [`Bus::stop`]. A socket's reader
    /// waits in `read` for the peer and is left to the process's end.
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

/// How often a thread that waits on FunctionFS looks whether the daemon is
/// leaving.
const STOP_POLL: Duration = Duration::from_millis(100);
/// How long a cancelled request may take to come back.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// What one read of the link was.
enum Got {
    Frame(usize),
    /// The stand-in's event (seqpacket only).
    Link(bool),
    /// Nothing for the daemon: read again.
    Again,
    Gone,
}

/// What one write came to.
enum Put {
    Taken,
    /// Not taken within its time: cancelled.
    Stalled,
    /// The link went from under it (disabled, unbound, the peer gone).
    Lost,
}

trait Reader: Send + 'static {
    fn get(&mut self, buf: &mut Vec<u8>, inner: &Inner) -> Got;
}

trait Writer: Send + 'static {
    fn put(&mut self, frame: Vec<u8>, limit: Duration, inner: &Inner) -> Put;
}

impl Reader for UnixStream {
    fn get(&mut self, buf: &mut Vec<u8>, _: &Inner) -> Got {
        match Read::read(self, buf) {
            Ok(0) => Got::Gone,
            Ok(1) => Got::Link(buf[0] == ENABLE),
            Ok(n) => Got::Frame(n),
            Err(e) if e.kind() == ErrorKind::Interrupted => Got::Again,
            Err(_) => Got::Gone,
        }
    }
}

impl Writer for UnixStream {
    fn put(&mut self, frame: Vec<u8>, _: Duration, _: &Inner) -> Put {
        match Write::write(self, &frame) {
            Ok(_) => Put::Taken,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Put::Stalled
            }
            Err(_) => Put::Lost,
        }
    }
}

fn errno(res: i64) -> i32 {
    (-res) as i32
}

/// The bulk OUT endpoint, read with one asynchronous request at a time.
struct FfsReader {
    file: std::fs::File,
    ctx: aio::Context,
}

impl Reader for FfsReader {
    fn get(&mut self, buf: &mut Vec<u8>, inner: &Inner) -> Got {
        // A request only while the function is enabled
        {
            let mut q = inner.lock();
            while !q.enabled && !q.stop {
                q = inner.wait(q, STOP_POLL);
            }
            if q.stop {
                return Got::Gone;
            }
        }
        let len = buf.len();
        if let Err(e) = self.ctx.read(self.file.as_fd(), std::mem::take(buf)) {
            *buf = vec![0; len];
            // EAGAIN: disabled since the flag was read; DISABLE is on its way
            std::thread::sleep(Duration::from_millis(10));
            return if e.raw_os_error() == Some(libc::EAGAIN) {
                Got::Again
            } else {
                log(&format!("FunctionFS: a read could not be started: {e}"));
                Got::Again
            };
        }
        loop {
            match self.ctx.wait(STOP_POLL) {
                Ok(Done::Result(res, back)) => {
                    *buf = back;
                    return match res {
                        n if n > 0 => Got::Frame(n as usize),
                        // A zero-length packet: no frame (§5.1: the link
                        // never sends one)
                        0 => Got::Again,
                        // The function was disabled under the request, or
                        // the host halted the endpoint: the events say what
                        // happened to the link
                        e if [libc::ESHUTDOWN, libc::ECONNRESET, libc::EPIPE, libc::EINTR]
                            .contains(&errno(e)) =>
                        {
                            Got::Again
                        }
                        e if errno(e) == libc::ENODEV => Got::Gone,
                        e => {
                            log(&format!(
                                "FunctionFS: a read ended in error {}",
                                std::io::Error::from_raw_os_error(errno(e))
                            ));
                            std::thread::sleep(Duration::from_millis(10));
                            Got::Again
                        }
                    };
                }
                Ok(Done::Waiting) => {
                    if inner.lock().stop {
                        let _ = self.ctx.cancel(CANCEL_WAIT);
                        return Got::Gone;
                    }
                }
                Err(e) => {
                    log(&format!("FunctionFS: waiting for a read: {e}"));
                    return Got::Gone;
                }
            }
        }
    }
}

/// The bulk IN endpoint: one asynchronous request at a time, cancelled
/// when the host has not taken it within its time.
struct FfsWriter {
    file: std::fs::File,
    ctx: aio::Context,
}

impl Writer for FfsWriter {
    fn put(&mut self, frame: Vec<u8>, limit: Duration, inner: &Inner) -> Put {
        if !inner.lock().enabled {
            return Put::Lost;
        }
        let len = frame.len() as i64;
        if self.ctx.write(self.file.as_fd(), frame).is_err() {
            // EAGAIN: the function is not enabled
            return Put::Lost;
        }
        let end = Instant::now() + limit;
        loop {
            let slice = end.saturating_duration_since(Instant::now()).min(STOP_POLL);
            match self.ctx.wait(slice) {
                Ok(Done::Result(res, _)) if res == len => return Put::Taken,
                Ok(Done::Result(..)) => return Put::Lost,
                Ok(Done::Waiting) => {
                    let stop = inner.lock().stop;
                    if stop || Instant::now() >= end {
                        // §5.7: cancel what the bus has not taken. A request
                        // that completed before the cancel reached it was
                        // taken after all.
                        return match self.ctx.cancel(CANCEL_WAIT) {
                            Ok(Done::Result(res, _)) if res == len => Put::Taken,
                            Ok(Done::Result(res, _)) => {
                                log(&format!(
                                    "FunctionFS: a write was cancelled after {}s ({})",
                                    limit.as_secs(),
                                    std::io::Error::from_raw_os_error(errno(res))
                                ));
                                if stop {
                                    Put::Lost
                                } else {
                                    Put::Stalled
                                }
                            }
                            _ => {
                                log("FunctionFS: a cancelled write did not come back");
                                Put::Stalled
                            }
                        };
                    }
                }
                Err(_) => return Put::Lost,
            }
        }
    }
}

fn log(msg: &str) {
    eprintln!("omamobile-linkd: {msg}");
}

impl Bus {
    fn new() -> Bus {
        Bus {
            inner: Arc::new(Inner {
                q: Mutex::new(Queue {
                    frames: VecDeque::new(),
                    writing: false,
                    handled: true,
                    stalled: false,
                    enabled: false,
                    stop: false,
                }),
                cv: Condvar::new(),
            }),
            threads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let h = std::thread::spawn(f);
        self.threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(h);
    }

    /// Start reading and writing `sock` (stage L2). Each write gets
    /// `write_timeout`.
    pub fn start(
        sock: UnixStream,
        max_data: u32,
        write_timeout: Duration,
        tx: Sender<Ev>,
    ) -> std::io::Result<Bus> {
        sock.set_write_timeout(Some(write_timeout))?;
        let bus = Bus::new();
        // The socket is the link: always there until the peer goes
        bus.inner.lock().enabled = true;
        let (r_in, r_tx) = (bus.inner.clone(), tx.clone());
        let reader = sock.try_clone()?;
        std::thread::spawn(move || read_loop(reader, max_data, &r_in, &r_tx));
        let w_in = bus.inner.clone();
        std::thread::spawn(move || write_loop(sock, write_timeout, &w_in, &tx));
        Ok(bus)
    }

    /// Start the function's three threads: `ep0`'s events, the OUT
    /// endpoint's reads and the IN endpoint's writes.
    pub fn start_ffs(
        function: Function,
        max_data: u32,
        write_timeout: Duration,
        tx: Sender<Ev>,
    ) -> std::io::Result<Bus> {
        let bus = Bus::new();
        let Function { ep0, out, inp } = function;
        let reader = FfsReader {
            file: out,
            ctx: aio::Context::new()?,
        };
        let writer = FfsWriter {
            file: inp,
            ctx: aio::Context::new()?,
        };
        let (e_in, e_tx) = (bus.inner.clone(), tx.clone());
        bus.spawn(move || ep0_loop(ep0, &e_in, &e_tx));
        let (r_in, r_tx) = (bus.inner.clone(), tx.clone());
        bus.spawn(move || read_loop(reader, max_data, &r_in, &r_tx));
        let w_in = bus.inner.clone();
        bus.spawn(move || write_loop(writer, write_timeout, &w_in, &tx));
        Ok(bus)
    }

    /// The daemon is leaving: every thread cancels what it has under way
    /// and lets go of its file, and this returns when they have. For
    /// FunctionFS that closes the function (see [`Function`]).
    pub fn stop(&self) {
        self.inner.lock().stop = true;
        self.inner.cv.notify_all();
        let threads: Vec<_> = self
            .threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for t in threads {
            let _ = t.join();
        }
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
            q = self.inner.wait(q, left);
        }
        true
    }

    /// The daemon has carried out what the last thing read asked for.
    pub fn handled(&self) {
        self.inner.lock().handled = true;
        self.inner.cv.notify_all();
    }
}

fn read_loop(mut link: impl Reader, max_data: u32, inner: &Inner, tx: &Sender<Ev>) {
    let mut buf = vec![0u8; frame::read_buffer_len(max_data)];
    loop {
        {
            // Not while 8 frames wait for the bus, and not before the
            // frame before this one is dealt with
            let mut q = inner.lock();
            while !q.stop && (!q.handled || q.waiting() >= MAX_QUEUED) {
                q = inner.wait(q, STOP_POLL);
            }
            if q.stop {
                return;
            }
            q.handled = false;
        }
        let ev = match link.get(&mut buf, inner) {
            Got::Frame(n) => Ev::Frame(buf[..n].to_vec()),
            Got::Link(up) => Ev::Link(up),
            Got::Again => {
                inner.lock().handled = true;
                continue;
            }
            Got::Gone => Ev::Gone,
        };
        let gone = matches!(ev, Ev::Gone);
        if gone && inner.lock().stop {
            return;
        }
        if tx.send(ev).is_err() || gone {
            return;
        }
    }
}

fn write_loop(mut link: impl Writer, limit: Duration, inner: &Inner, tx: &Sender<Ev>) {
    loop {
        let frame = {
            let mut q = inner.lock();
            loop {
                if q.stop {
                    return;
                }
                if let Some(f) = q.frames.pop_front() {
                    q.writing = true;
                    break f;
                }
                q = inner.wait(q, STOP_POLL);
            }
        };
        let res = link.put(frame, limit, inner);
        let mut q = inner.lock();
        q.writing = false;
        let mut ev = None;
        match res {
            Put::Taken => {}
            Put::Stalled => {
                // The bus did not take it in time: a lost link. What was
                // queued behind it is cancelled with it.
                q.frames.clear();
                q.stalled = true;
                ev = Some(Ev::WriteStalled);
            }
            // The link is gone or down: the reader or the events say so
            Put::Lost => q.frames.clear(),
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

/// `ep0`: the function's events, for the daemon and for the reader and
/// writer (whether the function is enabled). A control request for the
/// function is stalled: the link answers none (the OS descriptors are the
/// gadget's, answered by the kernel).
fn ep0_loop(ep0: std::fs::File, inner: &Inner, tx: &Sender<Ev>) {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    let mut buf = [0u8; 4 * ffs::EVENT_LEN];
    let wait = Timespec {
        tv_sec: 0,
        tv_nsec: STOP_POLL.as_nanos() as i64,
    };
    loop {
        if inner.lock().stop {
            return;
        }
        let mut fds = [PollFd::new(&ep0, PollFlags::IN)];
        match poll(&mut fds, Some(&wait)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => {
                let _ = tx.send(Ev::Gone);
                return;
            }
        }
        let n = match rustix::io::read(&ep0, &mut buf) {
            Ok(n) => n,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
            Err(e) => {
                log(&format!("FunctionFS: ep0: {e}"));
                if !inner.lock().stop {
                    let _ = tx.send(Ev::Gone);
                }
                return;
            }
        };
        for event in ffs::parse_events(&buf[..n]) {
            match event {
                ffs::Event::Enable => inner.lock().enabled = true,
                ffs::Event::Disable | ffs::Event::Unbind => inner.lock().enabled = false,
                ffs::Event::Setup { request_type, .. } => {
                    // The wrong direction of the data stage stalls it
                    let mut one = [0u8; 1];
                    let _ = if request_type & 0x80 != 0 {
                        rustix::io::read(&ep0, &mut one).map(|_| ())
                    } else {
                        rustix::io::write(&ep0, &one).map(|_| ())
                    };
                }
                _ => {}
            }
            inner.cv.notify_all();
            if tx.send(Ev::Function(event)).is_err() {
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
