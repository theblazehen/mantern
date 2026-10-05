//! Dev-only spike: measure how well libmandoc-rs handles a real man page corpus.
//!
//! `cargo run --release --example corpus -- [--json PATH] [ROOT...]` parses every
//! page under the given man roots (default: `manpath`) and prints coverage stats.
//! `cargo run --example corpus -- --dump NAME|PATH` prints one page's syntax tree.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use libmandoc_rs::{
    Compression, DiagnosticLevel, IncludePolicy, MacroSet, Node, NodeKind, ParseOptions,
    ParseReport, Parser, RenderFormat, Renderer,
};
use rayon::prelude::*;
use serde::Serialize;

type Error = Box<dyn std::error::Error + Send + Sync>;

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, target] if flag == "--dump" => dump(target),
        _ => corpus(&args),
    }
}

fn parser() -> Parser {
    Parser::new(ParseOptions {
        includes: IncludePolicy::SourceTree,
        compression: Compression::Auto,
    })
}

/// Sources libmandoc cannot decompress itself, decoded in Rust.
/// `None` means libmandoc reads the file directly (plain, gzip, zstd).
fn decoded_source(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut out = Vec::new();
    match ext {
        "xz" | "lzma" => {
            xz2::read::XzDecoder::new(fs::File::open(path)?).read_to_end(&mut out)?;
        }
        "bz2" => {
            bzip2::read::BzDecoder::new(fs::File::open(path)?).read_to_end(&mut out)?;
        }
        _ => return Ok(None),
    }
    Ok(Some(out))
}

fn parse(path: &Path) -> Result<ParseReport, Error> {
    let p = parser();
    Ok(match decoded_source(path)? {
        Some(bytes) => p.parse_bytes(path, &bytes)?,
        None => p.parse_file(path)?,
    })
}

fn render_lines(path: &Path, width: usize) -> Result<usize, Error> {
    let r = Renderer::new(RenderFormat::Utf8)
        .with_parser(parser())
        .with_width(width);
    let report = match decoded_source(path)? {
        Some(bytes) => r.render_bytes(path, &bytes)?,
        None => r.render_file(path)?,
    };
    Ok(report.output.lines().count())
}

// ---------------------------------------------------------------- discovery

fn man_roots(explicit: &[String]) -> Result<Vec<PathBuf>, Error> {
    if !explicit.is_empty() {
        return Ok(explicit.iter().map(PathBuf::from).collect());
    }
    let out = Command::new("manpath").output()?;
    let text = String::from_utf8(out.stdout)?;
    let mut roots: Vec<PathBuf> = text.trim().split(':').map(PathBuf::from).collect();
    roots.dedup();
    Ok(roots)
}

