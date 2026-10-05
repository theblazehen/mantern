//! Turns a libmandoc syntax tree into a Tern Surface Protocol document.
//!
//! Sections become folding `section`s, paragraphs styled `text` nodes,
//! tagged paragraphs and mdoc tag lists definition lists, displays
//! no-fill text and tbl(7) tables HTML tables. Whatever a page does that
//! has no native counterpart degrades to its text.

use std::collections::HashSet;

use libmandoc_rs::{Document, MacroSet, Node, NodeKind, NormalizedFont, NormalizedListKind, TableCellKind};
use serde_json::{Value, json};

use crate::escape::{self, Font, FontState};
use crate::inline::{Inline, find_links};

pub struct Options {
    /// Fold every section but NAME, SYNOPSIS and DESCRIPTION (long pages).
    pub fold: bool,
}

pub fn document(doc: &Document, opts: &Options) -> Value {
    let mut r = Renderer {
        set: doc.macro_set,
        next_id: 0,
        fonts: FontState::default(),
        page_name: doc.metadata.name.clone().or_else(|| doc.metadata.title.clone()).unwrap_or_default(),
        current: String::new(),
    };
    let mut children = vec![r.header(doc)];
    children.extend(r.sections(&doc.root, opts));
    let mut main = json!({ "id": "main", "k": "col", "c": children });
    route_links(&mut main);
    main
}

/// `man:name(section)` links can't be opened by a surface: a chip becomes a
/// click action the program hears (`man=name(section)`), and a link in
/// running text points at the page on the web.
fn route_links(node: &mut Value) {
    if let Some(label) = node["p"]["href"].as_str().and_then(|h| h.strip_prefix("man:")).map(str::to_owned) {
        node["p"].as_object_mut().map(|p| p.remove("href"));
        node["p"]["actions"] = json!({ "click": format!("man={label}") });
    }
    for span in node["p"]["spans"].as_array_mut().into_iter().flatten() {
        if let Some(url) = span["href"].as_str().and_then(|h| h.strip_prefix("man:")).and_then(web_page) {
            span["href"] = Value::String(url);
        }
    }
    for child in node["c"].as_array_mut().into_iter().flatten() {
        route_links(child);
    }
}

/// The web copy of `name(section)`.
fn web_page(label: &str) -> Option<String> {
    let (name, section) = label.strip_suffix(')')?.split_once('(')?;
    Some(format!("https://man.archlinux.org/man/{name}.{section}"))
}

struct Renderer {
    set: MacroSet,
    next_id: u64,
    fonts: FontState,
    page_name: String,
    /// Upper-cased title of the section being rendered.
    current: String,
}

/// Block-level output plus the paragraph being collected.
struct Flow {
    para: Inline,
    out: Vec<Value>,
    /// A `.HP` waiting to see whether a body-only `.IP` follows: some
    /// generators write a tagged entry that way.
    hp: Option<Node>,
}

impl Flow {
    fn new() -> Flow {
        Flow { para: Inline::new(false), out: Vec::new(), hp: None }
    }
}

impl Renderer {
    fn id(&mut self) -> String {
        self.next_id += 1;
        format!("n{}", radix36(self.next_id))
    }

    fn node(&mut self, kind: &str, props: Value, children: Vec<Value>) -> Value {
        let mut n = json!({ "id": self.id(), "k": kind, "p": props });
        if !children.is_empty() {
            n["c"] = Value::Array(children);
        }
        n
    }

    fn el(&mut self, tag: &str, class: &str, children: Vec<Value>) -> Value {
        let props = if class.is_empty() { json!({ "tag": tag }) } else { json!({ "tag": tag, "class": class }) };
        self.node("el", props, children)
    }

    fn text(&mut self, spans: Vec<Value>, role: &str) -> Value {
        let mut props = json!({ "spans": spans, "role": role });
        if role == ROLE_P {
            props["measure"] = json!("prose");
        }
        self.node("text", props, Vec::new())
    }

    // ------------------------------------------------------------ page

    fn header(&mut self, doc: &Document) -> Value {
        let m = &doc.metadata;
        let title = m.title.as_deref().unwrap_or("");
        let name = self.node(
            "text",
            json!({ "spans": [{ "t": title.to_lowercase(), "s": "strong accent" }], "role": "mantern.title", "wrap": "none" }),
            Vec::new(),
        );
        let mut left = vec![name];
        if let Some(section) = &m.section {
            left.push(self.node("badge", json!({ "text": section, "tone": "accent" }), Vec::new()));
        }
        // The volume, system and date, without repeating one in the other.
        let volume = m.volume.as_deref().filter(|v| !m.os.as_deref().is_some_and(|os| os.starts_with(v)));
        let meta: Vec<&str> = [volume, m.os.as_deref(), m.date.as_deref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case(title))
            .collect();
        let left = self.node("row", json!({ "gap": "sm", "align": "baseline" }), left);
        let right = self.node(
            "text",
            json!({ "spans": [{ "t": meta.join(" · "), "s": "muted" }], "truncate": "start" }),
            Vec::new(),
        );
        self.node("row", json!({ "justify": "between", "gap": "md", "role": "mantern.header" }), vec![left, right])
    }

