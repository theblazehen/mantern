//! Native man pages for Tern: parse with libmandoc, draw over the Tern
//! Surface Protocol.

pub mod browse;
pub mod escape;
pub mod inline;
pub mod page;
pub mod render;
pub mod tsp;
pub mod tty;

use std::io::{self, Write};

use serde_json::{Value, json};

pub const STYLE: &str = include_str!("style.css");

const HINT: &str = "q closes this page · click a SEE ALSO chip to open it below";

/// Tern draws at most this many levels. The surface root is level 1 and
/// `main` level 2; `main` is counted as 3, since the docs don't say whether
/// the root counts.
const MAX_DEPTH: usize = 64;
const MAIN_DEPTH: usize = 3;

/// Nodes per surface, the root included.
const MAX_NODES: usize = 200_000;

/// A chunked message joined past this is dropped.
const MAX_FRAME: usize = 24 * 1024 * 1024;

/// What the frame holds besides the document: `sf`, `s`, the `add` op around
/// it, the `reveal`.
const FRAME_ENVELOPE: usize = 1024;

/// Whether `s` names a man section as `man` takes it: `1`, `3p`, `1ssl`,
/// `n`. Never empty, never something `man` would read as an option.
pub fn is_section(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_digit() || c == 'n' || c == 'l')
        && s.len() <= 6
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Whether Tern would take `main` as the first frame of a surface: within
/// its depth, node and message limits, and made only of the `kinds` the
/// terminal listed in its `hello` reply (all of them, without a list). A
/// document it would reject, whole or by subtree, is refused here instead of
/// leaving an empty surface listening.
pub fn check_document(main: &Value, kinds: Option<&[String]>) -> Result<(), String> {
    let mut nodes = 1;
    let mut pending = vec![(main, MAIN_DEPTH)];
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(format!("deeper than the {MAX_DEPTH} levels Tern draws"));
        }
        nodes += 1;
        if nodes > MAX_NODES {
            return Err(format!("more than the {MAX_NODES} nodes Tern takes in a surface"));
        }
        let kind = node["k"].as_str().ok_or("a node without a kind")?;
        if kinds.is_some_and(|kinds| !kinds.iter().any(|k| k == kind)) {
            return Err(format!("the terminal doesn't draw `{kind}` nodes"));
        }
        if let Some(children) = node["c"].as_array() {
            pending.extend(children.iter().map(|child| (child, depth + 1)));
        }
    }
    let mut size = CountingWriter(FRAME_ENVELOPE);
    serde_json::to_writer(&mut size, main).map_err(|err| err.to_string())?;
    if size.0 > MAX_FRAME {
        return Err(format!("{} bytes, over the {MAX_FRAME} Tern joins", size.0));
    }
    Ok(())
}

struct CountingWriter(usize);

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The messages that open `page` as a live flow surface named `id`: open,
/// stylesheet, the document. It stays open (and listening) until
/// [`leave_messages`]. Fails, before anything is sent, when the terminal
/// couldn't take the document (see [`check_document`]).
pub fn page_messages(
    id: &str,
    page: &page::Page,
    fold: bool,
    styles: bool,
    kinds: Option<&[String]>,
) -> Result<Vec<(char, Value)>, String> {
    let mut main = render::document(&page.document, &render::Options { fold });
    let hint = json!({ "id": "hint", "k": "text", "p": { "spans": [{ "t": HINT, "s": "muted" }], "role": "mantern.hint" } });
    main["c"].as_array_mut().expect("the document is a col").push(hint);
    check_document(&main, kinds)?;
    Ok(open(id, &title(&page.document), main, styles))
}

/// A one-line page for a `name(section)` the system `man` doesn't have.
pub fn missing_messages(id: &str, label: &str, styles: bool) -> Vec<(char, Value)> {
    let text = json!({
        "id": "main", "k": "col", "c": [
            { "id": "missing", "k": "text", "p": { "spans": [
                { "t": "No manual entry for ", "s": "muted" },
                { "t": label, "s": "strong" },
            ], "role": "mantern.missing" } },
            { "id": "hint", "k": "text", "p": { "spans": [{ "t": HINT, "s": "muted" }], "role": "mantern.hint" } },
        ]
    });
    open(id, &format!("man {label}"), text, styles)
}

