//! Minimal inotify wrapper over libc.

use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

pub const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_MODIFY
    | libc::IN_CLOSE_WRITE
    | libc::IN_ACCESS
    | libc::IN_OPEN
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR
    | libc::IN_DONT_FOLLOW
    | libc::IN_EXCL_UNLINK;

#[derive(Debug)]
pub struct Event {
    pub wd: i32,
    pub mask: u32,
    pub name: Option<OsString>,
}

pub struct Inotify {
    fd: RawFd,
    buf: Vec<u8>,
}

impl Inotify {
    pub fn new() -> io::Result<Self> {
        // SAFETY: plain syscall; the returned fd is owned by this struct.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, buf: vec![0; 256 * 1024] })
    }

    pub fn add(&self, path: &Path) -> io::Result<i32> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
        if wd < 0 { Err(io::Error::last_os_error()) } else { Ok(wd) }
    }

    pub fn remove(&self, wd: i32) {
        // SAFETY: removing an unknown wd just returns EINVAL.
        unsafe {
            libc::inotify_rm_watch(self.fd, wd);
        }
    }

    /// Waits up to `timeout_ms` for events. Returns true if some are ready.
    pub fn wait(&self, timeout_ms: i32) -> io::Result<bool> {
        let mut p = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: `p` is a valid pollfd array of length 1.
        let r = unsafe { libc::poll(&mut p, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(e) };
        }
        Ok(r > 0)
    }

    /// Reads all queued events without blocking.
    pub fn read(&mut self, out: &mut Vec<Event>) -> io::Result<()> {
        loop {
            // SAFETY: the buffer is valid for writes of its full length.
            let n = unsafe { libc::read(self.fd, self.buf.as_mut_ptr().cast(), self.buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::WouldBlock => return Ok(()),
                    io::ErrorKind::Interrupted => continue,
                    _ => return Err(e),
                }
            }
            let n = n as usize;
            if n == 0 {
                return Ok(());
            }
            let mut off = 0;
            while off + 16 <= n {
                let b = &self.buf[off..];
                let wd = i32::from_ne_bytes(b[0..4].try_into().unwrap());
                let mask = u32::from_ne_bytes(b[4..8].try_into().unwrap());
                let len = u32::from_ne_bytes(b[12..16].try_into().unwrap()) as usize;
                let name = if len > 0 && 16 + len <= b.len() {
                    let raw = &b[16..16 + len];
                    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                    Some(OsString::from_vec(raw[..end].to_vec()))
                } else {
                    None
                };
                out.push(Event { wd, mask, name });
                off += 16 + len;
            }
        }
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        // SAFETY: we own the fd.
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// Current per-user watch limit.
pub fn max_user_watches() -> usize {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(8192)
}

/// Watches currently held by this user's processes (other users' are not visible,
/// but the limit is per user anyway).
pub fn watches_in_use() -> usize {
    let Ok(procs) = std::fs::read_dir("/proc") else { return 0 };
    let mut n = 0;
    for p in procs.flatten() {
        if !p.file_name().as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fdinfo")) else { continue };
        for fd in fds.flatten() {
            if let Ok(s) = std::fs::read_to_string(fd.path()) {
                n += s.lines().filter(|l| l.starts_with("inotify wd:")).count();
            }
        }
    }
    n
}