    /// The NAME section as a lead: the names in accent, then what they do.
    fn name_lead(&mut self, n: &Node) -> Value {
        self.fonts = FontState::default();
        let mut inl = Inline::new(false);
        for c in part(n, NodeKind::Body) {
            self.inline(c, &mut inl);
        }
        let plain = inl.plain();
        let (names, what) = plain
            .split_once(" — ")
            .or_else(|| plain.split_once(" - "))
            .unwrap_or(("", plain.as_str()));
        let mut spans = Vec::new();
        if !names.is_empty() {
            spans.push(json!({ "t": names, "s": "strong accent" }));
            spans.push(json!({ "t": "  —  ", "s": "muted" }));
        }
        spans.push(Value::String(what.trim().to_owned()));
        self.node("text", json!({ "spans": spans, "role": "mantern.lead", "measure": "prose" }), Vec::new())
    }

    fn sections(&mut self, root: &Node, opts: &Options) -> Vec<Value> {
        let mut out = Vec::new();
        let mut preamble = Flow::new();
        for n in &root.children {
            if self.skip(n) {
                continue;
            }
            if n.kind == NodeKind::Block && matches!(n.macro_name.as_deref(), Some("SH" | "Sh")) {
                self.flush(&mut preamble);
                out.append(&mut preamble.out);
                if head_plain(&self.head_spans(n)).eq_ignore_ascii_case("NAME") {
                    out.push(self.name_lead(n));
                } else {
                    out.push(self.section(n, opts.fold));
                }
            } else {
                self.feed_one(n, &mut preamble);
            }
        }
        self.release_hp(&mut preamble);
        self.flush(&mut preamble);
        out.append(&mut preamble.out);
        out
    }

    fn section(&mut self, n: &Node, fold: bool) -> Value {
        self.fonts = FontState::default();
        let head = self.head_spans(n);
        let title: String = head_plain(&head);
        self.current = title.to_ascii_uppercase();
        let mut body = self.blocks(part(n, NodeKind::Body));
        match self.current.as_str() {
            "SYNOPSIS" => body = vec![self.el("div", "mt-synopsis", body)],
            "SEE ALSO" => {
                let mut out = Vec::new();
                for item in see_also_items(body) {
                    match item {
                        SeeAlso::Keep(node) => out.push(node),
                        SeeAlso::Chips(refs) => {
                            let chips: Vec<Value> = refs
                                .into_iter()
                                .map(|(label, href)| {
                                    self.node("badge", json!({ "text": label, "tone": "accent", "href": href }), Vec::new())
                                })
                                .collect();
                            out.push(self.node("row", json!({ "wrap": true, "gap": "sm" }), chips));
                        }
                    }
                }
                body = out;
            }
            _ => {}
        }
        let essential = matches!(self.current.as_str(), "NAME" | "SYNOPSIS" | "DESCRIPTION");
        self.node(
            "section",
            json!({
                "head": head, "collapsible": true, "collapsed": fold && !essential,
                "key": format!("sh:{title}"), "role": "mantern.h1",
            }),
            body,
        )
    }

    fn subsection(&mut self, n: &Node) -> Value {
        let head = self.head_spans(n);
        let body = self.blocks(part(n, NodeKind::Body));
        let title: String = head_plain(&head);
        self.node(
            "section",
            json!({
                "head": head, "collapsible": true, "collapsed": false,
                "key": format!("ss:{title}"), "role": "mantern.h2",
            }),
            body,
        )
    }

    fn head_spans(&mut self, n: &Node) -> Vec<Value> {
        let mut inl = Inline::new(false);
        let saved = self.fonts;
        self.fonts = FontState::new(Font::BOLD);
        for c in part(n, NodeKind::Head) {
            self.inline(c, &mut inl);
        }
        self.fonts = saved;
        inl.take()
    }

    // ------------------------------------------------------------ blocks

    fn blocks(&mut self, nodes: &[Node]) -> Vec<Value> {
        let mut flow = Flow::new();
        self.feed(nodes, &mut flow);
        self.release_hp(&mut flow);
        self.flush(&mut flow);
        flow.out
    }

    fn feed(&mut self, nodes: &[Node], flow: &mut Flow) {
        for n in nodes {
            self.feed_one(n, flow);
        }
    }

    fn flush(&mut self, flow: &mut Flow) {
        if flow.para.is_blank() {
            flow.para.take();
            return;
        }
        let spans = flow.para.take();
        let node = if flow.para.nofill {
            let lang = if matches!(self.current.as_str(), "SYNOPSIS" | "EXAMPLES" | "EXAMPLE") { "sh" } else { "text" };
            let text = spans_plain(&spans);
            self.node("code", json!({ "text": text.trim_end(), "lang": lang, "role": ROLE_PRE }), Vec::new())
        } else {
            self.text(spans, ROLE_P)
        };
        flow.out.push(node);
    }

    fn push_block(&mut self, flow: &mut Flow, node: Value) {
        self.flush(flow);
        flow.out.push(node);
    }

    fn skip(&self, n: &Node) -> bool {
        n.kind == NodeKind::Comment
            || n.flags.no_print
            || matches!(n.macro_name.as_deref(), Some("TH" | "Dd" | "Dt" | "Os" | "Tg"))
    }

