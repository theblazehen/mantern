//! Writes Tern Surface Protocol messages: APC framing, chunking at the
//! negotiated size, and an optional JSONL recording for `surface-play`.

use std::fs::File;
use std::io::{self, Write};
use std::time::Instant;

use serde_json::{Value, json};

pub struct Wire<W: Write> {
    out: W,
    apc: usize,
    record: Option<File>,
    started: Instant,
    chunk_id: u32,
}

impl<W: Write> Wire<W> {
    /// `MANTERN_TSP_RECORD=<file>` records every message sent, in the JSONL
    /// shape Tern's `surface-play` replays.
    pub fn new(out: W, apc: usize) -> Wire<W> {
        let record = std::env::var_os("MANTERN_TSP_RECORD").and_then(|path| File::create(path).ok());
        Wire { out, apc: apc.max(1024), record, started: Instant::now(), chunk_id: 0 }
    }

    pub fn send(&mut self, verb: char, body: &Value) -> io::Result<()> {
        let body_text = body.to_string();
        if let Some(rec) = &mut self.record {
            let line = json!({
                "t": self.started.elapsed().as_millis() as u64,
                "dir": "out",
                "verb": verb.to_string(),
                "params": {},
                "body": body,
            });
            writeln!(rec, "{line}")?;
        }
        let bytes = body_text.as_bytes();
        if bytes.len() <= self.apc {
            write!(self.out, "\x1b_tsp;{verb};")?;
            self.out.write_all(bytes)?;
            return self.out.write_all(b"\x1b\\");
        }
        self.chunk_id += 1;
        let id = format!("k{}", self.chunk_id);
        let mut rest = bytes;
        while !rest.is_empty() {
            let cut = split_point(rest, self.apc);
            let (chunk, tail) = rest.split_at(cut);
            let more = if tail.is_empty() { "" } else { "m=1;" };
            write!(self.out, "\x1b_tsp;{verb};c={id};{more}")?;
            self.out.write_all(chunk)?;
            self.out.write_all(b"\x1b\\")?;
            rest = tail;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Where to end a chunk of at most `max` bytes. The next chunk must not
/// start with something that parses as a `key=value;` parameter, so cut
/// before a byte outside `[A-Za-z0-9_-]`, which is always safe.
fn split_point(bytes: &[u8], max: usize) -> usize {
    if bytes.len() <= max {
        return bytes.len();
    }
    let safe = |b: u8| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    (1..=max).rev().find(|&i| safe(bytes[i])).unwrap_or(max)
}

#[cfg(test)]
mod tests {
    use super::{Wire, split_point};
    use serde_json::json;

    #[test]
    fn chunks_never_start_with_a_parameter_shaped_segment() {
        let body = "abc=1;defghij";
        let cut = split_point(body.as_bytes(), 8);
        assert!(cut <= 8);
        assert!(!body.as_bytes()[cut].is_ascii_alphanumeric());
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
        assert_eq!(serde_json::from_str::<serde_json::Value>(&joined).unwrap(), body);
    }
}
