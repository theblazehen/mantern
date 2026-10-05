//! The live part: keep the page's surface listening so a SEE ALSO chip opens
//! its page below, and leave on `q`.

use std::convert::Infallible;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use libmandoc_rs::MacroSet;
use serde_json::Value;

use crate::page::{self, Page};
use crate::tsp::{SendError, Wire};
use crate::tty::{Input, Ready, Tty, take_input};
use crate::{is_section, leave_messages, missing_messages, page_messages};

type Messages = Vec<(char, Value)>;

/// How long the input drains once the surface has closed.
const DRAIN: Duration = Duration::from_millis(150);

pub struct Browser {
    /// The `man` that finds pages; asked again for every page followed.
    pub system_man: PathBuf,
    /// The terminal accepts stylesheets.
    pub styles: bool,
    /// `MANTERN_FOLD=0` keeps every section open.
    pub allow_fold: bool,
    /// The node kinds the terminal draws, when it said.
    pub kinds: Option<Vec<String>>,
    /// Frames the terminal lets a surface leave unacknowledged.
    pub credits: u64,
}

/// How a session ended.
#[derive(Debug)]
pub enum End {
    /// The user left (`q`, ^C, ^D).
    Quit,
    /// The first page can't be shown natively: the system `man` should run.
    /// The surface is closed and the input drained.
    Declined(String),
    /// The terminal hung up.
    Hangup,
    /// A termination signal arrived.
    Signal(i32),
    /// The terminal could not be read or written. Nothing sensible is left
    /// to run on it.
    Failed(io::Error),
}

impl From<io::Error> for End {
    fn from(err: io::Error) -> End {
        End::Failed(err)
    }
}

/// The surface on screen, and how many of its frames the terminal has yet to
/// acknowledge.
struct Live {
    n: u32,
    id: String,
    /// The `name(section)` this page was followed from; none for the first.
    label: Option<String>,
    /// A real page, not the "no manual entry" one.
    page: bool,
    /// Whether the surface was opened and not yet closed.
    open: bool,
    sent: u64,
    acked: u64,
}

impl Live {
    fn new(n: u32, label: Option<String>, page: bool) -> Live {
        Live { n, id: format!("man{n}"), label, page, open: false, sent: 0, acked: 0 }
    }

    /// A ack covers every frame up to its number.
    fn outstanding(&self) -> u64 {
        self.sent.saturating_sub(self.acked)
    }
}

impl Browser {
    /// Whether a page fills more than the screen, so its sections start folded.
    pub fn fold(&self, page: &Page, rows: u16, cols: u16) -> bool {
        self.allow_fold && page.rendered_lines(cols as usize).is_some_and(|lines| lines + 2 > rows as usize)
    }

    /// Shows `first`, then follows chips until the user quits (`q`, ^C, ^D)
    /// or the terminal goes away. However it ends, the surface it left open
    /// is closed and the input drained before this returns, so replies and
    /// events in flight never reach whatever reads the terminal next.
    pub fn run<W: Write>(&self, tty: &mut Tty, wire: &mut Wire<W>, first: &Page) -> End {
        let mut live = Live::new(1, None, true);
        let mut end = match self.drive(tty, wire, first, &mut live) {
            Err(end) => end,
            Ok(never) => match never {},
        };
        if live.open {
            // A page the terminal can't take is removed; any other keeps its place in the scrollback.
            let keep = !matches!(end, End::Declined(_));
            if let Err(err) = self.close(wire, &mut live, keep)
                && matches!(end, End::Quit)
            {
                end = End::Failed(err.into());
            }
        }
        if !tty.hung_up() {
            tty.drain(DRAIN);
        }
        end
    }