    fn feed_one(&mut self, n: &Node, flow: &mut Flow) {
        if self.skip(n) {
            return;
        }
        if matches!(n.kind, NodeKind::Head | NodeKind::Body | NodeKind::Tail) {
            self.feed(&n.children, flow);
            return;
        }
        if let Some(hp) = flow.hp.take() {
            let body_only_ip = n.kind == NodeKind::Block
                && n.macro_name.as_deref() == Some("IP")
                && head_is_empty(part(n, NodeKind::Head));
            self.flush(flow);
            if body_only_ip {
                self.dl_item(part(&hp, NodeKind::Body), part(n, NodeKind::Body), None, flow);
                return;
            }
            self.feed(part(&hp, NodeKind::Body), flow);
            self.flush(flow);
        }
        if n.kind == NodeKind::Table {
            self.flush(flow);
            self.table_row(n, flow);
            return;
        }
        let mac = n.macro_name.as_deref().unwrap_or("");
        let handled = match self.set {
            MacroSet::Mdoc => self.mdoc_block(mac, n, flow),
            _ => self.man_block(mac, n, flow),
        };
        if handled {
            return;
        }
        if n.flags.no_fill != flow.para.nofill {
            if !flow.para.is_blank() {
                self.flush(flow);
            }
            flow.para.nofill = n.flags.no_fill;
        }
        self.inline(n, &mut flow.para);
    }

    /// man(7) block macros; false when `n` is inline content.
    fn man_block(&mut self, mac: &str, n: &Node, flow: &mut Flow) -> bool {
        match (n.kind, mac) {
            (NodeKind::Block, "SH") => {
                let s = self.section(n, false);
                self.push_block(flow, s);
            }
            (NodeKind::Block, "SS") => {
                let s = self.subsection(n);
                self.push_block(flow, s);
            }
            (NodeKind::Block, "HP") => {
                self.flush(flow);
                flow.hp = Some(n.clone());
            }
            (NodeKind::Block, "PP" | "LP" | "P") => {
                self.flush(flow);
                self.feed(part(n, NodeKind::Body), flow);
                self.flush(flow);
            }
            (NodeKind::Block, "TP" | "TQ" | "IP") => {
                let head = part(n, NodeKind::Head);
                if head_is_empty(head) {
                    let body = self.blocks(part(n, NodeKind::Body));
                    let div = self.el("div", "mt-indent", body);
                    self.push_block(flow, div);
                } else {
                    self.flush(flow);
                    self.dl_item(head, part(n, NodeKind::Body), head_tag(n), flow);
                }
            }
            (NodeKind::Block, "RS") => {
                let body = self.blocks(part(n, NodeKind::Body));
                let div = self.el("div", "mt-indent", body);
                self.push_block(flow, div);
            }
            (NodeKind::Block, "SY") => {
                self.flush(flow);
                let saved = self.fonts;
                self.fonts = FontState::new(Font::BOLD);
                for c in part(n, NodeKind::Head) {
                    self.inline(c, &mut flow.para);
                }
                self.fonts = saved;
                for c in part(n, NodeKind::Body) {
                    self.feed_one(c, flow);
                }
                self.flush(flow);
            }
            (_, "sp") => {
                if flow.para.nofill {
                    flow.para.newline();
                    flow.para.newline();
                } else {
                    self.flush(flow);
                }
            }
            (_, "br" | "ti") => flow.para.newline(),
            _ => return false,
        }
        true
    }

    /// mdoc(7) block macros; false when `n` is inline content.
    fn mdoc_block(&mut self, mac: &str, n: &Node, flow: &mut Flow) -> bool {
        match (n.kind, mac) {
            (NodeKind::Block, "Sh") => {
                let s = self.section(n, false);
                self.push_block(flow, s);
            }
            (NodeKind::Block, "Ss") => {
                let s = self.subsection(n);
                self.push_block(flow, s);
            }
            (_, "Pp" | "Lp") => self.flush(flow),
            (_, "sp") => {
                if flow.para.nofill {
                    flow.para.newline();
                    flow.para.newline();
                } else {
                    self.flush(flow);
                }
            }
            (_, "br") => flow.para.newline(),
            (NodeKind::Block, "Bl") => {
                let list = self.mdoc_list(n);
                self.push_block(flow, list);
            }
            (NodeKind::Block, "Bd") => {
                let body = self.blocks(part(n, NodeKind::Body));
                let indent = n.offset.as_deref().is_some_and(|o| o != "left");
                let node = if indent { self.el("div", "mt-indent", body) } else { self.el("div", "mt-display", body) };
                self.push_block(flow, node);
            }
            (NodeKind::Block, "D1" | "Dl") => {
                let mut inner = Flow::new();
                inner.para.nofill = mac == "Dl";
                for c in part(n, NodeKind::Body) {
                    self.inline(c, &mut inner.para);
                }
                self.flush(&mut inner);
                let div = self.el("div", "mt-indent", inner.out);
                self.push_block(flow, div);
            }
            (NodeKind::Block, "Rs") => {
                self.flush(flow);
                self.reference(n, &mut flow.para);
                self.flush(flow);
            }
            // SYNOPSIS command and function blocks each take their own line.
            (NodeKind::Block, "Nm" | "Fo") => self.synopsis_line(mac, n, flow),
            (NodeKind::Element, "Fn" | "Fd" | "In" | "Ft") if n.flags.synopsis_pretty => {
                self.synopsis_line(mac, n, flow);
            }
            _ => return false,
        }
        true
    }

    fn synopsis_line(&mut self, mac: &str, n: &Node, flow: &mut Flow) {
        // A function after its `.Ft` type line stays in the same block.
        if !matches!(mac, "Fn" | "Fo") || !flow.para.plain().ends_with('\n') {
            self.flush(flow);
        }
        self.inline(n, &mut flow.para);
        if mac == "Ft" {
            flow.para.newline();
        } else {
            self.flush(flow);
        }
    }

