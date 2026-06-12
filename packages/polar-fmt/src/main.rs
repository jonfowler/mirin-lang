//! `polar-fmt` CLI.
//!
//! Usage:
//!   polar-fmt <file.plr>...      format files in place
//!   polar-fmt --check <file>...  exit non-zero if any file is not formatted
//!   polar-fmt [-]                read stdin, write formatted source to stdout
//!
//! With no file arguments (or a lone `-`) it reads stdin and writes stdout,
//! the rustfmt convention for editor integration.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs};

use polar_fmt::{FormatError, format_str};

struct Args {
    check: bool,
    files: Vec<PathBuf>,
    use_stdin: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut check = false;
    let mut files = Vec::new();
    let mut use_stdin = false;

    for raw in env::args_os().skip(1) {
        let s = raw.to_string_lossy().into_owned();
        match s.as_str() {
            "--check" => check = true,
            "-h" | "--help" => return Err(usage()),
            "-" => use_stdin = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`\n\n{}", usage()));
            }
            _ => files.push(PathBuf::from(s)),
        }
    }

    if files.is_empty() {
        use_stdin = true;
    }
    Ok(Args {
        check,
        files,
        use_stdin,
    })
}

fn usage() -> String {
    "usage: polar-fmt [--check] [<file.plr>...]\n\
     \n  with no files (or `-`), reads stdin and writes formatted source to stdout\n\
     \n  --check   do not write; exit 1 if any file would change"
        .to_string()
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    if args.use_stdin {
        return run_stdin(args.check);
    }

    let mut changed = false;
    let mut had_error = false;
    for path in &args.files {
        match run_file(path, args.check) {
            Ok(file_changed) => changed |= file_changed,
            Err(()) => had_error = true,
        }
    }

    if had_error {
        ExitCode::from(2)
    } else if args.check && changed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn run_stdin(check: bool) -> ExitCode {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("error: failed to read stdin");
        return ExitCode::from(2);
    }
    match format_str(&input) {
        Ok(formatted) => {
            if check {
                return if formatted == input {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                };
            }
            if io::stdout().write_all(formatted.as_bytes()).is_err() {
                return ExitCode::from(2);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Returns `Ok(true)` if the file was (or would be) changed.
fn run_file(path: &Path, check: bool) -> Result<bool, ()> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}: {e}", path.display());
            return Err(());
        }
    };
    let formatted = match format_str(&source) {
        Ok(f) => f,
        Err(FormatError::Parse) => {
            eprintln!("error: {}: syntax errors; not formatting", path.display());
            return Err(());
        }
    };

    if formatted == source {
        return Ok(false);
    }
    if check {
        eprintln!("{}: not formatted", path.display());
        return Ok(true);
    }
    if let Err(e) = fs::write(path, formatted.as_bytes()) {
        eprintln!("error: {}: {e}", path.display());
        return Err(());
    }
    Ok(true)
}
