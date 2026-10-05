//! `mantern`: man pages rendered natively in Tern.
//!
//! Takes exactly the arguments `man` does. When the output is a Tern pane and
//! the arguments name one page, the page is drawn as a native block among
//! the pane's output and stays live for `q` and SEE ALSO chips; anything else
//! runs the system `man` unchanged.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use libmandoc_rs::MacroSet;
use mantern::browse::Browser;
use mantern::{page, tsp, tty};
use serde_json::{Value, json};

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let Some(system_man) = system_man() else {
        eprintln!("mantern: no system man found on PATH");
        return ExitCode::from(127);
    };
    match native(&system_man, &args) {
        Ok(true) => return ExitCode::SUCCESS,
        Ok(false) => {}
        Err(err) => eprintln!("mantern: drawing natively failed ({err}); using {}", system_man.display()),
    }
    let err = Command::new(&system_man).args(&args).exec();
    eprintln!("mantern: {}: {err}", system_man.display());
    ExitCode::from(126)
}

/// Draws the page in Tern. `Ok(false)` when this invocation isn't one we
/// draw (no Tern, not a terminal, not a single page): the system `man`
/// handles it instead.
fn native(system_man: &Path, args: &[OsString]) -> Result<bool, Box<dyn Error>> {
    if !eligible(args) {
        return Ok(declined("not a single page on a terminal outside a multiplexer"));
    }
    let Some(path) = page::locate(system_man, args) else {
        return Ok(declined("the system man found no page"));
    };
    let page = page::parse(&path)?;
    if page.document.macro_set == MacroSet::None {
        return Ok(declined("the page uses neither man(7) nor mdoc(7)"));
    }

    let mut tty = tty::Tty::open_raw()?;
    let mut stdout = io::stdout().lock();
    let query = json!({ "q": "hello", "v": [1], "app": "mantern", "ver": env!("CARGO_PKG_VERSION") });
    let Some(hello) = tty.hello(&mut stdout, &query.to_string(), Duration::from_secs(1))? else {
        return Ok(declined("the terminal doesn't speak TSP"));
    };
    if env::var_os("MANTERN_DEBUG").is_some() {
        eprintln!("mantern: hello reply: {hello}");
    }
    if !has_feature(&hello, "flow") {
        return Ok(declined("the terminal has no flow surfaces"));
    }

    let apc = hello["apc"].as_u64().unwrap_or(65_536) as usize;
    let mut wire = tsp::Wire::new(stdout, apc);
    let browser = Browser {
        system_man: system_man.to_owned(),
        styles: has_feature(&hello, "styles"),
        allow_fold: env::var_os("MANTERN_FOLD").is_none_or(|v| v != "0"),
    };
    browser.run(&mut tty, &mut wire, &page)?;
    Ok(true)
}

/// `false`, after saying why when `MANTERN_DEBUG` is set.
fn declined(reason: &str) -> bool {
    if env::var_os("MANTERN_DEBUG").is_some() {
        eprintln!("mantern: system man used: {reason}");
    }
    false
}

/// One page (`[SECTION] NAME`) written to a terminal outside a multiplexer
/// (they swallow TSP), unless `MANTERN=0` opts out.
fn eligible(args: &[OsString]) -> bool {
    if env::var_os("MANTERN").is_some_and(|v| v == "0")
        || ["TMUX", "STY", "ZELLIJ"].iter().any(|v| env::var_os(v).is_some())
        // SAFETY: isatty only inspects the descriptor.
        || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
    {
        return false;
    }
    let Some(args) = args.iter().map(|a| a.to_str()).collect::<Option<Vec<_>>>() else {
        return false;
    };
    match args.as_slice() {
        [name] => !name.starts_with('-'),
        [section, name] => is_section(section) && !name.starts_with('-'),
        _ => false,
    }
}

fn is_section(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_digit() || c == 'n' || c == 'l')
        && s.len() <= 6
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn has_feature(hello: &Value, name: &str) -> bool {
    hello["features"].as_array().is_some_and(|f| f.iter().any(|v| v == name))
}

/// The first `man` on `PATH` that isn't this binary, so installing ourselves
/// as `man` never recurses into ourselves.
fn system_man() -> Option<PathBuf> {
    let me = env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join("man"))
        .filter(|candidate| is_executable(candidate))
        .find(|candidate| candidate.canonicalize().ok() != me)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
