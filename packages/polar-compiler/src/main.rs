use std::{env, path::PathBuf, process};

use polar_compiler::{parse_file, render_parse_error};

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

    let path = PathBuf::from(path);

    match parse_file(&path) {
        Ok(cst) => {
            print!("{cst}");
        }
        Err(err) => {
            let mut rendered = String::new();
            render_parse_error(&err, Some(&path), &mut rendered)
                .expect("rendering parse diagnostics should not fail");
            eprintln!("{rendered}");
            process::exit(1);
        }
    }
}