/// Pages directly under `ROOT/manSECTION/`, skipping localized trees and
/// preformatted `cat` directories.
fn pages(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let Ok(sections) = fs::read_dir(root) else { continue };
        for section in sections.flatten() {
            let name = section.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with("man") || !section.path().is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(section.path()) else { continue };
            for file in files.flatten() {
                let path = file.path();
                if !path.is_file() {
                    continue;
                }
                let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if seen.insert(canonical) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------- per page

#[derive(Default, Serialize)]
struct PageStats {
    path: String,
    error: Option<String>,
    macro_set: String,
    title: Option<String>,
    section: Option<String>,
    alias_target: Option<String>,
    parse_ms: f64,
    nodes: usize,
    text_nodes: usize,
    tables: usize,
    equations: usize,
    tagged: usize,
    rendered_lines: Option<usize>,
    diag_unsupported: usize,
    diag_error: usize,
    diag_warning: usize,
    diag_style: usize,
    /// Distinct messages at error/unsupported level (for aggregation).
    hard_messages: Vec<String>,
    macros: BTreeMap<String, usize>,
    /// man(7): `.TP`/`.IP`/`.TQ` heads whose text starts with a dash.
    dash_heads: usize,
    /// man(7): heads of any `.TP`/`.IP`/`.TQ`.
    tag_heads: usize,
    /// Visible `name(section)` cross-reference shapes in text.
    xref_shapes: usize,
    /// mdoc: explicit `.Xr` cross-references.
    xr_macros: usize,
    has_options_section: bool,
    /// The TSP document mantern draws: nodes, depth, JSON bytes.
    tsp_nodes: usize,
    tsp_depth: usize,
    tsp_bytes: usize,
    tsp_panic: bool,
}

fn analyze(path: &Path) -> PageStats {
    let mut s = PageStats {
        path: path.display().to_string(),
        ..PageStats::default()
    };
    let started = Instant::now();
    let report = match std::panic::catch_unwind(|| parse(path)) {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            s.error = Some(e.to_string());
            return s;
        }
        Err(_) => {
            s.error = Some("panic".into());
            return s;
        }
    };
    s.parse_ms = started.elapsed().as_secs_f64() * 1e3;

    let doc = &report.document;
    s.macro_set = match doc.macro_set {
        MacroSet::Man => "man",
        MacroSet::Mdoc => "mdoc",
        MacroSet::None => "none",
    }
    .into();
    s.title = doc.metadata.title.clone();
    s.section = doc.metadata.section.clone();
    s.alias_target = doc.metadata.alias_target.clone();

    for d in &report.diagnostics {
        match d.level {
            DiagnosticLevel::Unsupported => s.diag_unsupported += 1,
            DiagnosticLevel::Error => s.diag_error += 1,
            DiagnosticLevel::Warning => s.diag_warning += 1,
            DiagnosticLevel::Style => s.diag_style += 1,
        }
        if matches!(d.level, DiagnosticLevel::Unsupported | DiagnosticLevel::Error) {
            let msg = format!("{:?}: {}", d.level, normalize_message(&d.message));
            if !s.hard_messages.contains(&msg) {
                s.hard_messages.push(msg);
            }
        }
    }

    walk(&doc.root, doc.macro_set, &mut s);

    match std::panic::catch_unwind(|| {
        mantern::render::document(doc, &mantern::render::Options { fold: false })
    }) {
        Ok(main) => {
            let (nodes, depth) = tsp_size(&main, 1);
            s.tsp_nodes = nodes;
            s.tsp_depth = depth;
            s.tsp_bytes = main.to_string().len();
        }
        Err(_) => s.tsp_panic = true,
    }

    if s.alias_target.is_none() {
        s.rendered_lines = render_lines(path, 80).ok();
    }
    s
}

/// Keep the kind of finding, drop the page-specific argument after the colon.
fn normalize_message(message: &str) -> String {
    let head = message.split(':').next().unwrap_or(message).trim();
    // Keep the request/macro name for "unsupported roff request: .xx" style messages.
    let detail = message
        .split(':')
        .nth(1)
        .map(str::trim)
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|w| w.starts_with('.') || w.starts_with('\\'))
        .unwrap_or("");
    if detail.is_empty() {
        head.to_string()
    } else {
        format!("{head}: {detail}")
    }
}

fn walk(node: &Node, set: MacroSet, s: &mut PageStats) {
    s.nodes += 1;
    if node.tag.is_some() || node.flags.deep_link_target {
        s.tagged += 1;
    }
    match node.kind {
        NodeKind::Text => {
            s.text_nodes += 1;
            if let Some(t) = &node.text {
                s.xref_shapes += count_xref_shapes(&strip_escapes(t));
            }
        }
        NodeKind::Table if node.flags.table_start => s.tables += 1,
        NodeKind::Equation => s.equations += 1,
        _ => {}
    }
    if let Some(m) = &node.macro_name
        && matches!(node.kind, NodeKind::Block | NodeKind::Element)
    {
        *s.macros.entry(m.clone()).or_default() += 1;
        if m == "Xr" {
            s.xr_macros += 1;
        }
        if matches!(m.as_str(), "SH" | "Sh")
            && head_text(node).to_ascii_uppercase().contains("OPTION")
        {
            s.has_options_section = true;
        }
        if set == MacroSet::Man && matches!(m.as_str(), "TP" | "IP" | "TQ") {
            let head = head_text(node);
            if !head.trim().is_empty() {
                s.tag_heads += 1;
                if strip_escapes(&head).trim_start().starts_with(['-', '−', '‐']) {
                    s.dash_heads += 1;
                }
            }
        }
    }
    for child in &node.children {
        walk(child, set, s);
    }
}

/// Node count and depth of a TSP node tree.
fn tsp_size(node: &serde_json::Value, depth: usize) -> (usize, usize) {
    let mut nodes = 1;
    let mut deepest = depth;
    for child in node["c"].as_array().into_iter().flatten() {
        let (n, d) = tsp_size(child, depth + 1);
        nodes += n;
        deepest = deepest.max(d);
    }
    (nodes, deepest)
}

fn head_text(block: &Node) -> String {
    block
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Head)
        .map(collect_text)
        .unwrap_or_default()
}