    fn dl_item(&mut self, head: &[Node], body: &[Node], tag: Option<&str>, flow: &mut Flow) {
        let mut inl = Inline::new(false);
        for c in head {
            self.inline(c, &mut inl);
        }
        let short = inl.plain().trim().chars().count() <= 6;
        let dt_text = {
            let spans = accent_flags(inl.take());
            self.text(spans, ROLE_DT)
        };
        let mut dt = self.el("dt", "", vec![dt_text]);
        if let Some(tag) = tag {
            dt["p"]["attrs"] = json!({ "data-tag": tag });
        }
        let dd_children = self.blocks(body);

        // A tag with no body (`.TP` then `.TQ`) shares the next one's body.
        if let Some(last) = flow.out.last_mut()
            && is_dl(last)
            && let Some(item) = last["c"].as_array_mut().and_then(|items| items.last_mut())
            && let Some(parts) = item["c"].as_array_mut()
            && parts.last().is_some_and(|dd| dd["c"].as_array().is_none_or(Vec::is_empty))
        {
            let dd = parts.pop().expect("checked above");
            parts.push(dt);
            let mut dd = dd;
            if !dd_children.is_empty() {
                dd["c"] = Value::Array(dd_children);
            }
            parts.push(dd);
            item["p"]["class"] = json!("mt-item");
            return;
        }

        let dd = self.el("dd", "", dd_children);
        let item = self.el("div", if short { "mt-item mt-short" } else { "mt-item" }, vec![dt, dd]);
        match flow.out.last_mut() {
            Some(last) if is_dl(last) => last["c"].as_array_mut().expect("dl has items").push(item),
            _ => {
                let dl = self.el("dl", "mt-dl", vec![item]);
                flow.out.push(dl);
            }
        }
    }

    fn mdoc_list(&mut self, n: &Node) -> Value {
        let items: Vec<&Node> = part(n, NodeKind::Body)
            .iter()
            .filter(|c| c.macro_name.as_deref() == Some("It"))
            .collect();
        match n.list_kind {
            Some(NormalizedListKind::Definition) => {
                let mut flow = Flow::new();
                for it in items {
                    let tag = head_tag(it);
                    self.dl_item(part(it, NodeKind::Head), part(it, NodeKind::Body), tag, &mut flow);
                }
                self.flush(&mut flow);
                match flow.out.len() {
                    1 => flow.out.pop().expect("one node"),
                    _ => self.el("div", "", flow.out),
                }
            }
            Some(NormalizedListKind::Column) => {
                let mut rows = Vec::new();
                for it in items {
                    let cells = self.column_cells(it);
                    rows.push(self.el("tr", "", cells));
                }
                self.el("table", "mt-table", rows)
            }
            kind => {
                let (tag, class) = match kind {
                    Some(NormalizedListKind::Bullet) => ("ul", "mt-list"),
                    Some(NormalizedListKind::Ordered) => ("ol", "mt-list"),
                    _ => ("div", "mt-plain"),
                };
                let mut lis = Vec::new();
                for it in items {
                    let mut flow = Flow::new();
                    self.feed(part(it, NodeKind::Head), &mut flow);
                    self.feed(part(it, NodeKind::Body), &mut flow);
                    self.flush(&mut flow);
                    lis.push(self.el(if tag == "div" { "div" } else { "li" }, "", flow.out));
                }
                self.el(tag, class, lis)
            }
        }
    }

    /// The cells of one `.It` of a `-column` list: `Ta` macros and body
    /// parts separate them.
    fn column_cells(&mut self, it: &Node) -> Vec<Value> {
        let mut groups: Vec<Vec<&Node>> = vec![Vec::new()];
        for p in &it.children {
            if p.kind == NodeKind::Body && !groups.last().expect("never empty").is_empty() {
                groups.push(Vec::new());
            }
            for c in &p.children {
                if c.macro_name.as_deref() == Some("Ta") {
                    groups.push(Vec::new());
                } else {
                    groups.last_mut().expect("never empty").push(c);
                }
            }
        }
        groups
            .into_iter()
            .map(|cell| {
                let mut inl = Inline::new(false);
                for c in cell {
                    self.inline(c, &mut inl);
                }
                let spans = inl.take();
                let text = self.text(spans, ROLE_CELL);
                self.el("td", "", vec![text])
            })
            .collect()
    }

    fn table_row(&mut self, n: &Node, flow: &mut Flow) {
        if n.table_cells.is_empty()
            || n.table_cells.iter().all(|c| c.kind != TableCellKind::Text && c.kind != TableCellKind::Empty)
        {
            return;
        }
        let mut cells = Vec::new();
        for cell in &n.table_cells {
            if cell.vertical_continuation {
                continue;
            }
            let mut inl = Inline::new(false);
            if cell.kind == TableCellKind::Text
                && let Some(text) = &cell.text
            {
                let mut fonts = FontState::default();
                for run in escape::decode(text, &mut fonts) {
                    inl.raw(&run.text, run.font, None);
                }
            }
            let spans = inl.take();
            let text = self.text(spans, ROLE_CELL);
            let mut td = self.el("td", "", vec![text]);
            let mut attrs = serde_json::Map::new();
            if cell.column_span > 1 {
                attrs.insert("colspan".into(), json!(cell.column_span));
            }
            if cell.row_span > 1 {
                attrs.insert("rowspan".into(), json!(cell.row_span));
            }
            if !attrs.is_empty() {
                td["p"]["attrs"] = Value::Object(attrs);
            }
            cells.push(td);
        }
        let tr = self.el("tr", "", cells);
        match flow.out.last_mut() {
            Some(last) if !n.flags.table_start && last["p"]["class"] == "mt-table" => {
                last["c"].as_array_mut().expect("table has rows").push(tr);
            }
            _ => {
                let table = self.el("table", "mt-table", vec![tr]);
                flow.out.push(table);
            }
        }
    }

