//! `mantern`: man pages rendered natively in Tern.
//!
//! Takes exactly the arguments `man` does. When the output is a Tern pane and
//! the arguments name one page, the page is drawn as a native block among
//! the pane's output and stays live for `q` and SEE ALSO chips; anything else
//! runs the system `man` unchanged.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use libmandoc_rs::MacroSet;
use mantern::browse::{Browser, End};
use mantern::{is_section, page, tsp, tty};
use serde_json::{Value, json};

/// What `native` did.
enum Native {
    /// The page was shown and the user left.
    Drawn,
    /// Not ours to draw: the system `man` takes over.
    Declined,
    /// The session ended on the terminal (hung up, signalled, broken): there
    /// is no one left to show a manual to, so nothing else runs.
    Ended(ExitCode),
}

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let Some(system_man) = system_man() else {
        eprintln!("mantern: no system man found on PATH");
        return ExitCode::from(127);
    };
    match native(&system_man, &args) {
        Ok(Native::Drawn) => return ExitCode::SUCCESS,
        Ok(Native::Ended(code)) => return code,
        Ok(Native::Declined) => {}
        Err(err) => eprintln!("mantern: drawing natively failed ({err}); using {}", system_man.display()),
    }
    let err = Command::new(&system_man).args(&args).exec();
    eprintln!("mantern: {}: {err}", system_man.display());
    ExitCode::from(126)
}

/// Draws the page in Tern. `Declined` when this invocation isn't one we
/// draw (no Tern, not a terminal, not a single page, a page or a terminal
/// that can't do it): the system `man` handles it instead. An error means
/// the terminal was never touched.
fn native(system_man: &Path, args: &[OsString]) -> Result<Native, Box<dyn Error>> {
    if !eligible(args) {
        return Ok(declined("not a single page on a terminal outside a multiplexer"));
    }
    let Some(path) = page::locate(system_man, args) else {
        return Ok(declined("the system man found no page"));
    };
    let page = match page::parse(&path) {
        Ok(page) => page,
        Err(err) => return Ok(declined(&format!("the page can't be drawn natively: {err}"))),
    };
    if page.document.macro_set == MacroSet::None {
        return Ok(declined("the page uses neither man(7) nor mdoc(7)"));
    }

    let mut tty = tty::Tty::open_raw()?;
    let mut stdout = io::stdout().lock();
    let query = json!({ "q": "hello", "v": [1], "app": "mantern", "ver": env!("CARGO_PKG_VERSION") });
    let hello = match tty.hello(&mut stdout, &query.to_string(), Duration::from_secs(1)) {
        Ok(hello) => hello,
        Err(err) => return Ok(failed(&err)),
    };
    if let Some(code) = terminated(&tty) {
        return Ok(Native::Ended(code));
    }
    let Some(hello) = hello else {
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
        kinds: hello["kinds"]
            .as_array()
            .map(|kinds| kinds.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect()),
        // A terminal that granted none could never draw the first frame.
        credits: hello["credits"].as_u64().unwrap_or(2).max(1),
    };
    Ok(match browser.run(&mut tty, &mut wire, &page) {
        End::Quit => Native::Drawn,
        End::Declined(why) => match terminated(&tty) {
            Some(code) => Native::Ended(code),
            None => declined(&why),
        },
        End::Hangup => Native::Ended(ExitCode::from(HANGUP)),
        End::Signal(signal) => Native::Ended(ExitCode::from(128 + signal as u8)),
        End::Failed(err) => failed(&err),
    })
}

/// 128 + SIGHUP, the status of a process the terminal hung up on.
const HANGUP: u8 = 129;

/// The status to leave with when the terminal hung up or a termination
/// signal arrived (while the terminal was already restored or being so).
fn terminated(tty: &tty::Tty) -> Option<ExitCode> {
    match tty.signal() {
        Some(signal) => Some(ExitCode::from(128 + signal as u8)),
        None => tty.hung_up().then_some(ExitCode::from(HANGUP)),
    }
}

/// The terminal could not be read or written: say so, and don't start
/// another reader on it.
fn failed(err: &io::Error) -> Native {
    // Stderr may be the broken terminal itself; a failed warning is no failure.
    let _ = writeln!(io::stderr(), "mantern: terminal I/O failed: {err}");
    Native::Ended(ExitCode::FAILURE)
}

/// `Declined`, after saying why when `MANTERN_DEBUG` is set.
fn declined(reason: &str) -> Native {
    if env::var_os("MANTERN_DEBUG").is_some() {
        eprintln!("mantern: system man used: {reason}");
    }
    Native::Declined
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
