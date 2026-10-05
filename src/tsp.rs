//! Writes Tern Surface Protocol messages: APC framing, chunking at the
//! negotiated size, and an optional JSONL recording for `surface-play`.

use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::time::Instant;

use serde_json::{Value, json};

/// Tern abandons an APC string longer than this, everything after `ESC _`
/// included, so no chunk goes past it whatever `apc` says.
const MAX_STRING: usize = 262_144;

/// A chunked message is dropped once its joined body passes 24 MiB.
const MAX_JOINED: usize = 24 * 1024 * 1024;

pub struct Wire<W: Write> {
    out: W,
    apc: usize,
    record: Option<File>,
    started: Instant,
    chunk_id: u32,
}

/// Why a message didn't go out.
#[derive(Debug)]
pub enum SendError {
    /// The body can't be sent whole or chunked within the protocol's limits.
    Unsendable(String),
    Io(io::Error),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::Unsendable(why) => f.write_str(why),
            SendError::Io(err) => err.fmt(f),
        }
    }
}

impl From<io::Error> for SendError {
    fn from(err: io::Error) -> SendError {
        SendError::Io(err)
    }
}

impl From<SendError> for io::Error {
    fn from(err: SendError) -> io::Error {
        match err {
            SendError::Unsendable(why) => io::Error::new(io::ErrorKind::InvalidData, why),
            SendError::Io(err) => err,
        }
    }
}

impl<W: Write> Wire<W> {
    /// `MANTERN_TSP_RECORD=<file>` records every message sent, in the JSONL
    /// shape Tern's `surface-play` replays. `apc` is the largest body the
    /// terminal takes in one message.
    pub fn new(out: W, apc: usize) -> Wire<W> {
        let record = std::env::var_os("MANTERN_TSP_RECORD").and_then(|path| File::create(path).ok());
        Wire { out, apc, record, started: Instant::now(), chunk_id: 0 }
    }

