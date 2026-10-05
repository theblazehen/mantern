//! Dev tool: write the TSP messages `mantern` would send for a page as a
//! recording Tern's `surface-play` replays, without a terminal.
//!
//! `cargo run --example surface -- PAGE [--fold] > page.jsonl`

use std::path::PathBuf;

use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path = None;
    let mut fold = false;
    for a in std::env::args().skip(1) {
        if a == "--fold" {
            fold = true;
        } else {
            path = Some(PathBuf::from(a));
        }
    }
    let path = path.ok_or("usage: surface PAGE [--fold]")?;
    let page = mantern::page::parse(&path)?;
    let mut messages = mantern::page_messages("man", &page, fold, true);
    messages.extend(mantern::leave_messages("man"));

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("page");
    let name = name.split('.').next().unwrap_or(name);
    println!("{}", json!({ "t": 0, "dir": "out", "verb": "pty", "body": format!("$ man {name}\r\n") }));
    let mut nodes = 0;
    let mut bytes = 0;
    for (verb, body) in &messages {
        if *verb == 'f' {
            nodes = count(&body["ops"][0][4]);
            bytes = body.to_string().len();
        }
        println!("{}", json!({ "t": 0, "dir": "out", "verb": verb.to_string(), "params": {}, "body": body }));
    }
    eprintln!("{}: {nodes} nodes, {bytes} bytes in the frame", path.display());
    Ok(())
}

fn count(node: &serde_json::Value) -> usize {
    1 + node["c"].as_array().map_or(0, |c| c.iter().map(count).sum())
}
