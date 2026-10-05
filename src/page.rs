//! Finding and parsing a page: the system `man` locates it, libmandoc
//! parses it.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use libmandoc_rs::{
    Compression, Diagnostic, DiagnosticLevel, Document, IncludePolicy, ParseOptions, Parser, RenderFormat, Renderer,
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
    single_path(out.stdout)
}

/// The one path in `man -w` output. A path may hold any byte but NUL, so only
/// the record terminator is removed; several lines mean several candidate
/// pages, and guessing which one the system `man` would show is not ours to do.
fn single_path(mut stdout: Vec<u8>) -> Option<PathBuf> {
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    (!stdout.is_empty() && !stdout.contains(&b'\n')).then(|| PathBuf::from(OsString::from_vec(stdout)))
}

fn parser() -> Parser {
    Parser::new(ParseOptions { includes: IncludePolicy::SourceTree, compression: Compression::Auto })
}

/// Sources libmandoc can't decompress itself, decoded here. `None` means
/// libmandoc reads the file directly (plain, gzip, zstd).
fn decoded(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext @ ("xz" | "lzma" | "bz2")) => decode(ext, File::open(path)?).map(Some),
        _ => Ok(None),
    }
}

/// Every concatenated stream of an xz or bzip2 file, or a whole `.lzma`
/// (lzma-alone) file. `ext` is one of `xz`, `lzma`, `bz2`.
fn decode(ext: &str, input: impl Read) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    match ext {
        "xz" => {
            xz2::read::XzDecoder::new_multi_decoder(input).read_to_end(&mut out)?;
        }
        "lzma" => {
            let stream = xz2::stream::Stream::new_lzma_decoder(u64::MAX).map_err(io::Error::other)?;
            xz2::read::XzDecoder::new_stream(input, stream).read_to_end(&mut out)?;
        }
        _ => {
            bzip2::read::MultiBzDecoder::new(input).read_to_end(&mut out)?;
        }
    }
    Ok(out)
}

/// Whether a diagnostic means the document lost content, so the system
/// `man` should show the page. Tree truncation always does. libmandoc still
/// renders what it reports as unsupported, so that level doesn't. Errors
/// decline except the skips that cost nothing: an unknown macro or request,
/// and loading a macro package (`mso`). `so` is not among them: that page
/// is an alias whose body was never included. Style and warning
/// diagnostics leave the page whole.
pub fn loses_content(d: &Diagnostic) -> bool {
    if d.code().is_some() {
        return true;
    }
    d.level == DiagnosticLevel::Error
        && !(d.message.starts_with("skipping unknown macro") || d.message.trim_end() == "skipping insecure request: mso")
}

fn incomplete(diagnostics: &[Diagnostic]) -> Option<&Diagnostic> {
    diagnostics.iter().find(|d| loses_content(d))
}

pub struct Page {
    pub document: Document,
    source: Option<Vec<u8>>,
    path: PathBuf,
}

/// Parses the page; an error means the native view would be incomplete or
/// impossible and the system `man` should show it.
pub fn parse(path: &Path) -> Result<Page, Box<dyn std::error::Error>> {
    let source = decoded(path)?;
    let report = match &source {
        Some(bytes) => parser().parse_bytes(path, bytes)?,
        None => parser().parse_file(path)?,
    };
    if let Some(d) = incomplete(&report.diagnostics) {
        return Err(format!("{}: {:?}: {}", path.display(), d.level, d.message).into());
    }
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use libmandoc_rs::{Diagnostic, DiagnosticLevel};

    use super::{decode, incomplete, single_path};

    fn xz(data: &[u8]) -> Vec<u8> {
        let mut w = xz2::write::XzEncoder::new(Vec::new(), 6);
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    fn bz2(data: &[u8]) -> Vec<u8> {
        let mut w = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    fn lzma(data: &[u8]) -> Vec<u8> {
        let options = xz2::stream::LzmaOptions::new_preset(6).unwrap();
        let stream = xz2::stream::Stream::new_lzma_encoder(&options).unwrap();
        let mut w = xz2::write::XzEncoder::new_stream(Vec::new(), stream);
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn concatenated_streams_decode_whole() {
        let xz_two = [xz(b".TH A 1\n"), xz(b".SH NAME\n")].concat();
        assert_eq!(decode("xz", xz_two.as_slice()).unwrap(), b".TH A 1\n.SH NAME\n");
        let bz_two = [bz2(b".TH A 1\n"), bz2(b".SH NAME\n")].concat();
        assert_eq!(decode("bz2", bz_two.as_slice()).unwrap(), b".TH A 1\n.SH NAME\n");
    }

    #[test]
    fn lzma_alone_is_not_read_as_xz() {
        assert_eq!(decode("lzma", lzma(b".TH A 1\n").as_slice()).unwrap(), b".TH A 1\n");
    }

    fn diag(level: DiagnosticLevel, message: &str) -> Diagnostic {
        Diagnostic { level, message: message.to_owned(), location: None }
    }

    #[test]
    fn only_lost_content_declines() {
        let style = diag(DiagnosticLevel::Style, "no blank before macro");
        let warning = diag(DiagnosticLevel::Warning, "missing date");
        assert!(incomplete(&[style.clone(), warning.clone()]).is_none());
        // mandoc still renders what it calls unsupported.
        let unsupported = diag(DiagnosticLevel::Unsupported, "unsupported roff request: do");
        assert!(incomplete(&[unsupported]).is_none());
        // Harmless error skips.
        let mso = diag(DiagnosticLevel::Error, "skipping insecure request: mso");
        let unknown = diag(DiagnosticLevel::Error, "skipping unknown macro: .iX");
        assert!(incomplete(&[mso, unknown]).is_none());
        // Other errors lose content: an alias page, a table, anything else.
        let so = diag(DiagnosticLevel::Error, "skipping insecure request: so");
        assert_eq!(incomplete(&[warning.clone(), so.clone()]), Some(&so));
        let table = diag(DiagnosticLevel::Error, "tbl layout error");
        assert_eq!(incomplete(std::slice::from_ref(&table)), Some(&table));
        let cut = diag(
            DiagnosticLevel::Warning,
            "owned syntax tree exceeded the 256-level copy limit; deeper descendants were omitted",
        );
        assert_eq!(incomplete(&[style, cut.clone()]), Some(&cut));
    }

    #[test]
    fn locate_keeps_path_bytes_and_rejects_ambiguity() {
        use std::os::unix::ffi::OsStrExt;
        let path = single_path(b" /man/a\xff b.1 \n".to_vec()).unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b" /man/a\xff b.1 ");
        assert!(single_path(b"/man/a.1\n/man/b.1\n".to_vec()).is_none());
        assert!(single_path(b"\n".to_vec()).is_none());
        assert!(single_path(Vec::new()).is_none());
    }
}