fn collect_text(node: &Node) -> String {
    let mut out = String::new();
    fn go(n: &Node, out: &mut String) {
        if let Some(t) = &n.text {
            if !out.is_empty() && !n.flags.delimiter_close {
                out.push(' ');
            }
            out.push_str(t);
        }
        for c in &n.children {
            go(c, out);
        }
    }
    go(node, &mut out);
    out
}

/// Drop roff font/zero-width escapes and turn `\-`/`\(mi` into a dash,
/// enough to classify text without a full escape decoder.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('-') => out.push('-'),
            Some('e') => out.push('\\'),
            Some('f') => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                    }
                }
                Some('(') => {
                    chars.next();
                    chars.next();
                }
                _ => {}
            },
            Some('(') => {
                let a = chars.next().unwrap_or(' ');
                let b = chars.next().unwrap_or(' ');
                if (a, b) == ('m', 'i') || (a, b) == ('h', 'y') {
                    out.push('-');
                }
            }
            Some('&') | Some('c') | Some('%') => {}
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Count `word(1)`-style references: a name ending right before `(`, a
/// section digit, optional lowercase suffix, `)`.
fn count_xref_shapes(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    for (i, _) in text.match_indices('(') {
        let name_ok = i > 0 && {
            let b = bytes[i - 1];
            b.is_ascii_alphanumeric() || b == b'_' || b == b'+' || b == b'.'
        };
        let rest = &bytes[i + 1..];
        let digit_ok = rest.first().is_some_and(u8::is_ascii_digit);
        if name_ok && digit_ok {
            let close = rest[1..]
                .iter()
                .take(4)
                .position(|&b| b == b')')
                .map(|p| rest[1..1 + p].iter().all(u8::is_ascii_lowercase));
            if close == Some(true) {
                count += 1;
            }
        }
    }
    count
}

// ---------------------------------------------------------------- corpus

