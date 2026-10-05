//! The controlling terminal: raw input for the TSP handshake, and its size.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Signals that end the session. Raw mode turns `ISIG` off, so none of them
/// comes from the keyboard; they come from `kill`, a closing window, a
/// service manager.
const TERMINATION: [libc::c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

/// The first termination signal received, written by the handler.
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// Write end of the pipe the handler wakes the input loop through.
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// Only async-signal-safe work: atomics and `write(2)`, with `errno` left as
/// it was so the interrupted code sees its own error.
extern "C" fn on_signal(signal: libc::c_int) {
    let _ = SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
    let fd = WAKE.load(Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: the errno slot is thread-local; write only reads `byte`.
        unsafe {
            let errno = errno_slot();
            let saved = *errno;
            let byte = 1u8;
            libc::write(fd, (&raw const byte).cast(), 1);
            *errno = saved;
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
use libc::__errno_location as errno_slot;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
use libc::__error as errno_slot;

/// What a wait for the terminal ended with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ready {
    /// Bytes arrived and were appended.
    Data,
    /// The deadline passed.
    Timeout,
    /// The terminal hung up: it is gone, and nothing more will come.
    Hangup,
    /// A termination signal arrived; see [`Tty::signal`].
    Signal,
}

/// `/dev/tty` in raw input mode, restored on drop.
pub struct Tty {
    file: File,
    saved: libc::termios,
    /// Read end of the pipe the signal handler writes to; its write end is
    /// closed after the handlers are put back.
    wake: OwnedFd,
    wake_write: OwnedFd,
    /// The dispositions the handlers replaced.
    previous: Vec<(libc::c_int, libc::sigaction)>,
    hup: bool,
}

impl Tty {
    /// Opens the terminal and enters raw mode. Termination signals are caught
    /// first, so there is no moment in raw mode that a `SIGTERM` could end
    /// without [`Tty`] restoring the terminal.
    pub fn open_raw() -> io::Result<Tty> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open("/dev/tty")?;
        let fd = file.as_raw_fd();
        // SAFETY: fd is open for the lifetime of `file`; termios is plain data.
        let saved = unsafe {
            let mut t = std::mem::zeroed::<libc::termios>();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            t
        };
        let (wake, wake_write) = pipe()?;
        let mut tty = Tty { file, saved, wake, wake_write, previous: Vec::new(), hup: false };
        tty.catch_signals()?;
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::IEXTEN | libc::ISIG);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: as above.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(tty)
    }

    fn catch_signals(&mut self) -> io::Result<()> {
        SIGNAL.store(0, Ordering::SeqCst);
        WAKE.store(self.wake_write.as_raw_fd(), Ordering::SeqCst);
        for signal in TERMINATION {
            // SAFETY: sigaction is plain data; the handler is async-signal-safe.
            unsafe {
                let mut action = std::mem::zeroed::<libc::sigaction>();
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut action.sa_mask);
                let mut old = std::mem::zeroed::<libc::sigaction>();
                if libc::sigaction(signal, &action, &mut old) != 0 {
                    return Err(io::Error::last_os_error());
                }
                self.previous.push((signal, old));
            }
        }
        Ok(())
    }

    /// The termination signal that arrived, if one has.
    pub fn signal(&self) -> Option<i32> {
        Some(SIGNAL.load(Ordering::SeqCst)).filter(|&s| s != 0)
    }

    /// Whether the terminal hung up.
    pub fn hung_up(&self) -> bool {
        self.hup
    }

    /// Rows and columns of the terminal.
    pub fn size(&self) -> Option<(u16, u16)> {
        // SAFETY: winsize is plain data; TIOCGWINSZ fills it.
        let mut ws = unsafe { std::mem::zeroed::<libc::winsize>() };
        let ok = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) } == 0;
        (ok && ws.ws_col > 0).then_some((ws.ws_row, ws.ws_col))
    }

    /// Waits for input until `deadline` (forever without one), appending what
    /// arrives to `buf`. An interrupted wait resumes with the same deadline.
    /// With `watch`, a termination signal ends the wait too.
    fn ready(&mut self, deadline: Option<Instant>, watch: bool, buf: &mut Vec<u8>) -> io::Result<Ready> {
        loop {
            if self.hup {
                return Ok(Ready::Hangup);
            }
            let ms = match deadline {
                None => -1,
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(Ready::Timeout);
                    }
                    remaining.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32
                }
            };
            let mut fds = [
                libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            let count = if watch { 2 } else { 1 };
            // SAFETY: `count` valid pollfds.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, ms) };
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if ready == 0 {
                return Ok(Ready::Timeout);
            }
            if watch && fds[1].revents & libc::POLLIN != 0 {
                self.consume_wake();
                return Ok(Ready::Signal);
            }
            let revents = fds[0].revents;
            if revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "the terminal is not pollable"));
            }
            if revents & libc::POLLIN != 0 {
                let mut chunk = [0u8; 4096];
                match self.file.read(&mut chunk) {
                    Ok(0) if revents & (libc::POLLHUP | libc::POLLERR) == 0 => {
                        // VMIN=VTIME=0 lets a read return nothing without
                        // the terminal being gone.
                    }
                    Ok(0) => self.hup = true,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        return Ok(Ready::Data);
                    }
                    Err(err) if matches!(err.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => {}
                    // What a read on a hung-up tty gives on Linux.
                    Err(err) if err.raw_os_error() == Some(libc::EIO) => self.hup = true,
                    Err(err) => return Err(err),
                }
            } else if revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                self.hup = true;
            }
        }
    }

    /// Empties the wake pipe once its signal has been taken.
    fn consume_wake(&self) {
        let mut sink = [0u8; 16];
        // SAFETY: the pipe is non-blocking and `sink` is writable for its length.
        while unsafe { libc::read(self.wake.as_raw_fd(), sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
    }

    /// Ask whether the terminal speaks TSP: the `hello` query, then DA1,
    /// which every terminal answers. The reply comes before the DA1 answer
    /// or not at all.
    ///
    /// After `timeout` the reply no longer counts, but reading goes on for a
    /// grace period until the DA1 answer (the last thing owed), then drains
    /// briefly, so a slow link's late answers don't reach whatever runs
    /// next. A fixed wait can't promise an arbitrarily late answer never
    /// arrives. A termination signal or hangup ends the probe early; see
    /// [`Tty::signal`] and [`Tty::hung_up`].
    pub fn hello(&mut self, out: &mut impl Write, query: &str, timeout: Duration) -> io::Result<Option<Value>> {
        write!(out, "\x1b_tsp;q;{query}\x1b\\\x1b[c")?;
        out.flush()?;
        let deadline = Instant::now() + timeout;
        let mut buf = Vec::new();
        loop {
            if let Some(reply) = scan(&buf) {
                return Ok(reply);
            }
            match self.ready(Some(deadline), true, &mut buf)? {
                Ready::Data => {}
                Ready::Timeout => break,
                Ready::Hangup | Ready::Signal => return Ok(None),
            }
        }
        let grace = Instant::now() + HELLO_GRACE;
        loop {
            if scan(&buf).is_some() {
                break;
            }
            match self.ready(Some(grace), true, &mut buf)? {
                Ready::Data => {}
                Ready::Timeout => {
                    self.drain(DRAIN);
                    break;
                }
                Ready::Hangup | Ready::Signal => break,
            }
        }
        Ok(None)
    }

    /// Reads and discards input for `window`, so replies and events still in
    /// flight never reach the shell as typed text. Signals don't cut it
    /// short: this is the cleanup.
    pub fn drain(&mut self, window: Duration) {
        let deadline = Instant::now() + window;
        let mut sink = Vec::new();
        while let Ok(Ready::Data) = self.ready(Some(deadline), false, &mut sink) {
            sink.clear();
        }
    }

    /// Blocks until more input is appended to `buf`, the terminal hangs up
    /// or a termination signal arrives.
    pub fn wait_input(&mut self, buf: &mut Vec<u8>) -> io::Result<Ready> {
        self.ready(None, true, buf)
    }
}

/// How long [`Tty::hello`] keeps waiting for the DA1 answer after giving up
/// on the TSP reply.
const HELLO_GRACE: Duration = Duration::from_secs(1);

/// The drain after a probe that went unanswered.
const DRAIN: Duration = Duration::from_millis(150);

impl Drop for Tty {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read in open_raw on the same fd, and
        // the dispositions catch_signals replaced.
        unsafe {
            libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.saved);
            for (signal, old) in &self.previous {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
        WAKE.store(-1, Ordering::SeqCst);
    }
}

/// A non-blocking, close-on-exec pipe.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` holds the two descriptors pipe(2) writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are fresh and owned by nobody else.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&read, &write] {
        // SAFETY: fcntl on a descriptor we own.
        let ok = unsafe {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) == 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((read, write))
}

