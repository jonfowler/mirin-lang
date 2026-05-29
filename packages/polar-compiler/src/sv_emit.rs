//! SV IR → SystemVerilog text.
//!
//! Two responsibilities:
//!
//! 1. Identifier validation: every user-derived identifier (port names,
//!    parameter names, logic decl names) is checked against the SV-2017
//!    reserved-word set. Collisions raise a hard error rather than being
//!    silently mangled — the user must rename in their Polar source.
//! 2. Text emission: delegates to the `Display` impls on `sv_ir`, which
//!    are deterministic and tested independently.

use std::fmt::Write;

use crate::sv_ir::{SvExpr, SvFile, SvItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    pub kind: EmitErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitErrorKind {
    /// A Polar identifier collides with a SV-2017 reserved word.
    ReservedWord { identifier: String, module: String },
}

impl std::fmt::Display for EmitErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedWord { identifier, module } => write!(
                f,
                "identifier `{identifier}` (in module `{module}`) is a SystemVerilog reserved word — rename it in the Polar source"
            ),
        }
    }
}

pub fn emit(file: &SvFile) -> Result<String, Vec<EmitError>> {
    let errors = validate_identifiers(file);
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut out = String::new();
    write!(out, "{file}").expect("write to string cannot fail");
    Ok(out)
}

pub fn render_emit_errors(errors: &[EmitError], f: &mut impl std::fmt::Write) -> std::fmt::Result {
    for (i, e) in errors.iter().enumerate() {
        if i > 0 {
            writeln!(f)?;
        }
        writeln!(f, "error: {}", e.kind)?;
    }
    Ok(())
}

// ============================================================================
// Identifier validation
// ============================================================================

fn validate_identifiers(file: &SvFile) -> Vec<EmitError> {
    let mut errors = Vec::new();
    for module in &file.modules {
        if is_reserved(&module.name) {
            errors.push(EmitError {
                kind: EmitErrorKind::ReservedWord {
                    identifier: module.name.clone(),
                    module: module.name.clone(),
                },
            });
        }
        for p in &module.parameters {
            if is_reserved(&p.name) {
                push_err(&mut errors, &p.name, &module.name);
            }
        }
        for p in &module.ports {
            if is_reserved(&p.name) {
                push_err(&mut errors, &p.name, &module.name);
            }
        }
        for item in &module.items {
            validate_item(item, &module.name, &mut errors);
        }
    }
    errors
}

fn push_err(errors: &mut Vec<EmitError>, ident: &str, module: &str) {
    errors.push(EmitError {
        kind: EmitErrorKind::ReservedWord {
            identifier: ident.to_owned(),
            module: module.to_owned(),
        },
    });
}

fn validate_item(item: &SvItem, module: &str, errors: &mut Vec<EmitError>) {
    match item {
        SvItem::Logic(d) => {
            if is_reserved(&d.name) {
                push_err(errors, &d.name, module);
            }
        }
        SvItem::Assign { lhs, rhs } => {
            validate_expr_idents(lhs, module, errors);
            validate_expr_idents(rhs, module, errors);
        }
        SvItem::AlwaysFf(a) => {
            // Clock and reset names already come from validated ports — but
            // double-check in case future paths supply synthesized values.
            if is_reserved(&a.clock) {
                push_err(errors, &a.clock, module);
            }
            if is_reserved(&a.reset) {
                push_err(errors, &a.reset, module);
            }
            for s in &a.reset_body {
                validate_expr_idents(&s.lhs, module, errors);
                validate_expr_idents(&s.rhs, module, errors);
            }
            for s in &a.clocked_body {
                validate_expr_idents(&s.lhs, module, errors);
                validate_expr_idents(&s.rhs, module, errors);
            }
        }
        SvItem::Instance(inst) => {
            if is_reserved(&inst.name) {
                push_err(errors, &inst.name, module);
            }
            for (_port, expr) in &inst.ports {
                validate_expr_idents(expr, module, errors);
            }
        }
    }
}

fn validate_expr_idents(expr: &SvExpr, module: &str, errors: &mut Vec<EmitError>) {
    match expr {
        SvExpr::Ident(s) => {
            if is_reserved(s) {
                push_err(errors, s, module);
            }
        }
        SvExpr::Lit(_) => {}
        SvExpr::BinOp(_, l, r) => {
            validate_expr_idents(l, module, errors);
            validate_expr_idents(r, module, errors);
        }
        SvExpr::Sub1(e) => validate_expr_idents(e, module, errors),
    }
}