    // ------------------------------------------------------------ inline

    fn inline(&mut self, n: &Node, inl: &mut Inline) {
        if self.skip(n) {
            return;
        }
        if n.flags.line_start {
            inl.line_start();
        }
        match n.kind {
            NodeKind::Head | NodeKind::Body | NodeKind::Tail => self.children_inline(n, inl),
            NodeKind::Text => {
                let text = n.text.as_deref().unwrap_or("");
                self.words(text, inl, n.flags.delimiter_close);
                if n.flags.delimiter_open || n.flags.line_continuation {
                    inl.glue_next();
                }
            }
            NodeKind::Equation => {
                if let Some(eqn) = &n.equation {
                    inl.word(eqn, Font::ITALIC, None, false);
                }
            }
            NodeKind::Table => {}
            _ => match self.set {
                MacroSet::Mdoc => self.mdoc_inline(n, inl),
                _ => self.man_inline(n, inl),
            },
        }
    }

    /// Decoded text in the current font, as one word.
    fn words(&mut self, text: &str, inl: &mut Inline, close: bool) {
        let runs = escape::decode(text, &mut self.fonts);
        let mut first = true;
        for run in runs {
            if first {
                inl.word(&run.text, run.font, None, close);
                first = false;
            } else {
                inl.raw(&run.text, run.font, None);
            }
        }
    }

    fn with_font(&mut self, font: Font, f: impl FnOnce(&mut Self)) {
        let saved = self.fonts;
        self.fonts = FontState::new(font);
        f(self);
        self.fonts = saved;
    }

    fn children_inline(&mut self, n: &Node, inl: &mut Inline) {
        for c in &n.children {
            self.inline(c, inl);
        }
    }

    fn man_inline(&mut self, n: &Node, inl: &mut Inline) {
        let mac = n.macro_name.as_deref().unwrap_or("");
        let alternate = |a: Font, b: Font| [a, b];
        let pair = match mac {
            "BR" => Some(alternate(Font::BOLD, Font::ROMAN)),
            "RB" => Some(alternate(Font::ROMAN, Font::BOLD)),
            "BI" => Some(alternate(Font::BOLD, Font::ITALIC)),
            "IB" => Some(alternate(Font::ITALIC, Font::BOLD)),
            "IR" => Some(alternate(Font::ITALIC, Font::ROMAN)),
            "RI" => Some(alternate(Font::ROMAN, Font::ITALIC)),
            _ => None,
        };
        if let Some(fonts) = pair {
            for (i, c) in n.children.iter().enumerate() {
                self.with_font(fonts[i % 2], |r| {
                    if i > 0 {
                        inl.glue_next();
                    }
                    r.inline(c, inl);
                });
            }
            return;
        }
        match mac {
            "B" | "SB" => self.with_font(Font::BOLD, |r| r.children_inline(n, inl)),
            "I" => self.with_font(Font::ITALIC, |r| r.children_inline(n, inl)),
            "SM" => self.children_inline(n, inl),
            "ft" => {
                let name = n.children.first().and_then(|c| c.text.as_deref()).unwrap_or("P");
                self.fonts.select(name);
            }
            "MR" => {
                // groff 1.23 man page reference: .MR name section [trailer]
                let args: Vec<&str> = n.children.iter().filter_map(|c| c.text.as_deref()).collect();
                if let [name, section, rest @ ..] = args.as_slice() {
                    let label = format!("{name}({section})");
                    let href = format!("man:{label}");
                    inl.sep(false);
                    inl.raw(name, Font::ITALIC, Some(&href));
                    inl.raw(&format!("({section})"), Font::ROMAN, Some(&href));
                    for r in rest {
                        inl.raw(r, Font::ROMAN, None);
                    }
                } else {
                    self.children_inline(n, inl);
                }
            }
            "UR" | "MT" => {
                let target: String = part(n, NodeKind::Head)
                    .iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect();
                let href = if mac == "MT" { format!("mailto:{target}") } else { target.clone() };
                let mut label = Inline::new(false);
                for c in part(n, NodeKind::Body) {
                    self.inline(c, &mut label);
                }
                let text = label.plain();
                let text = text.trim();
                inl.word(if text.is_empty() { &target } else { text }, Font::ROMAN, Some(&href), false);
            }
            "OP" => {
                let args: Vec<&str> = n.children.iter().filter_map(|c| c.text.as_deref()).collect();
                inl.word("[", Font::ROMAN, None, false);
                inl.glue_next();
                if let Some(flag) = args.first() {
                    self.with_font(Font::BOLD, |r| r.words(flag, inl, false));
                }
                if let Some(arg) = args.get(1) {
                    self.with_font(Font::ITALIC, |r| r.words(arg, inl, false));
                }
                inl.word("]", Font::ROMAN, None, true);
            }
            "nf" | "fi" | "EX" | "EE" | "PD" | "ad" | "na" | "hy" | "nh" | "ta" | "ce" | "ll" | "in"
            | "ne" | "UC" | "AT" | "DT" | "YS" | "UE" | "ME" | "po" | "mc" | "rj" => {}
            _ => {
                for c in &n.children {
                    self.inline(c, inl);
                }
            }
        }
    }

