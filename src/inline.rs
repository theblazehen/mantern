//! Collects a paragraph's words into styled spans, with roff's spacing rules
//! and links found in the text.

use serde_json::{Value, json};

use crate::escape::Font;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub font: Font,
    pub href: Option<String>,
}

impl Span {
    fn to_json(&self) -> Value {
        let mut style = Vec::new();
        if self.font.bold {
            style.push("strong");
        }
        if self.font.italic {
            style.push("em");
        }
        match (&self.href, style.is_empty()) {
            (None, true) => Value::String(self.text.clone()),
            (None, false) => json!({ "t": self.text, "s": style.join(" ") }),
            (Some(href), _) => {
                let mut span = json!({ "t": self.text, "href": href });
                if !style.is_empty() {
                    span["s"] = Value::String(style.join(" "));
                }
                span
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Inline {
    spans: Vec<Span>,
    /// The next word attaches without a space (roff `\c`, mdoc `Ns`,
    /// an opening delimiter).
    glue: bool,
    /// mdoc `Sm off`: no spaces between words until `Sm on`. The first
    /// word after `Sm off` is still spaced from what came before.
    spacing_off: bool,
    space_once: bool,
    /// The next word starts a new output line (a no-fill source line).
    pending_newline: bool,
    /// No-fill text: source lines and spaces are kept.
    pub nofill: bool,
}

impl Inline {
    pub fn new(nofill: bool) -> Inline {
        Inline { nofill, ..Inline::default() }
    }

    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// A node that begins a source line: in no-fill text the next word goes
    /// on a new output line.
    pub fn line_start(&mut self) {
        if self.nofill && !self.spans.is_empty() {
            self.pending_newline = true;
        }
    }

    pub fn glue_next(&mut self) {
        self.glue = true;
    }

    /// An explicit line break (`.br`, `.ti`).
    pub fn newline(&mut self) {
        if !self.spans.is_empty() {
            self.raw("\n", Font::ROMAN, None);
        }
        self.glue = true;
        self.pending_newline = false;
    }

    /// Start a word: the space or newline that separates it from what came
    /// before. `close` marks closing punctuation, which attaches.
    pub fn sep(&mut self, close: bool) {
        if self.pending_newline {
            self.pending_newline = false;
            self.raw("\n", Font::ROMAN, None);
            self.glue = false;
            return;
        }
        let ends_open = self
            .spans
            .last()
            .and_then(|s| s.text.chars().last())
            .is_none_or(|c| c == '\n' || (!self.nofill && c.is_whitespace()));
        let spacing = !self.spacing_off || std::mem::take(&mut self.space_once);
        if !ends_open && !self.glue && !close && spacing {
            self.raw(" ", Font::ROMAN, None);
        }
        self.glue = false;
    }

    /// mdoc `Sm`: `Some(on)` sets spacing, `None` toggles it.
    pub fn set_spacing(&mut self, on: Option<bool>) {
        let on = on.unwrap_or(self.spacing_off);
        self.space_once = !on && !self.spacing_off;
        self.spacing_off = !on;
    }

    /// One word (or a run of words from one source line), separated from
    /// what precedes it.
    pub fn word(&mut self, text: &str, font: Font, href: Option<&str>, close: bool) {
        if text.is_empty() {
            return;
        }
        self.sep(close);
        self.raw(text, font, href);
    }

    /// Text appended with no spacing logic.
    pub fn raw(&mut self, text: &str, font: Font, href: Option<&str>) {
        if text.is_empty() {
            return;
        }
        let href = href.map(str::to_owned);
        match self.spans.last_mut() {
            Some(last) if last.font == font && last.href == href => last.text.push_str(text),
            _ => self.spans.push(Span { text: text.to_owned(), font, href }),
        }
    }

    /// The finished spans, trimmed and with references and URLs linked;
    /// resets the builder (keeping its mode).
    pub fn take(&mut self) -> Vec<Value> {
        let mut spans = std::mem::take(&mut self.spans);
        self.glue = false;
        self.pending_newline = false;
        trim(&mut spans, self.nofill);
        autolink(&mut spans);
        spans.iter().map(Span::to_json).collect()
    }

    /// The visible text, for headings and classification.
    pub fn plain(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

fn trim(spans: &mut Vec<Span>, nofill: bool) {
    while let Some(last) = spans.last_mut() {
        let trimmed = last.text.trim_end().len();
        last.text.truncate(trimmed);
        if last.text.is_empty() {
            spans.pop();
        } else {
            break;
        }
    }
    // No-fill text keeps its indentation; filled text starts at its first word.
    let lead = |c: char| if nofill { c == '\n' } else { c.is_whitespace() };
    while let Some(first) = spans.first_mut() {
        let start = first.text.len() - first.text.trim_start_matches(lead).len();
        first.text.drain(..start);
        if first.text.is_empty() {
            spans.remove(0);
        } else {
            break;
        }
    }
}

/// Links `name(section)` references and `http(s)://` URLs in the text,
/// splitting spans at the link boundaries. Spans that already link keep
/// their target.
fn autolink(spans: &mut Vec<Span>) {
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let links = find_links(&text);
    if links.is_empty() {
        return;
    }
    // Spans, boundaries and links are all visited in text order, so cursors
    // only move forward: a paragraph with thousands of links stays linear.
    let mut bounds: Vec<usize> = links.iter().flat_map(|&(a, b, _)| [a, b]).collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut next_bound = 0;
    let mut first_link = 0;
    let mut out = Vec::with_capacity(spans.len() + links.len() * 2);
    let mut offset = 0;
    for span in spans.drain(..) {
        let (start, end) = (offset, offset + span.text.len());
        offset = end;
        while bounds.get(next_bound).is_some_and(|&at| at <= start) {
            next_bound += 1;
        }
        let inner = bounds[next_bound..].iter().take_while(|&&at| at < end);
        let cuts: Vec<usize> = std::iter::once(start).chain(inner.copied()).chain(std::iter::once(end)).collect();
        next_bound += cuts.len() - 2;
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            // Links that end before this piece can't cover it or anything later.
            while links.get(first_link).is_some_and(|&(_, lb, _)| lb <= a) {
                first_link += 1;
            }
            let href = span.href.clone().or_else(|| {
                links[first_link..]
                    .iter()
                    .take_while(|&&(la, _, _)| la <= a)
                    .find(|&&(_, lb, _)| b <= lb)
                    .map(|(_, _, h)| h.clone())
            });
            out.push(Span {
                text: span.text[a - start..b - start].to_owned(),
                font: span.font,
                href,
            });
        }
    }
    // Re-merge neighbours the cuts left identical.
    let mut merged: Vec<Span> = Vec::with_capacity(out.len());
    for s in out {
        match merged.last_mut() {
            Some(last) if last.font == s.font && last.href == s.href => last.text.push_str(&s.text),
            _ => merged.push(s),
        }
    }
    *spans = merged;
}

/// Byte ranges and targets of the links in `text`.
pub fn find_links(text: &str) -> Vec<(usize, usize, String)> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();

    let mut search = 0;
    while let Some(found) = text[search..].find("http") {
        let start = search + found;
        let rest = &text[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            search = start + 4;
            continue;
        }
        let mut end = start
            + rest
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\u{a0}'))
                .unwrap_or(rest.len());
        // Trailing punctuation isn't part of the URL, nor is a `)` with no `(`
        // to close; the bracket counts are taken once and kept up to date.
        let url = &bytes[start..end];
        let opens = url.iter().filter(|&&c| c == b'(').count();
        let mut closes = url.iter().filter(|&&c| c == b')').count();
        while end > start {
            match bytes[end - 1] {
                b'.' | b',' | b';' | b':' | b'\'' => end -= 1,
                b')' if closes > opens => {
                    closes -= 1;
                    end -= 1;
                }
                _ => break,
            }
        }
        links.push((start, end, text[start..end].to_owned()));
        search = end.max(start + 1);
    }

    // URLs are found in text order and never overlap, so one cursor tells
    // whether a parenthesis falls inside one.
    let url_count = links.len();
    let mut url_cursor = 0;
    for (open, _) in text.match_indices('(') {
        while url_cursor < url_count && links[url_cursor].1 <= open {
            url_cursor += 1;
        }
        if url_cursor < url_count && links[url_cursor].0 <= open {
            continue;
        }
        // A section is at most six bytes, so the `)` is within seven.
        let window = &bytes[open + 1..bytes.len().min(open + 8)];
        let Some(close_rel) = window.iter().position(|&c| c == b')') else { continue };

        let section = &text[open + 1..open + 1 + close_rel];
        let valid_section = section.len() <= 6
            && section.starts_with(|c: char| c.is_ascii_digit() || c == 'n')
            && section.chars().all(|c| c.is_ascii_alphanumeric());
        if !valid_section {
            continue;
        }
        let name_start = text[..open]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '+'))
            .last()
            .map(|(i, _)| i);
        let Some(mut name_start) = name_start else { continue };
        // A name starts with a letter or digit, not punctuation.
        while name_start < open && !text[name_start..].starts_with(|c: char| c.is_alphanumeric()) {
            name_start += text[name_start..].chars().next().map_or(1, char::len_utf8);
        }
        if name_start >= open {
            continue;
        }
        let name = &text[name_start..open];
        let end = open + 2 + close_rel;
        links.push((name_start, end, format!("man:{name}({section})")));
    }
    links.sort_by_key(|&(a, _, _)| a);
    links
}

#[cfg(test)]
mod tests {
    use super::{Inline, find_links};
    use crate::escape::Font;

    #[test]
    fn finds_references_and_urls() {
        let links = find_links("see ls(1), git-commit(1) and https://example.com/a(1). Also f(x) and (1).");
        let found: Vec<_> = links.iter().map(|(_, _, h)| h.as_str()).collect();
        assert_eq!(found, ["man:ls(1)", "man:git-commit(1)", "https://example.com/a(1)"]);
    }

    #[test]
    fn keeps_section_suffixes_and_trims_punctuation() {
        let text = "openssl-req(1ssl), (perlre(1)).";
        let found: Vec<_> = find_links(text).into_iter().map(|(a, b, h)| (&text[a..b], h)).collect();
        assert_eq!(
            found,
            [("openssl-req(1ssl)", "man:openssl-req(1ssl)".to_owned()), ("perlre(1)", "man:perlre(1)".to_owned())]
        );
    }

    #[test]
    fn pathological_paragraph_completes() {
        let mut inline = Inline::new(false);
        for i in 0..20_000 {
            let font = if i % 2 == 0 { Font::ROMAN } else { Font::ITALIC };
            inline.word(&format!("page{i}(1),"), font, None, false);
        }
        inline.word(&format!("http://example.com/{}", ")".repeat(20_000)), Font::ROMAN, None, false);
        inline.word(&"(".repeat(20_000), Font::ROMAN, None, false);
        let spans = inline.take();
        let linked = spans.iter().filter(|s| s["href"].is_string()).count();
        assert!(linked >= 20_000);
    }
}
