//! Decodes the roff inline escapes libmandoc leaves in text nodes.
//!
//! libmandoc expands strings, registers and macros while parsing, but keeps
//! font changes, special characters and the other inline escapes in the text
//! for its formatters. This turns them into plain text runs tagged with the
//! font in effect, which is all a native renderer needs.

use libmandoc_rs::{SpecialCharacter, special_character};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Font {
    pub bold: bool,
    pub italic: bool,
}

impl Font {
    pub const ROMAN: Font = Font { bold: false, italic: false };
    pub const BOLD: Font = Font { bold: true, italic: false };
    pub const ITALIC: Font = Font { bold: false, italic: true };
    pub const BOLD_ITALIC: Font = Font { bold: true, italic: true };

    /// A roff font name or position, as `\f`, `.ft` and tbl use them.
    /// Constant-width faces read as their weight: the terminal is monospace.
    pub fn named(name: &str) -> Option<Font> {
        Some(match name {
            "R" | "1" | "C" | "CR" | "CW" | "L" | "LR" | "TR" | "HR" | "AR" => Font::ROMAN,
            "B" | "3" | "CB" | "LB" | "TB" | "HB" | "AB" => Font::BOLD,
            "I" | "2" | "CI" | "LI" | "TI" | "HI" | "AI" => Font::ITALIC,
            "BI" | "IB" | "4" | "CBI" | "TBI" | "HBI" | "ABI" => Font::BOLD_ITALIC,
            _ => return None,
        })
    }
}

/// The current and previous font, so `\fP` and `.ft` without an argument
/// can switch back.
#[derive(Clone, Copy, Debug, Default)]
pub struct FontState {
    pub current: Font,
    previous: Font,
}

impl FontState {
    pub fn new(font: Font) -> FontState {
        FontState { current: font, previous: font }
    }

    pub fn select(&mut self, name: &str) {
        let next = match name {
            "" | "P" => self.previous,
            _ => match Font::named(name) {
                Some(font) => font,
                None => return,
            },
        };
        self.previous = self.current;
        self.current = next;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub font: Font,
}

/// Decode `text`, starting in `fonts.current` and leaving the font it ends
/// in there, as roff does across input lines.
pub fn decode(text: &str, fonts: &mut FontState) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, runs: &mut Vec<Run>, font: Font| {
        if buf.is_empty() {
            return;
        }
        match runs.last_mut() {
            Some(last) if last.font == font => last.text.push_str(buf),
            _ => runs.push(Run { text: buf.clone(), font }),
        }
        buf.clear();
    };

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            buf.push(c);
            continue;
        }
        let Some(e) = chars.next() else { break };
        match e {
            'f' => {
                let name = arg(&mut chars);
                flush(&mut buf, &mut runs, fonts.current);
                fonts.select(&name);
            }
            '(' => {
                let name: String = chars.by_ref().take(2).collect();
                push_special(&mut buf, &name);
            }
            '[' => {
                let name = until(&mut chars, ']');
                push_special(&mut buf, &name);
            }
            'C' => {
                let name = delimited(&mut chars);
                push_special(&mut buf, &name);
            }
            'N' => {
                let code = delimited(&mut chars);
                if let Some(c) = code.parse::<u32>().ok().and_then(char::from_u32) {
                    buf.push(c);
                }
            }
            '-' => buf.push('-'),
            'e' | 'E' | '\\' => buf.push('\\'),
            '.' => buf.push('.'),
            '\'' => buf.push('\''),
            '`' => buf.push('`'),
            '_' => buf.push('_'),
            ' ' | '~' | '0' => buf.push('\u{a0}'),
            't' => buf.push('\t'),
            // Zero-width and motion-only escapes with no argument.
            '|' | '^' | '&' | ':' | '%' | '/' | ',' | ')' | 'c' | '{' | '}' | 'a' | 'd' | 'u'
            | 'r' | 'p' | 'z' => {}
            // Comments end the line.
            '"' | '#' => break,
            's' => skip_size(&mut chars),
            // One-name arguments: colors, fonts families, registers, strings.
            'm' | 'M' | 'F' | 'g' | 'k' | 'Y' | 'V' | '$' | 'O' => {
                arg(&mut chars);
            }
            'n' | '*' => {
                if chars.peek() == Some(&'+') || chars.peek() == Some(&'-') {
                    chars.next();
                }
                arg(&mut chars);
            }
            // Delimited arguments: motions, widths, drawing, device controls.
            'h' | 'v' | 'w' | 'o' | 'b' | 'x' | 'X' | 'Z' | 'D' | 'l' | 'L' | 'R' | 'S' | 'H'
            | 'A' | 'B' => {
                delimited(&mut chars);
            }
            other => buf.push(other),
        }
    }
    flush(&mut buf, &mut runs, fonts.current);
    runs
}

fn push_special(buf: &mut String, name: &str) {
    if let Some(hex) = name.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && let Some(c) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
    {
        buf.push(c);
        return;
    }
    if let Some(SpecialCharacter::Visible(c)) = special_character(name) {
        buf.push(c);
    }
}

/// `x`, `(xy` or `[name]`.
fn arg(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    match chars.next() {
        Some('(') => chars.by_ref().take(2).collect(),
        Some('[') => until(chars, ']'),
        Some(c) => c.to_string(),
        None => String::new(),
    }
}

/// `'text'` with any delimiter character.
fn delimited(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    match chars.next() {
        Some(delim) => until(chars, delim),
        None => String::new(),
    }
}

fn until(chars: &mut std::iter::Peekable<std::str::Chars>, end: char) -> String {
    let mut out = String::new();
    for c in chars.by_ref() {
        if c == end {
            break;
        }
        out.push(c);
    }
    out
}

/// `\sN`, `\s±N`, `\s(NN`, `\s[N]`, `\s'N'`.
fn skip_size(chars: &mut std::iter::Peekable<std::str::Chars>) {
    if matches!(chars.peek(), Some('+') | Some('-')) {
        chars.next();
    }
    match chars.peek() {
        Some('(') => {
            chars.next();
            chars.next();
            chars.next();
        }
        Some('[') => {
            chars.next();
            until(chars, ']');
        }
        Some('\'') => {
            chars.next();
            until(chars, '\'');
        }
        Some(c) if c.is_ascii_digit() => {
            let first = chars.next();
            // Sizes 1-3 take a second digit (\s10 .. \s39), as in groff.
            if matches!(first, Some('1'..='3')) && chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(text: &str) -> Vec<(String, Font)> {
        decode(text, &mut FontState::default()).into_iter().map(|r| (r.text, r.font)).collect()
    }

    #[test]
    fn fonts_switch_and_return() {
        assert_eq!(
            decoded(r"\fB\-a\fR, \fI\-\-all\fP!"),
            [
                ("-a".into(), Font::BOLD),
                (", ".into(), Font::ROMAN),
                ("--all".into(), Font::ITALIC),
                ("!".into(), Font::ROMAN),
            ]
        );
    }

    #[test]
    fn specials_and_dropped_escapes() {
        assert_eq!(decoded(r"\(lqquoted\(rq \[u2713] a\&b\ c\e"), [("“quoted” ✓ ab\u{a0}c\\".into(), Font::ROMAN)]);
        assert_eq!(decoded(r"size\s-2small\s0 \m[red]x"), [("sizesmall x".into(), Font::ROMAN)]);
    }

    #[test]
    fn font_carries_across_lines() {
        let mut fonts = FontState::default();
        decode(r"\fBbold", &mut fonts);
        assert_eq!(decode("still", &mut fonts)[0].font, Font::BOLD);
    }
}