    /// Runs the session until something ends it; that is the `Err`.
    fn drive<W: Write>(
        &self,
        tty: &mut Tty,
        wire: &mut Wire<W>,
        first: &Page,
        live: &mut Live,
    ) -> Result<Infallible, End> {
        let (rows, cols) = tty.size().unwrap_or((24, 80));
        let messages = page_messages(&live.id, first, self.fold(first, rows, cols), self.styles, self.kinds.as_deref())
            .map_err(End::Declined)?;
        self.show(wire, live, messages).map_err(|err| match err {
            SendError::Unsendable(why) => End::Declined(why),
            SendError::Io(err) => End::Failed(err),
        })?;

        let debug = std::env::var_os("MANTERN_DEBUG").is_some();
        let mut buf = Vec::new();
        loop {
            match tty.wait_input(&mut buf)? {
                Ready::Data | Ready::Timeout => {}
                Ready::Hangup => return Err(End::Hangup),
                Ready::Signal => return Err(End::Signal(tty.signal().unwrap_or(libc::SIGTERM))),
            }
            for input in take_input(&mut buf) {
                if debug {
                    eprintln!("mantern: input {input:?}");
                }
                match input {
                    Input::Key(b'q' | 0x03 | 0x04) => return Err(End::Quit),
                    Input::Event(e) if e["ev"] == "ack" && e["sf"] == live.id.as_str() => {
                        live.acked = live.acked.max(e["s"].as_u64().unwrap_or(0));
                    }
                    Input::Event(e) if e["ev"] == "action" && e["act"] == "man" && e["sf"] == live.id.as_str() => {
                        let Some(label) = e["value"].as_str() else { continue };
                        self.close(wire, live, true)?;
                        self.follow(tty, wire, live, label)?;
                    }
                    Input::Event(e) => {
                        if let Some(why) = rejection(&e, live) {
                            if live.n == 1 {
                                return Err(End::Declined(format!("the terminal rejected the page: {why}")));
                            }
                            if debug {
                                eprintln!("mantern: the terminal rejected {}: {why}", live.id);
                            }
                            self.missing(wire, live)?;
                        }
                    }
                    Input::Key(_) => {}
                }
            }
        }
    }

    /// Opens the page `label` names below, replacing `live` with it.
    fn follow<W: Write>(&self, tty: &Tty, wire: &mut Wire<W>, live: &mut Live, label: &str) -> Result<(), End> {
        let (rows, cols) = tty.size().unwrap_or((24, 80));
        *live = Live::new(live.n + 1, Some(label.to_owned()), true);
        match self.page_for(label, &live.id, rows, cols) {
            Some(messages) => match self.show(wire, live, messages) {
                Ok(()) => Ok(()),
                Err(SendError::Unsendable(_)) => self.missing(wire, live),
                Err(SendError::Io(err)) => Err(End::Failed(err)),
            },
            None => {
                live.page = false;
                self.show(wire, live, missing_messages(&live.id, label, self.styles))?;
                Ok(())
            }
        }
    }

    /// Takes the page that can't be shown off the screen and puts the "no
    /// manual entry" one in its place.
    fn missing<W: Write>(&self, wire: &mut Wire<W>, live: &mut Live) -> Result<(), End> {
        if live.open {
            self.close(wire, live, false)?;
        }
        let label = live.label.clone().unwrap_or_default();
        *live = Live::new(live.n + 1, Some(label.clone()), false);
        self.show(wire, live, missing_messages(&live.id, &label, self.styles))?;
        Ok(())
    }

    /// The messages that open `label`'s page, if the system `man` has it, we
    /// can draw it, and the terminal can take it.
    fn page_for(&self, label: &str, id: &str, rows: u16, cols: u16) -> Option<Messages> {
        let page = split_label(label)
            .and_then(|args| page::locate(&self.system_man, &args))
            .and_then(|path| page::parse(&path).ok())
            .filter(|page| page.document.macro_set != MacroSet::None)?;
        match page_messages(id, &page, self.fold(&page, rows, cols), self.styles, self.kinds.as_deref()) {
            Ok(messages) => Some(messages),
            Err(why) => {
                if std::env::var_os("MANTERN_DEBUG").is_some() {
                    eprintln!("mantern: {label} not drawn: {why}");
                }
                None
            }
        }
    }

    /// Sends the messages that open `live`'s surface.
    fn show<W: Write>(&self, wire: &mut Wire<W>, live: &mut Live, messages: Messages) -> Result<(), SendError> {
        live.open = true;
        transmit(wire, live, messages)
    }

    /// Closes `live`'s surface. Dropping the key hint is cosmetic, so it
    /// goes only if a frame fits the terminal's credits: a window that hasn't
    /// acked (slow, or detached) is never waited for.
    fn close<W: Write>(&self, wire: &mut Wire<W>, live: &mut Live, keep: bool) -> Result<(), SendError> {
        let hint = (keep && live.outstanding() < self.credits).then_some(live.sent + 1);
        live.open = false;
        transmit(wire, live, leave_messages(&live.id, hint, keep))
    }
}

impl From<SendError> for End {
    fn from(err: SendError) -> End {
        End::Failed(err.into())
    }
}