fn open(id: &str, title: &str, main: Value, styles: bool) -> Vec<(char, Value)> {
    let mut messages = vec![(
        'o',
        json!({ "id": id, "mode": "flow", "title": title, "role": "mantern.page" }),
    )];
    if styles {
        messages.push(('s', json!({ "sf": id, "name": "mantern", "css": STYLE })));
    }
    // A page taller than the pane would otherwise open at its bottom, where the cursor is.
    let first = main["c"][0]["id"].as_str().map(str::to_owned);
    let mut ops = vec![json!(["add", "main", id, null, main])];
    ops.extend(first.map(|first| json!(["reveal", first, "start"])));
    messages.push(('f', json!({ "sf": id, "s": 1, "ops": ops })));
    messages
}

/// Closes surface `id`: `keep` leaves the page in the scrollback, otherwise
/// it is removed. `hint_seq` is the next frame number of the surface when the
/// key hint should go first, as nothing listens any more; without it (no
/// credit to send a frame) the hint stays.
pub fn leave_messages(id: &str, hint_seq: Option<u64>, keep: bool) -> Vec<(char, Value)> {
    let mut messages = Vec::new();
    if let Some(seq) = hint_seq {
        messages.push(('f', json!({ "sf": id, "s": seq, "ops": [["del", "hint"]] })));
    }
    messages.push(('x', json!({ "id": id, "keep": keep })));
    messages
}

fn title(doc: &libmandoc_rs::Document) -> String {
    let m = &doc.metadata;
    let name = m.name.as_deref().or(m.title.as_deref()).unwrap_or("man");
    match &m.section {
        Some(section) => format!("man {name}({section})"),
        None => format!("man {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DEPTH, MAX_NODES, check_document, is_section};
    use serde_json::{Value, json};

    fn nest(levels: usize) -> Value {
        let mut node = json!({ "id": "leaf", "k": "text" });
        for i in 0..levels {
            node = json!({ "id": format!("n{i}"), "k": "col", "c": [node] });
        }
        node
    }

    #[test]
    fn depth_counts_the_wrappers_above_main() {
        // `main` itself is one level: it sits at 3 with the root and a margin above.
        assert!(check_document(&nest(MAX_DEPTH - 3), None).is_ok());
        assert!(check_document(&nest(MAX_DEPTH - 2), None).is_err());
    }

    #[test]
    fn nodes_are_capped_with_the_root_counted() {
        let leaves = |n: usize| json!({ "id": "main", "k": "col", "c": vec![json!({ "id": "x", "k": "text" }); n] });
        // The root and `main` take two.
        assert!(check_document(&leaves(MAX_NODES - 2), None).is_ok());
        assert!(check_document(&leaves(MAX_NODES - 1), None).is_err());
    }

    #[test]
    fn kinds_the_terminal_didnt_list_are_refused() {
        let doc = json!({ "id": "main", "k": "col", "c": [{ "id": "a", "k": "el", "p": { "tag": "table" } }] });
        let kinds = ["col".to_owned(), "text".to_owned()];
        assert!(check_document(&doc, Some(&kinds)).unwrap_err().contains("`el`"));
        assert!(check_document(&doc, None).is_ok());
        let kinds = ["col".to_owned(), "el".to_owned()];
        assert!(check_document(&doc, Some(&kinds)).is_ok());
    }

    #[test]
    fn a_frame_over_24_mib_is_refused() {
        let doc = json!({ "id": "main", "k": "col", "c": [{ "id": "a", "k": "text", "p": { "text": "x".repeat(25 * 1024 * 1024) } }] });
        assert!(check_document(&doc, None).unwrap_err().contains("bytes"));
    }

    #[test]
    fn sections_are_man_sections_not_options() {
        for ok in ["1", "3p", "1ssl", "n", "l", "8"] {
            assert!(is_section(ok), "{ok}");
        }
        for bad in ["", "--help", "-k", "1234567", "x", "1 2", "1;"] {
            assert!(!is_section(bad), "{bad}");
        }
    }
}
