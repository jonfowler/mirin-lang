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

/// Unique tempdir for SV output, scoped to the test binary's tmpdir so
/// parallel runs don't collide.
fn tmp_out_dir(suffix: &str) -> PathBuf {
    let base = env!("CARGO_TARGET_TMPDIR");
    let dir = Path::new(base).join(format!("sv-out-{suffix}"));
    // Best-effort clean-up of stale contents from a prior run.
    let _ = std::fs::remove_dir_all(&dir);
    dir
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
fn emit_cst_prints_to_stdout() {
    let output = bin()
        .args(["--emit", "cst"])
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
        stdout.contains("function_definition"),
        "expected function node in CST"
    );
    assert!(output.stderr.is_empty(), "expected no stderr on success");
}

#[test]
fn default_emits_sv_to_out_dir() {
    let out_dir = tmp_out_dir("default_emits_sv");
    let output = bin()
        .args(["--out".as_ref(), out_dir.as_os_str()])
        .arg(examples().join("accumulator.plr"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sv_path = out_dir.join("accumulator.sv");
    let sv = std::fs::read_to_string(&sv_path)
        .unwrap_or_else(|e| panic!("expected SV at {}: {e}", sv_path.display()));
    assert!(sv.contains("module accumulator"), "{sv}");
    assert!(sv.contains("always_ff @(posedge clk)"), "{sv}");
    assert!(sv.contains("if (!rstn)"), "{sv}");
    assert!(output.stderr.is_empty(), "unexpected stderr");
}

#[test]
fn success_produces_no_stderr() {
    let out_dir = tmp_out_dir("no_stderr");
    for name in &["counter", "mult_add", "accumulator", "pipeline"] {
        let output = bin()
            .args(["--out".as_ref(), out_dir.as_os_str()])
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

#[test]
fn reserved_word_exits_1_with_message() {
    let out_dir = tmp_out_dir("reserved_word");
    let output = bin()
        .args(["--out".as_ref(), out_dir.as_os_str()])
        .arg(fail_examples().join("sv-reserved-word.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`input`") && stderr.contains("reserved word"),
        "expected reserved-word error mentioning `input`, got: {stderr}"
    );
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

// --- direction errors ---

#[test]
fn direction_unknown_named_arg_message() {
    let output = bin()
        .arg(fail_examples().join("unknown-named-arg.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`target` has no named parameter `typo`"),
        "got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "expected nothing on stdout");
}

#[test]
fn direction_source_arrow_on_fn_param_message() {
    let output = bin()
        .arg(fail_examples().join("source-arrow-on-fn-param.plr"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`=>` cannot drive `target`'s `rstn`"),
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