/// Writes `messages`, counting the frames among them against the surface's
/// credits, and flushes.
fn transmit<W: Write>(wire: &mut Wire<W>, live: &mut Live, messages: Messages) -> Result<(), SendError> {
    for (verb, body) in &messages {
        if *verb == 'f' {
            live.sent = live.sent.max(body["s"].as_u64().unwrap_or(0));
        }
        wire.send(*verb, body)?;
    }
    wire.flush()?;
    Ok(())
}

/// Why Tern rejected the page's own frame, if `event` says it did: an
/// `error` for frame 1 (or for a message that names no frame) whose `add`
/// failed or that was dropped whole, before any ack. Errors about a
/// stylesheet or an element carry `sheet` or `id` and don't count.
fn rejection(event: &Value, live: &Live) -> Option<String> {
    if !live.page || live.acked > 0 || event["ev"] != "error" || event.get("sheet").is_some() || event.get("id").is_some() {
        return None;
    }
    let about_us = |key: &str, ours: Value| event.get(key).is_none_or(|v| *v == ours);
    (about_us("sf", Value::from(live.id.as_str())) && about_us("s", Value::from(1)) && about_us("op", Value::from(0)))
        .then(|| event["msg"].as_str().unwrap_or("no reason given").to_owned())
}

/// `tar(1)` as the arguments `man` takes: `1 tar`.
fn split_label(label: &str) -> Option<Vec<OsString>> {
    let (name, section) = label.strip_suffix(')')?.split_once('(')?;
    (!name.is_empty() && !name.starts_with('-') && is_section(section)).then(|| vec![section.into(), name.into()])
}

#[cfg(test)]
mod tests {
    use super::{Live, rejection, split_label};
    use serde_json::json;

    #[test]
    fn labels_become_man_arguments() {
        assert_eq!(split_label("openssl-req(1ssl)").unwrap(), ["1ssl", "openssl-req"]);
        assert_eq!(split_label("signal(7)").unwrap(), ["7", "signal"]);
        assert_eq!(split_label("pcap(3p)").unwrap(), ["3p", "pcap"]);
        assert_eq!(split_label("intro(n)").unwrap(), ["n", "intro"]);
        assert!(split_label("-k(1)").is_none());
        assert!(split_label("tar").is_none());
    }

    #[test]
    fn labels_with_option_looking_or_empty_sections_are_refused() {
        assert!(split_label("foo(--help)").is_none());
        assert!(split_label("foo(-k)").is_none());
        assert!(split_label("foo()").is_none());
        assert!(split_label("(1)").is_none());
    }

    #[test]
    fn only_the_pages_own_frame_errors_reject_it() {
        let live = Live::new(1, None, true);
        let rejected = |e: serde_json::Value| rejection(&e, &live);
        assert!(rejected(json!({ "ev": "error", "sf": "man1", "s": 1, "op": 0, "msg": "limit" })).is_some());
        assert_eq!(
            rejected(json!({ "ev": "error", "sf": "man1", "s": 1, "msg": "dropped" })).as_deref(),
            Some("dropped")
        );
        assert!(rejected(json!({ "ev": "error", "msg": "malformed f body: x" })).is_some());
        // Its reveal, a later frame, another surface, the stylesheet, an element.
        assert!(rejected(json!({ "ev": "error", "sf": "man1", "s": 1, "op": 1, "msg": "x" })).is_none());
        assert!(rejected(json!({ "ev": "error", "sf": "man1", "s": 2, "msg": "x" })).is_none());
        assert!(rejected(json!({ "ev": "error", "sf": "man2", "s": 1, "msg": "x" })).is_none());
        assert!(rejected(json!({ "ev": "error", "sf": "man1", "sheet": "mantern", "msg": "x" })).is_none());
        assert!(rejected(json!({ "ev": "error", "sf": "man1", "id": "a", "msg": "x" })).is_none());
        assert!(rejected(json!({ "ev": "ack", "sf": "man1", "s": 1 })).is_none());

        let mut acked = Live::new(1, None, true);
        acked.acked = 1;
        assert!(rejection(&json!({ "ev": "error", "sf": "man1", "s": 1, "msg": "x" }), &acked).is_none());
    }

    #[test]
    fn credits_count_frames_the_terminal_has_not_acked() {
        let mut live = Live::new(1, None, true);
        live.sent = 2;
        assert_eq!(live.outstanding(), 2);
        live.acked = 1;
        assert_eq!(live.outstanding(), 1);
        live.acked = 5;
        assert_eq!(live.outstanding(), 0);
    }
}
