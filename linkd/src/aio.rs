// SPDX-License-Identifier: MIT
//! Linux's native asynchronous I/O, as far as the link needs it: one read
//! or write at a time on an endpoint file, a wait with a time limit, and
//! `io_cancel` (INSTALL-LINK-PLAN §5.7, item 3: a write the bus has not
//! taken after 30 s is cancelled, which dequeues the USB request; upstream
//! `tools/usb/ffs-aio-example` uses the same calls).
//!
//! The one module with `unsafe`: neither std nor rustix has a name for
//! `io_setup`, `io_submit`, `io_getevents`, `io_cancel` and `io_destroy`,
//! so they are made as plain system calls. What makes them sound is kept
//! here and nowhere else: the request block and its buffer belong to the
//! [`Context`] from the submit until the kernel has reported the request
//! complete, and are never freed before that.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

const IOCB_CMD_PREAD: u16 = 0;
const IOCB_CMD_PWRITE: u16 = 1;

/// `struct iocb` (`include/uapi/linux/aio_abi.h`), little-endian layout
/// (x86_64 and aarch64 both): 64 bytes.
#[repr(C)]
#[derive(Default)]
struct Iocb {
    aio_data: u64,
    aio_key: u32,
    aio_rw_flags: i32,
    aio_lio_opcode: u16,
    aio_reqprio: i16,
    aio_fildes: u32,
    aio_buf: u64,
    aio_nbytes: u64,
    aio_offset: i64,
    aio_reserved2: u64,
    aio_flags: u32,
    aio_resfd: u32,
}

/// `struct io_event`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct IoEvent {
    data: u64,
    obj: u64,
    res: i64,
    res2: i64,
}

const _: () = assert!(std::mem::size_of::<Iocb>() == 64);
const _: () = assert!(std::mem::size_of::<IoEvent>() == 32);

/// What a request came to.
#[derive(Debug)]
pub enum Done {
    /// The kernel's result: bytes moved, or a negative errno.
    Result(i64, Vec<u8>),
    /// The time limit passed; the request is still the kernel's.
    Waiting,
}

struct InFlight {
    iocb: Box<Iocb>,
    buf: Vec<u8>,
}

/// An AIO context with room for one request, and that request.
pub struct Context {
    id: libc::c_ulong,
    flight: Option<InFlight>,
}

// The context id is a plain number the kernel hands out; the request block
// and the buffer are owned. One thread uses a context at a time.
unsafe impl Send for Context {}

fn check(ret: libc::c_long) -> io::Result<libc::c_long> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

impl Context {
    pub fn new() -> io::Result<Context> {
        let mut id: libc::c_ulong = 0;
        // SAFETY: io_setup writes the new context's id to the pointer, which
        // is valid for the call.
        check(unsafe { libc::syscall(libc::SYS_io_setup, 1 as libc::c_long, &mut id) })?;
        Ok(Context { id, flight: None })
    }

    /// A request is the kernel's.
    pub fn busy(&self) -> bool {
        self.flight.is_some()
    }

    fn submit(
        &mut self,
        fd: BorrowedFd<'_>,
        op: u16,
        mut buf: Vec<u8>,
        len: usize,
    ) -> io::Result<()> {
        assert!(self.flight.is_none(), "one request at a time");
        assert!(len <= buf.len());
        let mut iocb = Box::new(Iocb {
            aio_lio_opcode: op,
            aio_fildes: fd.as_raw_fd() as u32,
            aio_buf: buf.as_mut_ptr() as u64,
            aio_nbytes: len as u64,
            ..Iocb::default()
        });
        let mut list = [&mut *iocb as *mut Iocb];
        // SAFETY: the list points at one request block that lives in a Box,
        // whose buffer lives in a Vec; both are kept in `self.flight` until
        // the kernel reports the request complete (see `wait`), and a Vec's
        // heap buffer does not move when the Vec does.
        let n = check(unsafe {
            libc::syscall(
                libc::SYS_io_submit,
                self.id,
                1 as libc::c_long,
                list.as_mut_ptr(),
            )
        })?;
        if n != 1 {
            return Err(io::Error::other("io_submit took no request"));
        }
        self.flight = Some(InFlight { iocb, buf });
        Ok(())
    }

