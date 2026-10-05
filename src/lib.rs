//! Native man pages for Tern: parse with libmandoc, draw over the Tern
//! Surface Protocol.

pub mod browse;
pub mod escape;
pub mod inline;
pub mod page;
pub mod render;
pub mod tsp;
pub mod tty;

use serde_json::{Value, json};

pub const STYLE: &str = include_str!("style.css");

const HINT: &str = "q closes this page · click a SEE ALSO chip to open it below";

/// The messages that open `page` as a live flow surface named `id`: open,
/// stylesheet, the document. It stays open (and listening) until
/// [`leave_messages`].
pub fn page_messages(id: &str, page: &page::Page, fold: bool, styles: bool) -> Vec<(char, Value)> {
    let mut main = render::document(&page.document, &render::Options { fold });
    let hint = json!({ "id": "hint", "k": "text", "p": { "spans": [{ "t": HINT, "s": "muted" }], "role": "mantern.hint" } });
    main["c"].as_array_mut().expect("the document is a col").push(hint);
    open(id, &title(&page.document), main, styles)
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

/// Closes surface `id` for good: the key hint goes, as nothing listens any
/// more, and the page stays in the scrollback.
pub fn leave_messages(id: &str) -> Vec<(char, Value)> {
    vec![
        ('f', json!({ "sf": id, "s": 2, "ops": [["del", "hint"]] })),
        ('x', json!({ "id": id, "keep": true })),
    ]
}

fn title(doc: &libmandoc_rs::Document) -> String {
    let m = &doc.metadata;
    let name = m.name.as_deref().or(m.title.as_deref()).unwrap_or("man");
    match &m.section {
        Some(section) => format!("man {name}({section})"),
        None => format!("man {name}"),
    }
}