    fn mdoc_inline(&mut self, n: &Node, inl: &mut Inline) {
        let mac = n.macro_name.as_deref().unwrap_or("");
        if let Some((open, close)) = enclosure(mac, n) {
            inl.word(&open, Font::ROMAN, None, false);
            inl.glue_next();
            for c in n.children.iter().filter(|c| c.kind != NodeKind::Tail) {
                self.inline(c, inl);
            }
            if !close.is_empty() {
                inl.word(&close, Font::ROMAN, None, true);
            }
            for tail in n.children.iter().filter(|c| c.kind == NodeKind::Tail) {
                self.children_inline(tail, inl);
            }
            return;
        }
        match mac {
            "Fl" => {
                if n.children.is_empty() {
                    inl.word("-", Font::BOLD, None, false);
                }
                for c in &n.children {
                    let text = c.text.as_deref().unwrap_or("");
                    if c.flags.delimiter_close || c.flags.delimiter_open {
                        self.inline(c, inl);
                    } else {
                        let saved = self.fonts;
                        self.fonts = FontState::new(Font::BOLD);
                        self.words(&format!("-{text}"), inl, false);
                        self.fonts = saved;
                    }
                }
            }
            "Ar" if n.children.is_empty() => inl.word("file ...", Font::ITALIC, None, false),
            "Nm" if n.kind == NodeKind::Block => {
                self.with_font(Font::BOLD, |r| {
                    for c in part(n, NodeKind::Head) {
                        r.inline(c, inl);
                    }
                });
                for c in part(n, NodeKind::Body) {
                    self.inline(c, inl);
                }
            }
            "Nm" if n.children.is_empty() => {
                let name = self.page_name.clone();
                inl.word(&name, Font::BOLD, None, false);
            }
            "Xr" => {
                let args: Vec<&Node> = n.children.iter().filter(|c| c.kind == NodeKind::Text).collect();
                let name = args.first().and_then(|c| c.text.as_deref()).unwrap_or("");
                match args.get(1).and_then(|c| c.text.as_deref()) {
                    Some(section) if !args[1].flags.delimiter_close => {
                        let href = format!("man:{name}({section})");
                        inl.word(&format!("{name}({section})"), Font::ROMAN, Some(&href), false);
                        for c in &args[2..] {
                            self.inline(c, inl);
                        }
                    }
                    _ => {
                        inl.word(name, Font::ROMAN, None, false);
                        for c in args.iter().skip(1) {
                            self.inline(c, inl);
                        }
                    }
                }
            }
            "Lk" | "Mt" => {
                let args: Vec<&str> = n.children.iter().filter_map(|c| c.text.as_deref()).collect();
                if let Some(target) = args.first() {
                    let href = if mac == "Mt" { format!("mailto:{target}") } else { (*target).to_owned() };
                    let label = if args.len() > 1 { args[1..].join(" ") } else { (*target).to_owned() };
                    inl.word(&label, Font::ROMAN, Some(&href), false);
                }
            }
            "Fn" | "Fo" => {
                let (name, args): (Option<&Node>, Vec<&Node>) = if mac == "Fo" {
                    (
                        part(n, NodeKind::Head).iter().find(|c| c.kind == NodeKind::Text),
                        part(n, NodeKind::Body).iter().filter(|c| c.macro_name.as_deref() == Some("Fa")).collect(),
                    )
                } else {
                    (n.children.first(), n.children.iter().skip(1).filter(|c| c.kind == NodeKind::Text).collect())
                };
                if let Some(name) = name.and_then(|c| c.text.as_deref()) {
                    self.with_font(Font::BOLD, |r| r.words(name, inl, false));
                }
                inl.raw("(", Font::ROMAN, None);
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        inl.raw(", ", Font::ROMAN, None);
                    }
                    let text: String = if a.kind == NodeKind::Text {
                        a.text.clone().unwrap_or_default()
                    } else {
                        a.children.iter().filter_map(|c| c.text.as_deref()).collect::<Vec<_>>().join(" ")
                    };
                    let mut fonts = FontState::new(Font::ITALIC);
                    for run in escape::decode(&text, &mut fonts) {
                        inl.raw(&run.text, run.font, None);
                    }
                }
                inl.raw(")", Font::ROMAN, None);
                if n.flags.synopsis_pretty {
                    inl.raw(";", Font::ROMAN, None);
                }
            }
            "In" => {
                let header = n.children.first().and_then(|c| c.text.as_deref()).unwrap_or("");
                if n.flags.synopsis_pretty {
                    inl.word("#include", Font::BOLD, None, false);
                }
                inl.word(&format!("<{header}>"), Font::BOLD, None, false);
            }
            "Nd" => {
                inl.word("—", Font::ROMAN, None, false);
                self.children_inline(n, inl);
            }
            "Ns" => inl.glue_next(),
            "Ap" => {
                inl.raw("'", Font::ROMAN, None);
                inl.glue_next();
            }
            "Pf" => {
                if let Some(prefix) = n.children.first() {
                    self.inline(prefix, inl);
                }
                inl.glue_next();
                for c in n.children.iter().skip(1) {
                    self.inline(c, inl);
                }
            }
            "Sm" => {
                let arg = n.children.first().and_then(|c| c.text.as_deref());
                inl.set_spacing(match arg {
                    Some("off") => Some(false),
                    Some("on") => Some(true),
                    _ => None,
                });
            }
            "Ex" | "Rv" => {
                let names: Vec<String> = n
                    .children
                    .iter()
                    .filter_map(|c| c.text.clone())
                    .filter(|t| t != "-std")
                    .collect();
                let names = if names.is_empty() { vec![self.page_name.clone()] } else { names };
                let sentence = if mac == "Ex" {
                    format!(
                        "The {} utility exits 0 on success, and >0 if an error occurs.",
                        names.join(", ")
                    )
                } else {
                    format!(
                        "The {}() function returns the value 0 if successful; otherwise the value -1 is returned and the global variable errno is set to indicate the error.",
                        names.join("(), ")
                    )
                };
                inl.word(&sentence, Font::ROMAN, None, false);
            }
            // libmandoc already spells out the system name in the children.
            "Bx" | "Ox" | "Nx" | "Fx" | "Dx" | "Bsx" | "Ux" | "At" if !n.children.is_empty() => {
                self.children_inline(n, inl);
            }
            "Bx" | "Ox" | "Nx" | "Fx" | "Dx" | "Bsx" | "Ux" | "At" => {
                let name = match mac {
                    "Bx" => "BSD",
                    "Ox" => "OpenBSD",
                    "Nx" => "NetBSD",
                    "Fx" => "FreeBSD",
                    "Dx" => "DragonFly",
                    "Bsx" => "BSD/OS",
                    "Ux" => "UNIX",
                    _ => "AT&T UNIX",
                };
                inl.word(name, Font::ROMAN, None, false);
            }
            "Bf" => {
                let font = match n.font {
                    Some(NormalizedFont::Emphasis) => Font::ITALIC,
                    Some(NormalizedFont::Symbolic) => Font::BOLD,
                    _ => Font::ROMAN,
                };
                self.with_font(font, |r| {
                    for c in part(n, NodeKind::Body) {
                        r.inline(c, inl);
                    }
                });
            }
            _ => {
                let font = mdoc_font(mac);
                match font {
                    Some(font) => self.with_font(font, |r| r.children_inline(n, inl)),
                    None => self.children_inline(n, inl),
                }
            }
        }
    }

    /// An `.Rs` reference: its parts in order, comma separated.
    fn reference(&mut self, n: &Node, inl: &mut Inline) {
        let parts: Vec<&Node> = part(n, NodeKind::Body).iter().filter(|c| !self.skip(c)).collect();
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                inl.raw(",", Font::ROMAN, None);
            }
            let font = match p.macro_name.as_deref() {
                Some("%B" | "%J") => Font::ITALIC,
                _ => Font::ROMAN,
            };
            if p.macro_name.as_deref() == Some("%U") {
                let url: String = p.children.iter().filter_map(|c| c.text.as_deref()).collect();
                inl.word(&url, Font::ROMAN, Some(&url), false);
            } else if p.macro_name.as_deref() == Some("%T") {
                inl.word("“", Font::ROMAN, None, false);
                inl.glue_next();
                self.children_inline(p, inl);
                inl.word("”", Font::ROMAN, None, true);
            } else {
                self.with_font(font, |r| r.children_inline(p, inl));
            }
        }
        if !parts.is_empty() {
            inl.raw(".", Font::ROMAN, None);
        }
    }
}