    /// Start reading into `buf` (all of it).
    pub fn read(&mut self, fd: BorrowedFd<'_>, buf: Vec<u8>) -> io::Result<()> {
        let len = buf.len();
        self.submit(fd, IOCB_CMD_PREAD, buf, len)
    }

    /// Start writing all of `buf`.
    pub fn write(&mut self, fd: BorrowedFd<'_>, buf: Vec<u8>) -> io::Result<()> {
        let len = buf.len();
        self.submit(fd, IOCB_CMD_PWRITE, buf, len)
    }

    /// Wait up to `limit` for the request to complete.
    pub fn wait(&mut self, limit: Duration) -> io::Result<Done> {
        if self.flight.is_none() {
            return Err(io::Error::other("no request"));
        }
        let end = Instant::now() + limit;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let mut ts = libc::timespec {
                tv_sec: left.as_secs() as libc::time_t,
                tv_nsec: left.subsec_nanos() as libc::c_long,
            };
            let mut ev = IoEvent::default();
            // SAFETY: one event is written to `ev`; the timespec is read.
            let ret = unsafe {
                libc::syscall(
                    libc::SYS_io_getevents,
                    self.id,
                    1 as libc::c_long,
                    1 as libc::c_long,
                    &mut ev as *mut IoEvent,
                    &mut ts as *mut libc::timespec,
                )
            };
            match check(ret) {
                Ok(1) => {
                    // The kernel is done with the block and the buffer
                    let f = self.flight.take().expect("a request");
                    debug_assert_eq!(ev.obj, &*f.iocb as *const Iocb as u64);
                    return Ok(Done::Result(ev.res, f.buf));
                }
                Ok(_) => {
                    if Instant::now() >= end {
                        return Ok(Done::Waiting);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Cancel the request and wait up to `limit` for the kernel to give it
    /// back. A request that completed before the cancel reached it comes
    /// back with its own result.
    pub fn cancel(&mut self, limit: Duration) -> io::Result<Done> {
        let Some(f) = &mut self.flight else {
            return Err(io::Error::other("no request"));
        };
        let mut ev = IoEvent::default();
        // SAFETY: the block is the one submitted and still the kernel's.
        // `io_cancel` answers EINPROGRESS when the cancel is under way (the
        // event then comes through io_getevents), or EINVAL/EAGAIN when the
        // request is already done or cannot be cancelled; in every case the
        // event is waited for below.
        let _ = unsafe {
            libc::syscall(
                libc::SYS_io_cancel,
                self.id,
                &mut *f.iocb as *mut Iocb,
                &mut ev as *mut IoEvent,
            )
        };
        self.wait(limit)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if self.flight.is_some() {
            let _ = self.cancel(Duration::from_secs(5));
        }
        // io_destroy waits for whatever is still under way to end, so a
        // block and a buffer the kernel never gave back are not freed while
        // it holds them. If io_destroy fails they are leaked instead.
        // SAFETY: the context id is ours and is not used again.
        let ok = unsafe { libc::syscall(libc::SYS_io_destroy, self.id) } == 0;
        if let Some(f) = self.flight.take() {
            if !ok {
                std::mem::forget(f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn a_write_and_a_read_go_through_and_come_back() {
        let dir = std::env::temp_dir().join(format!("linkd-aio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut ctx = Context::new().unwrap();
        ctx.write(f.as_fd(), b"one frame".to_vec()).unwrap();
        assert!(ctx.busy());
        match ctx.wait(Duration::from_secs(5)).unwrap() {
            Done::Result(n, buf) => {
                assert_eq!(n, 9);
                assert_eq!(buf, b"one frame");
            }
            Done::Waiting => panic!("a file write waits"),
        }
        assert!(!ctx.busy());
        ctx.read(f.as_fd(), vec![0; 64]).unwrap();
        match ctx.wait(Duration::from_secs(5)).unwrap() {
            Done::Result(n, buf) => assert_eq!(&buf[..n as usize], b"one frame"),
            Done::Waiting => panic!("a file read waits"),
        }
        // A cancel with nothing under way is refused, not undefined
        assert!(ctx.cancel(Duration::from_millis(1)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
