//! SystemVerilog intermediate representation.
//!
//! Sits between flattened HIR and the text emitter. The shape is intentionally
//! thin — it carries just enough structure for `sv_emit` to lay out a SV file
//! deterministically, and just enough type information to drive
//! `[N-1:0]`-style declarations. Analysis lives in earlier passes, not here.
//!
//! The first-pass scope (see `planning/system_verilog_backend.md`) covers
//! parametric scalar modules with combinational `assign`s and clocked
//! `always_ff` blocks using synchronous active-low reset. Module instances
//! land later when user-function calls cross module boundaries.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvFile {
    pub modules: Vec<SvModule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvModule {
    pub name: String,
    /// SV `parameter` declarations (e.g. `parameter int N = 8`). Each entry
    /// becomes one `#(...)` slot at module-header time.
    pub parameters: Vec<SvParameter>,
    /// Flat list of input/output ports.
    pub ports: Vec<SvPort>,
    /// Module body items, in source order.
    pub items: Vec<SvItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvParameter {
    pub name: String,
    /// First-pass parameters are all `int`. Wider parameter types land when
    /// the surface grammar grows them.
    pub default: Option<SvExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvPort {
    pub direction: SvPortDirection,
    pub ty: SvType,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvPortDirection {
    Input,
    Output,
}

/// SV value type for a port or local declaration. First pass uses `logic`
/// everywhere (no `wire`/`reg` split — `always_ff`-assigned signals are still
/// declared `logic` per SV-2017 idiom).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvType {
    /// `None` = 1-bit (no packed range); `Some(expr)` = `[expr-1:0]`.
    pub width: Option<SvExpr>,
}

impl SvType {
    pub fn bit() -> Self {
        Self { width: None }
    }
    pub fn uint(width: SvExpr) -> Self {
        Self { width: Some(width) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvItem {
    /// `logic [W-1:0] name;`
    Logic(SvLogicDecl),
    /// `assign lhs = rhs;`
    Assign { lhs: SvExpr, rhs: SvExpr },
    /// `always_ff @(posedge clk) begin if (!rstn) … else … end`. First pass
    /// is always synchronous active-low reset.
    AlwaysFf(SvAlwaysFf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvLogicDecl {
    pub ty: SvType,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvAlwaysFf {
    /// The clock signal (just the identifier — sensitivity is fixed to
    /// `posedge`).
    pub clock: String,
    /// The reset signal (just the identifier — polarity is fixed to
    /// active-low for first pass: the emitter writes `if (!rstn)`).
    pub reset: String,
    /// Non-blocking assignment(s) executed when reset is asserted.
    pub reset_body: Vec<SvSeqAssign>,
    /// Non-blocking assignment(s) executed on the clock edge when reset is
    /// not asserted.
    pub clocked_body: Vec<SvSeqAssign>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvSeqAssign {
    pub lhs: SvExpr,
    pub rhs: SvExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvExpr {
    /// Bare identifier.
    Ident(String),
    /// A pre-formatted literal (e.g. `"'0"`, `"1'b0"`, `"8'd3"`). The emitter
    /// drops these in verbatim — width/format choices belong to lowering.
    Lit(String),
    /// `lhs OP rhs`.
    BinOp(SvBinOp, Box<SvExpr>, Box<SvExpr>),
    /// `expr - 1` etc., used in width expressions like `[N-1:0]`.
    /// Same as `BinOp(Sub, ...)`; kept separate for clarity of intent.
    Sub1(Box<SvExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvBinOp {
    Add,
    Mul,
}

// ============================================================================
// Display — small, deterministic pretty-printer. Used for diagnostics and
// tests; the production emitter lives in `sv_emit.rs` and can choose a
// different layout if needed.
// ============================================================================

impl fmt::Display for SvFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, m) in self.modules.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{m}")?;
        }
        Ok(())
    }
}

impl fmt::Display for SvModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "module {}", self.name)?;
        if !self.parameters.is_empty() {
            write!(f, " #(")?;
            for (i, p) in self.parameters.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "parameter int {}", p.name)?;
                if let Some(d) = &p.default {
                    write!(f, " = {d}")?;
                }
            }
            write!(f, ")")?;
        }
        writeln!(f, " (")?;
        for (i, p) in self.ports.iter().enumerate() {
            let comma = if i + 1 < self.ports.len() { "," } else { "" };
            writeln!(f, "    {p}{comma}")?;
        }
        writeln!(f, ");")?;
        for item in &self.items {
            write!(f, "{item}")?;
        }
        writeln!(f, "endmodule")
    }
}

impl fmt::Display for SvPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dir = match self.direction {
            SvPortDirection::Input => "input ",
            SvPortDirection::Output => "output",
        };
        write!(f, "{dir} logic{} {}", self.ty.bracketed(), self.name)
    }
}

impl SvType {
    /// Render the optional packed range as ` [W-1:0]` (note the leading
    /// space) or `""` for single-bit. Used both for port declarations and
    /// for internal `logic` decls.
    pub fn bracketed(&self) -> String {
        match &self.width {
            Some(w) => format!(" [{}:0]", w_minus_1(w)),
            None => String::new(),
        }
    }
}

fn w_minus_1(w: &SvExpr) -> String {
    // Concrete integer widths get pre-subtracted so the emitted SV looks
    // idiomatic (`[7:0]` rather than `[8-1:0]`). Symbolic widths stay
    // symbolic and rely on the SV elaborator.
    if let SvExpr::Lit(s) = w {
        if let Ok(n) = s.parse::<i64>() {
            if n >= 1 {
                return format!("{}", n - 1);
            }
        }
    }
    format!("{w}-1")
}

impl fmt::Display for SvItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Logic(d) => writeln!(f, "    logic{} {};", d.ty.bracketed(), d.name),
            Self::Assign { lhs, rhs } => writeln!(f, "    assign {lhs} = {rhs};"),
            Self::AlwaysFf(a) => write!(f, "{a}"),
        }
    }
}