/// IEEE 1800-2017 §C.1 reserved keywords. Sorted; no SV-AMS or pre-2017
/// keywords. We err on the side of completeness — false positives just push
/// the user to rename, which is the desired behaviour.
fn is_reserved(ident: &str) -> bool {
    SV_RESERVED_WORDS.binary_search(&ident).is_ok()
}

const SV_RESERVED_WORDS: &[&str] = &[
    "accept_on",
    "alias",
    "always",
    "always_comb",
    "always_ff",
    "always_latch",
    "and",
    "assert",
    "assign",
    "assume",
    "automatic",
    "before",
    "begin",
    "bind",
    "bins",
    "binsof",
    "bit",
    "break",
    "buf",
    "bufif0",
    "bufif1",
    "byte",
    "case",
    "casex",
    "casez",
    "cell",
    "chandle",
    "checker",
    "class",
    "clocking",
    "cmos",
    "config",
    "const",
    "constraint",
    "context",
    "continue",
    "cover",
    "covergroup",
    "coverpoint",
    "cross",
    "deassign",
    "default",
    "defparam",
    "design",
    "disable",
    "dist",
    "do",
    "edge",
    "else",
    "end",
    "endcase",
    "endchecker",
    "endclass",
    "endclocking",
    "endconfig",
    "endfunction",
    "endgenerate",
    "endgroup",
    "endinterface",
    "endmodule",
    "endpackage",
    "endprimitive",
    "endprogram",
    "endproperty",
    "endsequence",
    "endspecify",
    "endtable",
    "endtask",
    "enum",
    "event",
    "eventually",
    "expect",
    "export",
    "extends",
    "extern",
    "final",
    "first_match",
    "for",
    "force",
    "foreach",
    "forever",
    "fork",
    "forkjoin",
    "function",
    "generate",
    "genvar",
    "global",
    "highz0",
    "highz1",
    "if",
    "iff",
    "ifnone",
    "ignore_bins",
    "illegal_bins",
    "implements",
    "implies",
    "import",
    "incdir",
    "include",
    "initial",
    "inout",
    "input",
    "inside",
    "instance",
    "int",
    "integer",
    "interconnect",
    "interface",
    "intersect",
    "join",
    "join_any",
    "join_none",
    "large",
    "let",
    "liblist",
    "library",
    "local",
    "localparam",
    "logic",
    "longint",
    "macromodule",
    "matches",
    "medium",
    "modport",
    "module",
    "nand",
    "negedge",
    "nettype",
    "new",
    "nexttime",
    "nmos",
    "nor",
    "noshowcancelled",
    "not",
    "notif0",
    "notif1",
    "null",
    "or",
    "output",
    "package",
    "packed",
    "parameter",
    "pmos",
    "posedge",
    "primitive",
    "priority",
    "program",
    "property",
    "protected",
    "pull0",
    "pull1",
    "pulldown",
    "pullup",
    "pulsestyle_ondetect",
    "pulsestyle_onevent",
    "pure",
    "rand",
    "randc",
    "randcase",
    "randsequence",
    "rcmos",
    "real",
    "realtime",
    "ref",
    "reg",
    "reject_on",
    "release",
    "repeat",
    "restrict",
    "return",
    "rnmos",
    "rpmos",
    "rtran",
    "rtranif0",
    "rtranif1",
    "s_always",
    "s_eventually",
    "s_nexttime",
    "s_until",
    "s_until_with",
    "scalared",
    "sequence",
    "shortint",
    "shortreal",
    "showcancelled",
    "signed",
    "small",
    "soft",
    "solve",
    "specify",
    "specparam",
    "static",
    "string",
    "strong",
    "strong0",
    "strong1",
    "struct",
    "super",
    "supply0",
    "supply1",
    "sync_accept_on",
    "sync_reject_on",
    "table",
    "tagged",
    "task",
    "this",
    "throughout",
    "time",
    "timeprecision",
    "timeunit",
    "tran",
    "tranif0",
    "tranif1",
    "tri",
    "tri0",
    "tri1",
    "triand",
    "trior",
    "trireg",
    "type",
    "typedef",
    "union",
    "unique",
    "unique0",
    "unsigned",
    "until",
    "until_with",
    "untyped",
    "use",
    "uwire",
    "var",
    "vectored",
    "virtual",
    "void",
    "wait",
    "wait_order",
    "wand",
    "weak",
    "weak0",
    "weak1",
    "while",
    "wildcard",
    "wire",
    "with",
    "within",
    "wor",
    "xnor",
    "xor",
];

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{flatten_aggregates, lower_to_hir};
    use crate::resolve::resolve_file;
    use crate::surface_ir::parse_surface_source;
    use crate::sv_lower::lower_to_sv;
    use crate::typeck;

    fn build_sv(src: &str) -> Result<String, Vec<EmitError>> {
        let surface = parse_surface_source(src).expect("parse");
        let resolve = resolve_file(&surface);
        let hir = lower_to_hir(&surface, &resolve).expect("lower");
        let tc = typeck::check_file(&hir, &resolve);
        let hir = crate::hir::lower_method_calls(&hir, &tc.method_resolutions);
        let hir = crate::hir::desugar_user_calls(&hir).expect("desugar");
        let flat = flatten_aggregates(&hir, &tc.expr_types, &tc.local_types).expect("flatten");
        let sv = lower_to_sv(&flat, &resolve);
        emit(&sv)
    }

    #[test]
    fn sorted_keyword_list() {
        // Binary search requires the list be sorted; smoke-test the
        // invariant so future edits don't silently break the check.
        for win in SV_RESERVED_WORDS.windows(2) {
            assert!(win[0] < win[1], "{} >= {}", win[0], win[1]);
        }
    }

    #[test]
    fn known_keywords_detected() {
        assert!(is_reserved("input"));
        assert!(is_reserved("output"));
        assert!(is_reserved("module"));
        assert!(is_reserved("reg"));
        assert!(is_reserved("logic"));
        assert!(!is_reserved("inp"));
        assert!(!is_reserved("clk"));
        assert!(!is_reserved("rstn"));
    }

    #[test]
    fn emits_accumulator() {
        let s = build_sv(include_str!("../../../examples/working/accumulator.plr")).expect("emit");
        // Eyeball-check the key shapes; the exact whitespace can shift
        // without breaking SV semantics.
        assert!(s.contains("module accumulator"), "{s}");
        assert!(s.contains("input  logic clk"), "{s}");
        assert!(s.contains("input  logic [7:0] data"), "{s}");
        assert!(s.contains("output logic [7:0] result"), "{s}");
        assert!(s.contains("always_ff @(posedge clk)"), "{s}");
        assert!(s.contains("if (!rstn)"), "{s}");
        assert!(s.contains("acc <= 0;"), "{s}");
        assert!(s.contains("acc <= (acc + data);"), "{s}");
        assert!(s.contains("assign result = acc;"), "{s}");
        assert!(s.contains("endmodule"), "{s}");
    }

    #[test]
    fn emits_counter_with_parameter() {
        let s = build_sv(include_str!("../../../examples/working/counter.plr")).expect("emit");
        assert!(s.contains("#(parameter int bits"), "{s}");
        assert!(s.contains("[bits-1:0]"), "{s}");
    }

    #[test]
    fn emits_packet_struct() {
        let s =
            build_sv(include_str!("../../../examples/working/packet_struct.plr")).expect("emit");
        assert!(s.contains("inp__valid"), "{s}");
        assert!(s.contains("inp__payload"), "{s}");
        assert!(s.contains("result__valid"), "{s}");
        assert!(s.contains("result__payload"), "{s}");
        // Two always_ff blocks (one per field).
        let always_ff = s.matches("always_ff").count();
        assert_eq!(always_ff, 2, "{s}");
    }

    #[test]
    fn shadowed_lets_emit_unique_sv_names() {
        // pipeline.plr shadows the `data` param with two `let data = …`
        // bindings. The emitter must rename the shadows so SV doesn't see
        // three declarations of the same identifier.
        let s = build_sv(include_str!("../../../examples/working/pipeline.plr")).expect("emit");
        // The original `data` port and the renamed shadows should both appear.
        assert!(s.contains("input  logic [7:0] data,"), "{s}");
        assert!(s.contains("data_1"), "{s}");
        assert!(s.contains("data_2"), "{s}");
        // No duplicate logic decls.
        let dup_count = s.matches("logic [7:0] data;").count();
        assert_eq!(dup_count, 0, "unexpected duplicate `data` decl: {s}");
    }

    #[test]
    fn emits_multi_call_with_lifted_nested_calls() {
        // multi_call.plr's `add9` writes `return add3(add3(x))` — a nested
        // user-fn call. The out_args pass lifts the inner call into a
        // synthetic temp; sv_lower emits three `add3` instances in `add9`.
        let s = build_sv(include_str!("../../../examples/working/multi_call.plr")).expect("emit");
        let instances = s.matches("add3 add3").count();
        assert_eq!(
            instances, 3,
            "expected 3 add3 instances, got {instances} in:\n{s}"
        );
    }

    #[test]
    fn emits_delay_impl_with_chained_method_calls() {
        // delay_impl.plr defines `reg2` as a method on `Option` and writes
        // `upstream.reg2(rstn).reg2(rstn)` — chained method dispatch. The
        // out_args + method_lower combo lifts the inner call into a temp
        // and produces two reg2 instances.
        let s = build_sv(include_str!("../../../examples/working/delay_impl.plr")).expect("emit");
        let instances = s.matches("reg2 reg2").count();
        assert_eq!(
            instances, 2,
            "expected 2 reg2 instances, got {instances} in:\n{s}"
        );
    }

    #[test]
    fn emits_delay_with_out_arg_call_syntax() {
        // delay.plr exercises `out`-direction params at the call site,
        // both named (`f { downstream => ds }(…)`) and positional
        // (`f(…, out => ds)`). Each form should connect the callee's
        // out-direction port to a caller-side local.
        let s = build_sv(include_str!("../../../examples/working/delay.plr")).expect("emit");
        // Implicit-var `ds` introduced by the source-arrow becomes two
        // logic decls (per-leaf of `Option @clk`).
        assert!(s.contains("logic ds__valid;"), "{s}");
        assert!(s.contains("logic [7:0] ds__payload;"), "{s}");
        // Named-source-arrow call wires the callee's `downstream__*`
        // output to the caller's `ds__*` leaves.
        assert!(
            s.contains(".downstream__valid(ds__valid)"),
            "expected named-source-arrow connection, in:\n{s}"
        );
        // Positional out-arg call wires the same way.
        assert!(
            s.contains(".downstream__payload(ds__payload)"),
            "expected positional out-arg connection, in:\n{s}"
        );
    }

    #[test]
    fn emits_delay_with_user_fn_instances() {
        // delay.plr's `double_delay` instantiates `reg2` twice; flatten +
        // out_args + sv_lower should produce two SV instance declarations.
        let s = build_sv(include_str!("../../../examples/working/delay.plr")).expect("emit");
        // Two `reg2` instances appear (one named `reg2`, one `reg2_1`).
        assert!(s.contains("module reg2"), "{s}");
        assert!(s.contains("module double_delay"), "{s}");
        assert!(s.contains("reg2 reg2 ("), "{s}");
        assert!(s.contains("reg2 reg2_1 ("), "{s}");
        // Aggregate args expanded into per-leaf port connections.
        assert!(s.contains(".a__valid(upstream__valid)"), "{s}");
        assert!(s.contains(".a__payload(upstream__payload)"), "{s}");
        assert!(s.contains(".result__valid(delay1__valid)"), "{s}");
        assert!(s.contains(".result__payload(delay1__payload)"), "{s}");
    }

    #[test]
    fn emits_working_examples_without_errors() {
        for (name, source) in crate::test_support::working_examples() {
            let _ = build_sv(&source)
                .unwrap_or_else(|e| panic!("example `{name}` failed to emit: {e:?}"));
        }
    }

    #[test]
    fn fail_example_with_reserved_word_errors() {
        // examples/fail-expected/sv-reserved-word.plr uses `input` as a parameter
        // name. The earlier passes accept it; the emitter should reject it.
        let src = include_str!("../../../examples/fail-expected/sv-reserved-word.plr");
        let errs = build_sv(src).expect_err("expected emission error");
        assert!(
            errs.iter().any(|e| matches!(
                &e.kind,
                EmitErrorKind::ReservedWord { identifier, .. } if identifier == "input"
            )),
            "expected `input` reserved-word error, got: {errs:?}"
        );
    }
}