fn corpus(args: &[String]) -> Result<(), Error> {
    let mut json_out = None;
    let mut roots = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--json" {
            json_out = Some(PathBuf::from(it.next().ok_or("--json needs a path")?));
        } else {
            roots.push(a.clone());
        }
    }
    let roots = man_roots(&roots)?;
    let files = pages(&roots);
    eprintln!("roots: {roots:?}\npages: {}", files.len());

    let started = Instant::now();
    let stats: Vec<PageStats> = files.par_iter().map(|p| analyze(p)).collect();
    let wall = started.elapsed();

    report(&stats, wall);
    if let Some(path) = json_out {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, serde_json::to_vec(&stats)?)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "-".into()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / d as f64)
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn top<K: Ord + Clone + std::fmt::Display>(counts: &BTreeMap<K, usize>, n: usize) -> String {
    let mut v: Vec<_> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter()
        .take(n)
        .map(|(k, c)| format!("    {c:>6}  {k}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(stats: &[PageStats], wall: std::time::Duration) {
    let total = stats.len();
    let failed: Vec<_> = stats.iter().filter(|s| s.error.is_some()).collect();
    let ok: Vec<_> = stats.iter().filter(|s| s.error.is_none()).collect();
    let aliases = ok.iter().filter(|s| s.alias_target.is_some()).count();
    let real: Vec<_> = ok.iter().filter(|s| s.alias_target.is_none()).collect();

    println!("== corpus");
    println!("pages {total}, parsed {} ({}), failed {}", ok.len(), pct(ok.len(), total), failed.len());
    println!("alias (.so) pages {aliases}, real pages {}", real.len());
    println!("wall {:.2}s", wall.as_secs_f64());

    let mut sets: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &real {
        *sets.entry(s.macro_set.as_str()).or_default() += 1;
    }
    println!("\n== macro sets (real pages)\n{}", top(&sets, 10));

    let mut err_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for s in &failed {
        let e = s.error.as_deref().unwrap_or("");
        let key = e.rsplit(": ").next().unwrap_or(e).chars().take(90).collect::<String>();
        *err_kinds.entry(key).or_default() += 1;
    }
    if !failed.is_empty() {
        println!("\n== failures\n{}", top(&err_kinds, 15));
        for s in failed.iter().take(10) {
            println!("    e.g. {}: {}", s.path, s.error.as_deref().unwrap_or(""));
        }
    }

    let with = |f: &dyn Fn(&PageStats) -> bool| real.iter().filter(|s| f(s)).count();
    println!("\n== diagnostics (real pages)");
    println!("any unsupported  {:>6} ({})", with(&|s| s.diag_unsupported > 0), pct(with(&|s| s.diag_unsupported > 0), real.len()));
    println!("any error        {:>6} ({})", with(&|s| s.diag_error > 0), pct(with(&|s| s.diag_error > 0), real.len()));
    println!("clean (no unsup/error) {:>6} ({})", with(&|s| s.diag_unsupported + s.diag_error == 0), pct(with(&|s| s.diag_unsupported + s.diag_error == 0), real.len()));

    let mut hard: BTreeMap<String, usize> = BTreeMap::new();
    for s in &real {
        for m in &s.hard_messages {
            *hard.entry(m.clone()).or_default() += 1;
        }
    }
    println!("\n== top unsupported/error findings (pages affected)\n{}", top(&hard, 30));

    for set in ["man", "mdoc"] {
        let pages: Vec<_> = real.iter().filter(|s| s.macro_set == set).collect();
        if pages.is_empty() {
            continue;
        }
        let mut macros: BTreeMap<String, usize> = BTreeMap::new();
        for s in &pages {
            for (m, c) in &s.macros {
                *macros.entry(m.clone()).or_default() += c;
            }
        }
        let n = pages.len();
        let count = |f: &dyn Fn(&PageStats) -> bool| pages.iter().filter(|s| f(s)).count();
        println!("\n== {set}(7) pages: {n}");
        println!("with tables        {:>6} ({})", count(&|s| s.tables > 0), pct(count(&|s| s.tables > 0), n));
        println!("with equations     {:>6} ({})", count(&|s| s.equations > 0), pct(count(&|s| s.equations > 0), n));
        println!("with tags/anchors  {:>6} ({})", count(&|s| s.tagged > 0), pct(count(&|s| s.tagged > 0), n));
        println!("OPTIONS section    {:>6} ({})", count(&|s| s.has_options_section), pct(count(&|s| s.has_options_section), n));
        println!("xref shapes in text {:>5} pages, {} refs", count(&|s| s.xref_shapes > 0), pages.iter().map(|s| s.xref_shapes).sum::<usize>());
        if set == "man" {
            let heads: usize = pages.iter().map(|s| s.tag_heads).sum();
            let dash: usize = pages.iter().map(|s| s.dash_heads).sum();
            println!("TP/IP/TQ heads     {heads:>6}, dash-led {dash} ({})", pct(dash, heads));
            println!(
                "pages w/ dash heads {:>5}; of OPTIONS pages: {}",
                count(&|s| s.dash_heads > 0),
                pct(count(&|s| s.dash_heads > 0 && s.has_options_section), count(&|s| s.has_options_section))
            );
        } else {
            println!("Xr macros          {:>6}", pages.iter().map(|s| s.xr_macros).sum::<usize>());
        }
        println!("top macros\n{}", top(&macros, 25));
    }

    let mut ms: Vec<f64> = real.iter().map(|s| s.parse_ms).collect();
    ms.sort_by(f64::total_cmp);
    let slowest = real.iter().max_by(|a, b| a.parse_ms.total_cmp(&b.parse_ms));
    println!("\n== parse time (ms)");
    println!(
        "p50 {:.2}  p90 {:.2}  p99 {:.2}  max {:.2} ({})",
        percentile(&ms, 0.5),
        percentile(&ms, 0.9),
        percentile(&ms, 0.99),
        percentile(&ms, 1.0),
        slowest.map(|s| s.path.as_str()).unwrap_or("-")
    );

    let mut nodes: Vec<f64> = real.iter().map(|s| s.nodes as f64).collect();
    nodes.sort_by(f64::total_cmp);
    let biggest = real.iter().max_by_key(|s| s.nodes);
    println!("\n== AST size (nodes)");
    println!(
        "p50 {}  p90 {}  p99 {}  max {} ({})",
        percentile(&nodes, 0.5),
        percentile(&nodes, 0.9),
        percentile(&nodes, 0.99),
        percentile(&nodes, 1.0),
        biggest.map(|s| s.path.as_str()).unwrap_or("-")
    );

    let panics: Vec<_> = real.iter().filter(|s| s.tsp_panic).collect();
    let mut tsp_nodes: Vec<f64> = real.iter().map(|s| s.tsp_nodes as f64).collect();
    tsp_nodes.sort_by(f64::total_cmp);
    let deepest = real.iter().max_by_key(|s| s.tsp_depth);
    let heaviest = real.iter().max_by_key(|s| s.tsp_bytes);
    println!("\n== TSP document (mantern render)");
    println!("render panics {}", panics.len());
    for s in panics.iter().take(10) {
        println!("    e.g. {}", s.path);
    }
    println!(
        "nodes p50 {}  p99 {}  max {}  (limit 200000)",
        percentile(&tsp_nodes, 0.5),
        percentile(&tsp_nodes, 0.99),
        percentile(&tsp_nodes, 1.0)
    );
    println!(
        "max depth {} ({})  (limit 64; +3 for surface, region and col)",
        deepest.map_or(0, |s| s.tsp_depth),
        deepest.map_or("-", |s| s.path.as_str())
    );
    println!(
        "max frame JSON {} bytes ({})",
        heaviest.map_or(0, |s| s.tsp_bytes),
        heaviest.map_or("-", |s| s.path.as_str())
    );
    println!(
        "over limits: nodes {}  depth {}",
        real.iter().filter(|s| s.tsp_nodes >= 200_000).count(),
        real.iter().filter(|s| s.tsp_depth + 2 > 64).count()
    );

    let mut lines: Vec<f64> = real.iter().filter_map(|s| s.rendered_lines.map(|l| l as f64)).collect();
    lines.sort_by(f64::total_cmp);
    let render_failed = real.iter().filter(|s| s.rendered_lines.is_none()).count();
    println!("\n== rendered length at 80 cols (lines)");
    println!(
        "p50 {}  p90 {}  p99 {}  max {}  (render failures {render_failed})",
        percentile(&lines, 0.5),
        percentile(&lines, 0.9),
        percentile(&lines, 0.99),
        percentile(&lines, 1.0),
    );
    for rows in [40.0, 60.0] {
        let fit = lines.iter().filter(|&&l| l <= rows).count();
        println!("fit in {rows} rows: {} ({})", fit, pct(fit, lines.len()));
    }
}

// ---------------------------------------------------------------- dump

fn dump(target: &str) -> Result<(), Error> {
    let path = if Path::new(target).exists() {
        PathBuf::from(target)
    } else {
        let out = Command::new("man").args(["-w", target]).output()?;
        if !out.status.success() {
            return Err(format!("man -w {target} failed").into());
        }
        PathBuf::from(String::from_utf8(out.stdout)?.trim())
    };
    let report = parse(&path)?;
    let doc = &report.document;
    println!("# {}", path.display());
    println!("macro_set {:?}  metadata {:?}", doc.macro_set, doc.metadata);
    for d in &report.diagnostics {
        println!("diag {:?} {:?}: {}", d.level, d.location.map(|l| (l.line, l.column)), d.message);
    }
    print_node(&doc.root, 0);
    Ok(())
}

fn print_node(n: &Node, depth: usize) {
    let mut line = format!("{}{:?}", "  ".repeat(depth), n.kind);
    if let Some(m) = &n.macro_name {
        line += &format!(" .{m}");
    }
    if let Some(t) = &n.text {
        line += &format!(" {t:?}");
    }
    if let Some(t) = &n.tag {
        line += &format!(" tag={t:?}");
    }
    if let Some(k) = n.list_kind {
        line += &format!(" list={k:?}");
    }
    if let Some(k) = n.display_kind {
        line += &format!(" display={k:?}");
    }
    let f = &n.flags;
    for (on, name) in [
        (f.no_fill, "nofill"),
        (f.deep_link_target, "target"),
        (f.line_start, "line"),
        (f.delimiter_close, "dclose"),
        (f.line_continuation, "cont"),
        (f.no_print, "noprint"),
    ] {
        if on {
            line += &format!(" +{name}");
        }
    }
    if !n.table_cells.is_empty() {
        let cells: Vec<_> = n.table_cells.iter().map(|c| format!("{c:?}")).collect();
        line += &format!(" cells=[{}]", cells.join(", "));
    }
    if let Some(e) = &n.equation {
        line += &format!(" eqn={e:?}");
    }
    println!("{line}");
    for c in &n.children {
        print_node(c, depth + 1);
    }
}