const ROLE_P: &str = "mantern.p";
const ROLE_PRE: &str = "mantern.pre";
const ROLE_DT: &str = "mantern.dt";
const ROLE_CELL: &str = "mantern.cell";

fn mdoc_font(mac: &str) -> Option<Font> {
    Some(match mac {
        "Fl" | "Cm" | "Ic" | "Nm" | "Sy" | "Fd" | "Ms" | "Cd" => Font::BOLD,
        "Ar" | "Pa" | "Va" | "Vt" | "Em" | "Fa" | "Ft" | "Ad" | "%B" | "%J" => Font::ITALIC,
        _ => return None,
    })
}

/// Opening and closing delimiters of mdoc's enclosure macros.
fn enclosure(mac: &str, n: &Node) -> Option<(String, String)> {
    let (open, close) = match mac {
        "Op" | "Oo" | "Bq" | "Bo" => ("[", "]"),
        "Dq" | "Do" => ("“", "”"),
        "Sq" | "So" | "Ql" => ("‘", "’"),
        "Pq" | "Po" => ("(", ")"),
        "Brq" | "Bro" => ("{", "}"),
        "Aq" | "Ao" => ("⟨", "⟩"),
        "Qq" | "Qo" => ("\"", "\""),
        "Eo" => ("", ""),
        "En" => {
            let e = n.enclosure.as_ref()?;
            return Some((e.opening.clone(), e.closing.clone().unwrap_or_default()));
        }
        _ => return None,
    };
    Some((open.to_owned(), close.to_owned()))
}

fn part(n: &Node, kind: NodeKind) -> &[Node] {
    n.children.iter().find(|c| c.kind == kind).map_or(&[], |c| c.children.as_slice())
}

fn head_tag(n: &Node) -> Option<&str> {
    n.children.iter().find(|c| c.kind == NodeKind::Head).and_then(|h| h.tag.as_deref())
}

fn head_is_empty(head: &[Node]) -> bool {
    fn visible(n: &Node) -> bool {
        n.text.as_deref().is_some_and(|t| !t.trim().is_empty()) || n.children.iter().any(visible)
    }
    !head.iter().any(visible)
}

fn head_plain(spans: &[Value]) -> String {
    spans
        .iter()
        .map(|s| s.as_str().or_else(|| s["t"].as_str()).unwrap_or(""))
        .collect()
}

fn is_dl(node: &Value) -> bool {
    node["k"] == "el" && node["p"]["tag"] == "dl"
}

fn radix36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("ascii digits")
}