/// `Some(reply)` once the DA1 answer has arrived, with the TSP reply when
/// one preceded it; `None` while more input is needed.
fn scan(buf: &[u8]) -> Option<Option<Value>> {
    let text = String::from_utf8_lossy(buf);
    let da1 = text.find("\x1b[?")?;
    text[da1..].find('c')?;
    let reply = text[..da1].find("\x1b_tsp;r;").and_then(|start| {
        let body = &text[start + "\x1b_tsp;r;".len()..da1];
        let end = body.find("\x1b\\")?;
        serde_json::from_str::<Value>(&body[..end]).ok()
    });
    Some(reply.filter(|r| r["r"] == "hello"))
}

/// What the terminal sent while a surface listens.
#[derive(Debug, PartialEq)]
pub enum Input {
    Key(u8),
    /// A TSP event (`ESC _ tsp;e;{json} ESC \`).
    Event(Value),
}

/// Takes every complete key and event off the front of `buf`, leaving a
/// sequence that hasn't fully arrived. Other escape sequences (cursor keys,
/// foreign strings) are dropped whole, so their bytes never pass as keys.
pub fn take_input(buf: &mut Vec<u8>) -> Vec<Input> {
    const EVENT: &[u8] = b"\x1b_tsp;e;";
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let rest = &buf[i..];
        if rest[0] != 0x1b {
            out.push(Input::Key(rest[0]));
            i += 1;
            continue;
        }
        let Some(&kind) = rest.get(1) else { break };
        let end = match kind {
            b'_' | b'P' | b']' => {
                let st = rest.windows(2).position(|w| w == b"\x1b\\").map(|p| (p, 2));
                let bel = (kind == b']').then(|| rest.iter().position(|&b| b == 0x07).map(|p| (p, 1))).flatten();
                match (st, bel) {
                    (Some(a), Some(b)) => (a.0 + a.1).min(b.0 + b.1),
                    (Some((p, n)), None) | (None, Some((p, n))) => p + n,
                    (None, None) => break,
                }
            }
            b'[' => match rest[2..].iter().position(|b| (0x40..=0x7e).contains(b)) {
                Some(p) => p + 3,
                None => break,
            },
            _ => 2,
        };
        if rest.starts_with(EVENT)
            && let Ok(event) = serde_json::from_slice(&rest[EVENT.len()..end - 2])
        {
            out.push(Input::Event(event));
        }
        i += end;
    }
    buf.drain(..i);
    out
}

#[cfg(test)]
mod tests {
    use super::{Input, scan, take_input};

    #[test]
    fn waits_for_da1_and_takes_the_reply_before_it() {
        assert_eq!(scan(b"\x1b_tsp;r;{\"r\":\"hello\",\"v\":1}\x1b\\"), None);
        let done = scan(b"\x1b_tsp;r;{\"r\":\"hello\",\"v\":1,\"apc\":65536}\x1b\\\x1b[?62;22c").unwrap();
        assert_eq!(done.unwrap()["apc"], 65536);
        assert_eq!(scan(b"\x1b[?62;22c"), Some(None));
    }

    #[test]
    fn splits_keys_from_events_and_holds_partial_sequences() {
        let mut buf = b"q\x1b[A\x1b_tsp;e;{\"ev\":\"action\",\"act\":\"man\",\"value\":\"ls(1)\"}\x1b\\x\x1b_tsp;e;{\"ev".to_vec();
        let got = take_input(&mut buf);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], Input::Key(b'q'));
        assert!(matches!(&got[1], Input::Event(e) if e["value"] == "ls(1)"));
        assert_eq!(got[2], Input::Key(b'x'));
        assert_eq!(buf, b"\x1b_tsp;e;{\"ev");
    }
}