    pub fn send(&mut self, verb: char, body: &Value) -> Result<(), SendError> {
        let body_text = body.to_string();
        self.record(verb, body);
        let bytes = body_text.as_bytes();
        let single = format!("tsp;{verb};").len();
        if bytes.len() <= self.apc.min(MAX_STRING - single - ST) {
            write!(self.out, "\x1b_tsp;{verb};")?;
            self.out.write_all(bytes)?;
            self.out.write_all(b"\x1b\\")?;
            return Ok(());
        }
        self.chunk_id += 1;
        let id = format!("k{}", self.chunk_id);
        let header = format!("tsp;{verb};c={id};m=1;").len();
        // Planned before the first byte goes out, so a body that can't be
        // split leaves the terminal's input untouched.
        let hard = MAX_STRING - header - ST;
        let chunks = plan(bytes, self.apc.min(hard), hard).map_err(SendError::Unsendable)?;
        let last = chunks.len() - 1;
        for (i, chunk) in chunks.iter().enumerate() {
            let more = if i == last { "" } else { "m=1;" };
            write!(self.out, "\x1b_tsp;{verb};c={id};{more}")?;
            if let Some(escape) = &chunk.escape {
                self.out.write_all(escape)?;
            }
            self.out.write_all(&bytes[chunk.range.clone()])?;
            self.out.write_all(b"\x1b\\")?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// The recording is a debugging aid: when its file fails (a full disk),
    /// it stops, and the terminal's session carries on unaffected.
    fn record(&mut self, verb: char, body: &Value) {
        let Some(rec) = &mut self.record else { return };
        let line = json!({
            "t": self.started.elapsed().as_millis() as u64,
            "dir": "out",
            "verb": verb.to_string(),
            "params": {},
            "body": body,
        });
        if let Err(err) = writeln!(rec, "{line}") {
            // Stderr may be the very terminal in use; a failed warning is no failure.
            let _ = writeln!(io::stderr(), "mantern: recording stopped: {err}");
            self.record = None;
        }
    }
}

/// The 2 bytes of the `ESC \` that ends each message.
const ST: usize = 2;

/// One message of a chunked body: `escape`, then `range` of the body.
#[derive(Debug, PartialEq)]
struct Chunk {
    /// The `\u00XX` that replaces the byte a chunk was cut at.
    escape: Option<[u8; 6]>,
    range: std::ops::Range<usize>,
}

/// Where a JSON text stands: inside a string, and inside which escape.
#[derive(Clone, Copy, Default)]
struct Json {
    in_string: bool,
    after_backslash: bool,
    hex_left: u8,
}

impl Json {
    /// A byte that can be written as `\u00XX` without changing the text: a
    /// letter or digit in a string, outside every escape.
    fn escapable(self, b: u8) -> bool {
        self.in_string && !self.after_backslash && self.hex_left == 0 && b.is_ascii_alphanumeric()
    }

    fn step(&mut self, b: u8) {
        if !self.in_string {
            self.in_string = b == b'"';
        } else if self.hex_left > 0 {
            self.hex_left -= 1;
        } else if self.after_backslash {
            self.after_backslash = false;
            self.hex_left = if b == b'u' { 4 } else { 0 };
        } else if b == b'\\' {
            self.after_backslash = true;
        } else if b == b'"' {
            self.in_string = false;
        }
    }
}

/// A byte a chunk may start with: one outside `[A-Za-z0-9_-]` can't begin a
/// `key=value;` parameter, which Tern would take off the body.
fn safe(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Cuts `bytes` into chunks of at most `limit` bytes (`apc`), none of which
/// starts parameter-shaped. A cut goes before the last safe byte that fits;
/// failing that, at a letter or digit inside a JSON string, which is
/// rewritten as a `\u00XX` escape so the next chunk starts with a
/// backslash; failing that, at the nearest safe byte past `limit`, up to
/// `hard`. A body no such cut can split is an error: a wrong cut would
/// silently change the text Tern joins.
fn plan(bytes: &[u8], limit: usize, hard: usize) -> Result<Vec<Chunk>, String> {
    if bytes.len() > MAX_JOINED {
        return Err(format!("a {} byte message is over the {MAX_JOINED} bytes Tern joins", bytes.len()));
    }
    let mut chunks = Vec::new();
    let mut at = 0;
    let mut escape: Option<[u8; 6]> = None;
    let mut state = Json::default();
    while at < bytes.len() {
        let used = escape.map_or(0, |e| e.len());
        let room = limit.saturating_sub(used);
        let reach = hard.saturating_sub(used);
        if bytes.len() - at <= room {
            chunks.push(Chunk { escape, range: at..bytes.len() });
            break;
        }
        // (cut, state before `bytes[cut]`)
        let mut safe_cut = None;
        let mut escape_cut = None;
        let mut scan = state;
        for (cut, &b) in bytes.iter().enumerate().take(at + room + 1).skip(at) {
            if cut > at {
                if safe(b) {
                    safe_cut = Some((cut, scan));
                } else if scan.escapable(b) {
                    escape_cut = Some((cut, scan));
                }
            }
            scan.step(b);
        }
        let (cut, next, rewritten) = if let Some((cut, s)) = safe_cut {
            (cut, s, None)
        } else if let Some((cut, s)) = escape_cut {
            (cut, s, Some(unicode_escape(bytes[cut])))
        } else {
            // Past the limit: the first safe byte, or the end if the rest fits.
            let end = (at + reach).min(bytes.len());
            if end == bytes.len() {
                chunks.push(Chunk { escape, range: at..end });
                break;
            }
            let cut = (at + room + 1..=end)
                .find(|&i| safe(bytes[i]))
                .ok_or_else(|| format!("no place to split the message within {hard} bytes"))?;
            let mut s = state;
            bytes[at..cut].iter().for_each(|&b| s.step(b));
            (cut, s, None)
        };
        chunks.push(Chunk { escape, range: at..cut });
        state = next;
        match rewritten {
            Some(e) => {
                escape = Some(e);
                at = cut + 1;
            }
            None => {
                escape = None;
                at = cut;
            }
        }
    }
    Ok(chunks)
}

fn unicode_escape(b: u8) -> [u8; 6] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    [b'\\', b'u', b'0', b'0', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]
}

#[cfg(test)]
mod tests {
    use super::{Json, Wire, plan, safe};
    use serde_json::{Value, json};

    /// What Tern would join: every chunk's body, `\u00XX` escapes included.
    fn joined(bytes: &[u8], chunks: &[super::Chunk]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(chunk.escape.iter().flatten());
            out.extend_from_slice(&bytes[chunk.range.clone()]);
        }
        out
    }

    #[test]
    fn chunks_never_start_with_a_parameter_shaped_segment() {
        let body = br#"{"t":"abc=1;defghij klmno pqrst=2;uvwxyz"}"#;
        let chunks = plan(body, 8, 64).unwrap();
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            let first = chunk.escape.map_or(body[chunk.range.start], |e| e[0]);
            assert!(safe(first), "chunk starts with {:?}", first as char);
        }
        assert_eq!(joined(body, &chunks), body);
    }

