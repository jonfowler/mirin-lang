//! SystemVerilog reserved-word check over the emitted [`SvFile`].
//!
//! A Mirin identifier that survives to an SV identifier (a module/parameter/port
//! name, a `logic` decl, an instance name, a clock/reset) must not collide with
//! an SV-2017 keyword, or the generated Verilog is invalid. The reference
//! compiler hard-errors at emit time; this query reports the same, walking the
//! flattened `SvFile` so it catches names introduced by flattening too.

use crate::backend::ir::SvItem;
use crate::backend::lower::sv_file;
use crate::base::db::SourceRoot;

/// QUERY: SV reserved-word collisions in the crate's emitted SystemVerilog,
/// as rendered messages (no span — the name may be synthesised at flatten).
#[salsa::tracked(returns(ref))]
pub fn reserved_words(db: &dyn salsa::Database, krate: SourceRoot) -> Vec<String> {
    let mut out = Vec::new();
    for module in &sv_file(db, krate).modules {
        let mut check = |name: &str| {
            if is_reserved(name) {
                out.push(format!(
                    "identifier `{name}` (in module `{}`) is a SystemVerilog reserved word \
                     — rename it in the Mirin source",
                    module.name
                ));
            }
        };
        check(&module.name);
        for p in &module.parameters {
            check(&p.name);
        }
        for p in &module.ports {
            check(&p.name);
        }
        for item in &module.items {
            match item {
                SvItem::Logic(d) => check(&d.name),
                SvItem::Instance(inst) => check(&inst.name),
                SvItem::AlwaysFf(a) => {
                    check(&a.clock);
                    if let Some(rst) = &a.reset {
                        check(rst);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn is_reserved(ident: &str) -> bool {
    SV_RESERVED_WORDS.binary_search(&ident).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;

    fn diags(src: &str) -> Vec<String> {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src.to_owned());
        let krate = vfs.source_root(&mut db, "t.mrn");
        reserved_words(&db, krate).clone()
    }

    #[test]
    fn reserved_words_are_sorted_for_binary_search() {
        assert!(SV_RESERVED_WORDS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_port_named_input_collides() {
        let d = diags(
            "fn m { dom clk: Clock } ( input: uint(8) @clk ) -> uint(8) @clk { return input; }",
        );
        assert!(
            d.iter()
                .any(|m| m.contains("`input`") && m.contains("reserved word")),
            "{d:?}"
        );
    }

    #[test]
    fn ordinary_names_do_not_collide() {
        assert!(diags("fn f (value: uint(8)) -> uint(8) { return value; }").is_empty());
    }
}

static SV_RESERVED_WORDS: &[&str] = &[
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