/// Bold runs in a definition term are the flags and names it defines.
fn accent_flags(spans: Vec<Value>) -> Vec<Value> {
    spans
        .into_iter()
        .map(|mut span| {
            if let Some(style) = span["s"].as_str()
                && style.split(' ').any(|t| t == "strong")
            {
                span["s"] = Value::String(format!("{style} accent"));
            }
            span
        })
        .collect()
}

fn spans_plain(spans: &[Value]) -> String {
    spans.iter().map(|s| s.as_str().or_else(|| s["t"].as_str()).unwrap_or("")).collect()
}

/// A SEE ALSO block: kept as it is, or a run of references shown as chips.
enum SeeAlso {
    Keep(Value),
    /// `(label, href)`, once per target, in order.
    Chips(Vec<(String, String)>),
}

/// Paragraphs that are only a list of references become chips; every other
/// block (prose, bibliography entries, paragraphs mixing text and
/// references) stays, its links working in place. Neighbouring reference
/// paragraphs share one chip row.
fn see_also_items(body: Vec<Value>) -> Vec<SeeAlso> {
    let mut items: Vec<SeeAlso> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for node in body {
        let Some(refs) = reference_run(&node) else {
            seen.clear();
            items.push(SeeAlso::Keep(node));
            continue;
        };
        if !matches!(items.last(), Some(SeeAlso::Chips(_))) {
            seen.clear();
            items.push(SeeAlso::Chips(Vec::new()));
        }
        let Some(SeeAlso::Chips(chips)) = items.last_mut() else { unreachable!("a chip group was just pushed") };
        for (label, href) in refs {
            if seen.insert(href.clone()) {
                chips.push((label, href));
            }
        }
    }
    items
}

/// The `(label, href)` of each reference in a paragraph made only of
/// `name(section)` references and URLs, separated by commas, periods and
/// whitespace; `None` for anything else.
fn reference_run(node: &Value) -> Option<Vec<(String, String)>> {
    if node["k"] != "text" {
        return None;
    }
    let plain = spans_plain(node["p"]["spans"].as_array()?);
    let links = find_links(&plain);
    let mut at = 0;
    for &(a, b, _) in &links {
        let between = plain.get(at..a)?;
        if !between.chars().all(|c| c.is_whitespace() || matches!(c, ',' | '.')) {
            return None;
        }
        at = b;
    }
    let tail = plain.get(at..)?;
    if links.is_empty() || !tail.chars().all(|c| c.is_whitespace() || matches!(c, ',' | '.')) {
        return None;
    }
    Some(
        links
            .into_iter()
            .map(|(a, b, href)| {
                // A page chip reads `name(1)`; a URL chip shows the URL.
                let label = href.strip_prefix("man:").unwrap_or(&plain[a..b]).to_owned();
                (label, href)
            })
            .collect(),
    )
}

impl Renderer {
    /// An `.HP` nothing paired with is an ordinary paragraph.
    fn release_hp(&mut self, flow: &mut Flow) {
        if let Some(hp) = flow.hp.take() {
            self.flush(flow);
            self.feed(part(&hp, NodeKind::Body), flow);
            self.flush(flow);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{SeeAlso, see_also_items};

    fn para(text: &str) -> Value {
        json!({ "id": "n", "k": "text", "p": { "spans": [text], "role": "p" } })
    }

    #[test]
    fn standalone_reference_paragraphs_become_one_chip_row() {
        let items = see_also_items(vec![para("ls(1), git-commit(1)."), para("ls(1), openssl-req(1ssl)")]);
        let [SeeAlso::Chips(chips)] = items.as_slice() else { panic!("one chip group expected") };
        let labels: Vec<_> = chips.iter().map(|(l, h)| (l.as_str(), h.as_str())).collect();
        assert_eq!(
            labels,
            [
                ("ls(1)", "man:ls(1)"),
                ("git-commit(1)", "man:git-commit(1)"),
                ("openssl-req(1ssl)", "man:openssl-req(1ssl)"),
            ]
        );
    }

    #[test]
    fn prose_and_mixed_paragraphs_survive() {
        let prose = para("RFC 2616, Hypertext Transfer Protocol.");
        let mixed = para("See also curl(1) for the details.");
        let unlinked = para("Knuth, The TeXbook.");
        let items = see_also_items(vec![prose.clone(), para("ls(1)"), mixed.clone(), unlinked.clone()]);
        assert!(matches!(&items[0], SeeAlso::Keep(n) if *n == prose));
        assert!(matches!(&items[1], SeeAlso::Chips(c) if c.len() == 1));
        assert!(matches!(&items[2], SeeAlso::Keep(n) if *n == mixed));
        assert!(matches!(&items[3], SeeAlso::Keep(n) if *n == unlinked));
    }

    #[test]
    fn url_chip_keeps_its_address() {
        let items = see_also_items(vec![para("https://example.com/doc, ls(1).")]);
        let [SeeAlso::Chips(chips)] = items.as_slice() else { panic!("one chip group expected") };
        assert_eq!(chips[0], ("https://example.com/doc".to_owned(), "https://example.com/doc".to_owned()));
        assert_eq!(chips[1].1, "man:ls(1)");
    }

    #[test]
    fn huge_reference_list_completes() {
        let text: String = (0..20_000).map(|i| format!("page{i}(1), ")).collect();
        let items = see_also_items(vec![para(&text)]);
        let [SeeAlso::Chips(chips)] = items.as_slice() else { panic!("one chip group expected") };
        assert_eq!(chips.len(), 20_000);
    }
}
