//! Finding and parsing a page: the system `man` locates it, libmandoc
//! parses it.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use libmandoc_rs::{
    Compression, Document, IncludePolicy, ParseOptions, Parser, RenderFormat, Renderer,
};

/// The page file the system `man` would show for these arguments.
pub fn locate(system_man: &Path, args: &[OsString]) -> Option<PathBuf> {
    let out = Command::new(system_man)
        .arg("-w")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let first = String::from_utf8(out.stdout).ok()?.lines().next()?.trim().to_owned();
    (!first.is_empty()).then(|| PathBuf::from(first))
}

fn parser() -> Parser {
    Parser::new(ParseOptions { includes: IncludePolicy::SourceTree, compression: Compression::Auto })
}

/// Sources libmandoc can't decompress itself, decoded here. `None` means
/// libmandoc reads the file directly (plain, gzip, zstd).
fn decoded(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    match path.extension().and_then(|e| e.to_str()) {
        Some("xz" | "lzma") => {
            xz2::read::XzDecoder::new(File::open(path)?).read_to_end(&mut out)?;
        }
        Some("bz2") => {
            bzip2::read::BzDecoder::new(File::open(path)?).read_to_end(&mut out)?;
        }
        _ => return Ok(None),
    }
    Ok(Some(out))
}

pub struct Page {
    pub document: Document,
    source: Option<Vec<u8>>,
    path: PathBuf,
}

pub fn parse(path: &Path) -> Result<Page, Box<dyn std::error::Error>> {
    let source = decoded(path)?;
    let report = match &source {
        Some(bytes) => parser().parse_bytes(path, bytes)?,
        None => parser().parse_file(path)?,
    };
    Ok(Page { document: report.document, source, path: path.to_owned() })
}

impl Page {
    /// How many lines the page takes as terminal text at `width` columns.
    pub fn rendered_lines(&self, width: usize) -> Option<usize> {
        let r = Renderer::new(RenderFormat::Utf8).with_parser(parser()).with_width(width);
        let report = match &self.source {
            Some(bytes) => r.render_bytes(&self.path, bytes),
            None => r.render_file(&self.path),
        };
        report.ok().map(|r| r.output.lines().count())
    }
}
