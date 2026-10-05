//! The controlling terminal: raw input for the TSP handshake, and its size.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use serde_json::Value;

/// `/dev/tty` in raw input mode, restored on drop.
pub struct Tty {
    file: File,
    saved: libc::termios,
    /// The terminal hung up.
    hup: bool,
}

impl Tty {
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
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::IEXTEN | libc::ISIG);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: as above.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Tty { file, saved, hup: false })
    }

    /// Rows and columns of the terminal.
    pub fn size(&self) -> Option<(u16, u16)> {
        // SAFETY: winsize is plain data; TIOCGWINSZ fills it.
        let mut ws = unsafe { std::mem::zeroed::<libc::winsize>() };
        let ok = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) } == 0;
        (ok && ws.ws_col > 0).then_some((ws.ws_row, ws.ws_col))
    }

    fn read_until(&mut self, deadline: Instant, buf: &mut Vec<u8>) -> io::Result<bool> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            return if err.kind() == io::ErrorKind::Interrupted { Ok(true) } else { Err(err) };
        }
        if ready == 0 {
            return Ok(false);
        }
        let mut chunk = [0u8; 4096];
        let n = self.file.read(&mut chunk)?;
        buf.extend_from_slice(&chunk[..n]);
        self.hup |= n == 0 && pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0;
        Ok(true)
    }

    /// Ask whether the terminal speaks TSP: the `hello` query, then DA1,
    /// which every terminal answers. The reply comes before the DA1 answer
    /// or not at all.
    pub fn hello(&mut self, out: &mut impl Write, query: &str, timeout: Duration) -> io::Result<Option<Value>> {
        write!(out, "\x1b_tsp;q;{query}\x1b\\\x1b[c")?;
        out.flush()?;
        let deadline = Instant::now() + timeout;
        let mut buf = Vec::new();
        loop {
            if let Some(reply) = scan(&buf) {
                return Ok(reply);
            }
            if !self.read_until(deadline, &mut buf)? {
                return Ok(None);
            }
        }
    }

    /// Reads and discards input for `window`, so replies and events still in
    /// flight never reach the shell as typed text.
    pub fn drain(&mut self, window: Duration) {
        let deadline = Instant::now() + window;
        let mut sink = Vec::new();
        while self.read_until(deadline, &mut sink).unwrap_or(false) {
            sink.clear();
        }
    }

    /// Blocks for more input, appended to `buf`. `false` once the terminal
    /// hung up.
    pub fn wait_input(&mut self, buf: &mut Vec<u8>) -> io::Result<bool> {
        let before = buf.len();
        while buf.len() == before && !self.hup {
            self.read_until(Instant::now() + Duration::from_secs(3600), buf)?;
        }
        Ok(buf.len() > before)
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read in open_raw on the same fd.
        unsafe {
            libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.saved);
        }
    }
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
        if rest.starts_with(EVENT) {
            if let Ok(event) = serde_json::from_slice(&rest[EVENT.len()..end - 2]) {
                out.push(Input::Event(event));
            }
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
