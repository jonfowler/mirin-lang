use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_polar-compiler"))
}

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn fail_examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fail-examples")
}

// --- invocation errors (exit 2) ---

#[test]
fn no_args_exits_2() {
    let status = bin().status().unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn nonexistent_file_exits_2() {
    let status = bin().arg("no-such-file.plr").status().unwrap();
    assert_eq!(status.code(), Some(2));
}

// --- success cases ---

#[test]
fn success_exits_zero_with_cst_on_stdout() {
    let output = bin()
        .arg(examples().join("add_constant.plr"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "expected CST on stdout");
    assert!(
        stdout.contains("source_file"),
        "expected CST root node in output"
    );
    assert!(
        stdout.contains("component_definition"),
        "expected component node in CST"
    );
    assert!(output.stderr.is_empty(), "expected no stderr on success");
}

#[test]
fn success_produces_no_stderr() {
    for name in &["counter", "mult_add", "accumulator", "pipeline"] {
        let output = bin()
            .arg(examples().join(format!("{name}.plr")))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{name}: unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// --- parse errors ---

#[test]
fn parse_error_exits_1_with_message_on_stderr() {
    let output = bin()
        .arg(fail_examples().join("missing-semicolon.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error:"),
        "expected error on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("missing-semicolon.plr"),
        "expected filename in output, got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "expected nothing on stdout");
}

// --- name resolution errors ---

#[test]
fn resolve_undefined_name_exits_1() {
    let status = bin()
        .arg(fail_examples().join("undefined-name.plr"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn resolve_undefined_name_message() {
    let output = bin()
        .arg(fail_examples().join("undefined-name.plr"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("undefined name `offset`"), "got: {stderr}");
    assert!(
        stderr.contains("undefined-name.plr"),
        "expected filename in output, got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "expected nothing on stdout");
}

#[test]
fn resolve_duplicate_def_message() {
    let output = bin()
        .arg(fail_examples().join("duplicate-def.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`process` is defined more than once in this file"),
        "got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "expected nothing on stdout");
}

#[test]
fn resolve_duplicate_var_message() {
    let output = bin()
        .arg(fail_examples().join("duplicate-var.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`count` is declared more than once as `var` in this block"),
        "got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "expected nothing on stdout");
}

#[test]
fn resolve_var_after_let_message() {
    let output = bin()
        .arg(fail_examples().join("var-after-let.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot declare `var acc` after a `let acc` binding in the same block"),
        "got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "expected nothing on stdout");
}

// --- output format: source excerpt is rendered ---

#[test]
fn resolve_error_includes_source_excerpt() {
    let output = bin()
        .arg(fail_examples().join("undefined-name.plr"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Expect a line number pointer and carets in the output
    assert!(
        stderr.contains(" --> "),
        "expected location pointer, got: {stderr}"
    );
    assert!(
        stderr.contains("  |"),
        "expected excerpt gutter, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "expected caret underline, got: {stderr}"
    );
}
