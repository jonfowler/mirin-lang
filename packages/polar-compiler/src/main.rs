use std::{env, path::PathBuf, process};

use polar_compiler::parse_file;

fn main() {
    let mut args = env::args_os();
    let program = args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .unwrap_or_else(|| "polar-compiler".to_owned());

    let Some(path) = args.next() else {
        eprintln!("usage: {program} <path-to-.plr-file>");
        process::exit(2);
    };

    if args.next().is_some() {
        eprintln!("usage: {program} <path-to-.plr-file>");
        process::exit(2);
    }

    match parse_file(PathBuf::from(path)) {
        Ok(cst) => {
            print!("{cst}");
        }
        Err(err) => {
            eprintln!("{err}");
            process::exit(1);
        }
    }
}
