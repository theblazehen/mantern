//! The live part: keep the page's surface listening so a SEE ALSO chip opens
//! its page below, and leave on `q`.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use libmandoc_rs::MacroSet;

use crate::page::{self, Page};
use crate::tsp::Wire;
use crate::tty::{Input, Tty, take_input};
use crate::{leave_messages, missing_messages, page_messages};

pub struct Browser {
    /// The `man` that finds pages; asked again for every page followed.
    pub system_man: PathBuf,
    /// The terminal accepts stylesheets.
    pub styles: bool,
    /// `MANTERN_FOLD=0` keeps every section open.
    pub allow_fold: bool,
}

impl Browser {
    /// Whether a page fills more than the screen, so its sections start folded.
    pub fn fold(&self, page: &Page, rows: u16, cols: u16) -> bool {
        self.allow_fold && page.rendered_lines(cols as usize).is_some_and(|lines| lines + 2 > rows as usize)
    }

    /// Shows `first`, then follows chips until the user quits (`q`, ^C, ^D)
    /// or the terminal goes away.
    pub fn run<W: Write>(&self, tty: &mut Tty, wire: &mut Wire<W>, first: &Page) -> io::Result<()> {
        let mut count = 1;
        let mut id = format!("man{count}");
        let (rows, cols) = tty.size().unwrap_or((24, 80));
        send(wire, page_messages(&id, first, self.fold(first, rows, cols), self.styles))?;

        let debug = std::env::var_os("MANTERN_DEBUG").is_some();
        let mut buf = Vec::new();
        'session: while tty.wait_input(&mut buf)? {
            for input in take_input(&mut buf) {
                if debug {
                    eprintln!("mantern: input {input:?}");
                }
                match input {
                    Input::Key(b'q' | 0x03 | 0x04) => break 'session,
                    Input::Event(e) if e["ev"] == "action" && e["act"] == "man" && e["sf"] == id.as_str() => {
                        let Some(label) = e["value"].as_str() else { continue };
                        send(wire, leave_messages(&id))?;
                        count += 1;
                        id = format!("man{count}");
                        let (rows, cols) = tty.size().unwrap_or((24, 80));
                        send(wire, self.open(label, &id, rows, cols))?;
                    }
                    _ => {}
                }
            }
        }
        send(wire, leave_messages(&id))?;
        tty.drain(Duration::from_millis(150));
        Ok(())
    }

    fn open(&self, label: &str, id: &str, rows: u16, cols: u16) -> Vec<(char, serde_json::Value)> {
        let found = split_label(label)
            .and_then(|args| page::locate(&self.system_man, &args))
            .and_then(|path| page::parse(&path).ok())
            .filter(|page| page.document.macro_set != MacroSet::None);
        match found {
            Some(page) => page_messages(id, &page, self.fold(&page, rows, cols), self.styles),
            None => missing_messages(id, label, self.styles),
        }
    }
}

fn send<W: Write>(wire: &mut Wire<W>, messages: Vec<(char, serde_json::Value)>) -> io::Result<()> {
    for (verb, body) in &messages {
        wire.send(*verb, body)?;
    }
    wire.flush()
}

/// `tar(1)` as the arguments `man` takes: `1 tar`.
fn split_label(label: &str) -> Option<Vec<OsString>> {
    let (name, section) = label.strip_suffix(')')?.split_once('(')?;
    (!name.is_empty() && !name.starts_with('-')).then(|| vec![section.into(), name.into()])
}

#[cfg(test)]
mod tests {
    use super::split_label;

    #[test]
    fn labels_become_man_arguments() {
        assert_eq!(split_label("openssl-req(1ssl)").unwrap(), ["1ssl", "openssl-req"]);
        assert!(split_label("-k(1)").is_none());
        assert!(split_label("tar").is_none());
    }
}