impl fmt::Display for SvAlwaysFf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "    always_ff @(posedge {}) begin", self.clock)?;
        writeln!(f, "        if (!{}) begin", self.reset)?;
        for a in &self.reset_body {
            writeln!(f, "            {} <= {};", a.lhs, a.rhs)?;
        }
        writeln!(f, "        end else begin")?;
        for a in &self.clocked_body {
            writeln!(f, "            {} <= {};", a.lhs, a.rhs)?;
        }
        writeln!(f, "        end")?;
        writeln!(f, "    end")
    }
}

impl fmt::Display for SvExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ident(s) => f.write_str(s),
            Self::Lit(s) => f.write_str(s),
            Self::BinOp(op, l, r) => {
                let op = match op {
                    SvBinOp::Add => "+",
                    SvBinOp::Mul => "*",
                };
                write!(f, "({l} {op} {r})")
            }
            Self::Sub1(e) => write!(f, "{e}-1"),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_minimal_module() {
        let m = SvModule {
            name: "passthrough".to_owned(),
            parameters: vec![],
            ports: vec![
                SvPort {
                    direction: SvPortDirection::Input,
                    ty: SvType::uint(SvExpr::Lit("8".to_owned())),
                    name: "data".to_owned(),
                },
                SvPort {
                    direction: SvPortDirection::Output,
                    ty: SvType::uint(SvExpr::Lit("8".to_owned())),
                    name: "result".to_owned(),
                },
            ],
            items: vec![SvItem::Assign {
                lhs: SvExpr::Ident("result".to_owned()),
                rhs: SvExpr::Ident("data".to_owned()),
            }],
        };
        let s = format!("{m}");
        assert!(s.contains("module passthrough"), "{s}");
        assert!(s.contains("input  logic [7:0] data"), "{s}");
        assert!(s.contains("output logic [7:0] result"), "{s}");
        assert!(s.contains("assign result = data;"), "{s}");
    }

    #[test]
    fn renders_parametric_module() {
        let m = SvModule {
            name: "p".to_owned(),
            parameters: vec![SvParameter {
                name: "N".to_owned(),
                default: Some(SvExpr::Lit("8".to_owned())),
            }],
            ports: vec![SvPort {
                direction: SvPortDirection::Output,
                ty: SvType::uint(SvExpr::Ident("N".to_owned())),
                name: "out".to_owned(),
            }],
            items: vec![],
        };
        let s = format!("{m}");
        assert!(s.contains("#(parameter int N = 8)"), "{s}");
        assert!(s.contains("[N-1:0] out"), "{s}");
    }

    #[test]
    fn renders_always_ff() {
        let a = SvAlwaysFf {
            clock: "clk".to_owned(),
            reset: "rstn".to_owned(),
            reset_body: vec![SvSeqAssign {
                lhs: SvExpr::Ident("acc".to_owned()),
                rhs: SvExpr::Lit("'0".to_owned()),
            }],
            clocked_body: vec![SvSeqAssign {
                lhs: SvExpr::Ident("acc".to_owned()),
                rhs: SvExpr::BinOp(
                    SvBinOp::Add,
                    Box::new(SvExpr::Ident("acc".to_owned())),
                    Box::new(SvExpr::Ident("data".to_owned())),
                ),
            }],
        };
        let s = format!("{a}");
        assert!(s.contains("always_ff @(posedge clk)"), "{s}");
        assert!(s.contains("if (!rstn)"), "{s}");
        assert!(s.contains("acc <= '0;"), "{s}");
        assert!(s.contains("acc <= (acc + data);"), "{s}");
    }
}