    #[test]
    fn a_long_run_in_a_string_is_cut_with_a_unicode_escape() {
        let text = "a".repeat(100);
        let body = json!({ "t": text }).to_string();
        let chunks = plan(body.as_bytes(), 32, 1000).unwrap();
        assert!(chunks.iter().filter(|c| c.escape.is_some()).count() >= 2);
        let rejoined = String::from_utf8(joined(body.as_bytes(), &chunks)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&rejoined).unwrap()["t"], text.as_str());
        for chunk in &chunks {
            let len = chunk.escape.map_or(0, |e| e.len()) + chunk.range.len();
            assert!(len <= 32);
        }
    }

    #[test]
    fn only_letters_and_digits_outside_escapes_are_escapable() {
        let mut state = Json::default();
        let marks: Vec<bool> = br#""ab\u00e9\nx"#
            .iter()
            .map(|&b| {
                let escapable = state.escapable(b);
                state.step(b);
                escapable
            })
            .collect();
        // " a b \ u 0 0 e 9 \ n x
        assert_eq!(marks, [false, true, true, false, false, false, false, false, false, false, false, true]);
    }

    #[test]
    fn a_run_outside_strings_waits_for_a_safe_byte_within_the_hard_limit() {
        let body = format!("[{},{}]", "1".repeat(40), "2".repeat(40));
        let chunks = plan(body.as_bytes(), 16, 64).unwrap();
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.escape.is_none()));
        assert_eq!(joined(body.as_bytes(), &chunks), body.as_bytes());
        assert!(plan(body.as_bytes(), 16, 32).is_err());
    }

    #[test]
    fn bodies_over_the_joined_limit_are_refused() {
        let body = vec![b' '; super::MAX_JOINED + 1];
        assert!(plan(&body, 65_536, 262_000).is_err());
    }

    #[test]
    fn large_bodies_split_into_numbered_chunks_that_rejoin() {
        let text = "word ".repeat(1000);
        let body = json!({ "text": text });
        let mut out = Vec::new();
        Wire::new(&mut out, 1024).send('f', &body).unwrap();
        let wire = String::from_utf8(out).unwrap();
        let messages: Vec<&str> = wire.split("\x1b\\").filter(|m| !m.is_empty()).collect();
        assert!(messages.len() > 1);
        let mut joined = String::new();
        for (i, m) in messages.iter().enumerate() {
            let last = i == messages.len() - 1;
            let prefix = if last { "\x1b_tsp;f;c=k1;" } else { "\x1b_tsp;f;c=k1;m=1;" };
            joined.push_str(m.strip_prefix(prefix).expect("chunk header"));
        }
        assert_eq!(serde_json::from_str::<Value>(&joined).unwrap(), body);
    }

    #[test]
    fn the_negotiated_limit_is_honoured_below_1024() {
        let body = json!({ "text": "word ".repeat(100) });
        let mut out = Vec::new();
        Wire::new(&mut out, 256).send('f', &body).unwrap();
        let wire = String::from_utf8(out).unwrap();
        let bodies = wire.split("\x1b\\").filter(|m| !m.is_empty()).map(|m| {
            m.strip_prefix("\x1b_tsp;f;c=k1;m=1;").or_else(|| m.strip_prefix("\x1b_tsp;f;c=k1;")).unwrap().len()
        });
        assert!(bodies.max().unwrap() <= 256);
    }
}
