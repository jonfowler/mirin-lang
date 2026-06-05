//! SystemVerilog IR — a thin, `'static` tree between the flattened HIR and the
//! text emitter (`Display`). A faithful port of `polar-compiler`'s `svir::ir`,
//! starting with the combinational subset (`logic` decls + `assign`); clocked
//! `always_ff` and module `Instance`s arrive in Q5c/Q5d.
//!
//! Names are resolved to strings here (post-lowering), so the IR carries no
//! `DefId`/`'db` and is a clean salsa value.

use std::fmt;

#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub struct SvFile {
    pub modules: Vec<SvModule>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub struct SvModule {
    pub name: String,
    pub parameters: Vec<SvParameter>,
    pub ports: Vec<SvPort>,
    pub items: Vec<SvItem>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvParameter {
    pub name: String,
    pub default: Option<SvExpr>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvPort {
    pub direction: SvPortDirection,
    pub ty: SvType,
    pub name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvPortDirection {
    Input,
    Output,
}

/// SV value type. `None` width = 1-bit `logic`; `Some(w)` = `logic [w-1:0]`.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvType {
    pub width: Option<SvExpr>,
}

impl SvType {
    pub fn bit() -> Self {
        Self { width: None }
    }
    pub fn uint(width: SvExpr) -> Self {
        Self { width: Some(width) }
    }
    /// Render the optional packed range as ` [W-1:0]` (concrete widths
    /// pre-subtracted to look idiomatic) or `""` for single-bit.
    pub fn bracketed(&self) -> String {
        match &self.width {
            Some(w) => format!(" [{}:0]", w_minus_1(w)),
            None => String::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvItem {
    /// `logic [W-1:0] name;`
    Logic(SvLogicDecl),
    /// `assign lhs = rhs;`
    Assign { lhs: SvExpr, rhs: SvExpr },
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvLogicDecl {
    pub ty: SvType,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvExpr {
    /// Bare identifier.
    Ident(String),
    /// A pre-formatted literal dropped in verbatim (`"3"`, `"1'b0"`).
    Lit(String),
    /// `(lhs OP rhs)`.
    BinOp(SvBinOp, Box<SvExpr>, Box<SvExpr>),
    /// `expr - 1`, for width expressions like `[N-1:0]`.
    Sub1(Box<SvExpr>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvBinOp {
    Add,
    Mul,
}

// ----- Display: the deterministic pretty-printer -----

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

impl fmt::Display for SvItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Logic(d) => writeln!(f, "    logic{} {};", d.ty.bracketed(), d.name),
            Self::Assign { lhs, rhs } => writeln!(f, "    assign {lhs} = {rhs};"),
        }
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

fn w_minus_1(w: &SvExpr) -> String {
    if let SvExpr::Lit(s) = w
        && let Ok(n) = s.parse::<i64>()
        && n >= 1
    {
        return format!("{}", n - 1);
    }
    format!("{w}-1")
}
