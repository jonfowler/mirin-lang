//! SystemVerilog IR — a thin, `'static` tree between the flattened HIR and the
//! text emitter (`Display`). A faithful port of `mirin-compiler`'s `svir::ir`,
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
    /// `logic signed [..]` — sint's two's-complement vectors.
    pub signed: bool,
    /// Unpacked-array dims, outermost first — rendered AFTER the name
    /// (`logic [3:0] v [0:2]`). Vec flattening fills these.
    pub unpacked: Vec<SvExpr>,
}

impl SvType {
    pub fn bit() -> Self {
        Self {
            width: None,
            signed: false,
            unpacked: Vec::new(),
        }
    }
    pub fn uint(width: SvExpr) -> Self {
        Self {
            width: Some(width),
            signed: false,
            unpacked: Vec::new(),
        }
    }
    pub fn sint(width: SvExpr) -> Self {
        Self {
            width: Some(width),
            signed: true,
            unpacked: Vec::new(),
        }
    }
    /// Render the optional packed range as ` [W-1:0]` (concrete widths
    /// pre-subtracted to look idiomatic) or `""` for single-bit, with a
    /// ` signed` qualifier first when applicable.
    /// The unpacked dims after the name: ` [0:N-1]` each.
    pub fn unpacked_suffix(&self) -> String {
        self.unpacked
            .iter()
            .map(|n| format!(" [0:{}]", w_minus_1(n)))
            .collect()
    }

    pub fn bracketed(&self) -> String {
        let sign = if self.signed { " signed" } else { "" };
        match &self.width {
            Some(w) => format!("{sign} [{}:0]", w_minus_1(w)),
            None => sign.to_owned(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvItem {
    CombAssert(SvCombAssert),
    /// `initial begin lhs = rhs; … end` — power-on state.
    Initial(Vec<(SvExpr, SvExpr)>),
    /// A named generate-for: structural replication with a RECOVERABLE
    /// hierarchy — instance paths are `label[i].name`
    /// (planning/for_loops.md).
    GenerateFor(SvGenerateFor),
    /// Raw verilog text from an inline-verilog fn body, emitted as-is
    /// (dedented to the module body's indentation).
    Verbatim(String),
    /// `logic [W-1:0] name;`
    Logic(SvLogicDecl),
    /// `assign lhs = rhs;`
    Assign {
        lhs: SvExpr,
        rhs: SvExpr,
    },
    /// `always_ff @(posedge clk) begin … end`, synchronous active-low reset.
    AlwaysFf(SvAlwaysFf),
    /// `always_comb begin … end` — combinational procedural block.
    AlwaysComb(SvAlwaysComb),
    /// `module inst (.port(expr), …);` — a submodule instantiation.
    Instance(SvInstance),
    /// `initial begin assert (cond); end` — a discharged width obligation.
    InitialAssert {
        cond: SvExpr,
    },
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvInstance {
    /// The instantiated module's name (the callee).
    pub module: String,
    /// The instance name within the surrounding module.
    pub name: String,
    /// SV parameter bindings (`#(.n(8))`) — the callee's Const-kind generics
    /// at this call site, in declared order.
    pub parameters: Vec<(String, SvExpr)>,
    /// Port connections in declaration order: `(port_name, expression)`.
    pub connections: Vec<(String, SvExpr)>,
}

/// `for (genvar i = 0; i < N; i++) begin : label … end`.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvGenerateFor {
    pub var: String,
    pub bound: SvExpr,
    pub label: String,
    pub items: Vec<SvItem>,
}

/// `always_comb assert (i < N);` — a simulation-time bounds check on a
/// dynamic vector/bits index (planning/vectors.md). Synthesis ignores it;
/// simulation fires exactly when the out-of-range access happens.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvCombAssert {
    pub cond: String,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvLogicDecl {
    pub ty: SvType,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvAlwaysFf {
    pub clock: String,
    /// Active-low reset signal; `None` = reset-less (`when` lowers here).
    pub reset: Option<String>,
    pub reset_body: Vec<SvSeqAssign>,
    pub clocked_body: Vec<SvSeqAssign>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvSeqAssign {
    pub lhs: SvExpr,
    pub rhs: SvExpr,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvAlwaysComb {
    pub body: Vec<SvCombStmt>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvCombStmt {
    Assign { lhs: SvExpr, rhs: SvExpr },
    If(SvCombIf),
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvCombIf {
    pub cond: SvExpr,
    pub then_branch: Vec<SvCombStmt>,
    pub else_branch: Vec<SvCombStmt>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvExpr {
    /// Bare identifier.
    Ident(String),
    /// A pre-formatted literal dropped in verbatim (`"3"`, `"1'b0"`).
    Lit(String),
    /// `(lhs OP rhs)`.
    BinOp(SvBinOp, Box<SvExpr>, Box<SvExpr>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub enum SvBinOp {
    Add,
    Sub,
    Mul,
    Eq,
    Lt,
    And,
    Or,
}

/// `(-x)` — the one unary operator (Neg on sint).
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SvNeg(pub Box<SvExpr>);

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
        write!(
            f,
            "{dir} logic{} {}{}",
            self.ty.bracketed(),
            self.name,
            self.ty.unpacked_suffix()
        )
    }
}

impl fmt::Display for SvItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerateFor(g) => {
                writeln!(
                    f,
                    "    for (genvar {v} = 0; {v} < {b}; {v}++) begin : {l}",
                    v = g.var,
                    b = g.bound,
                    l = g.label
                )?;
                for item in &g.items {
                    // Re-indent the item's own rendering one level deeper.
                    let rendered = item.to_string();
                    for line in rendered.lines() {
                        if line.is_empty() {
                            writeln!(f)?;
                        } else {
                            writeln!(f, "    {line}")?;
                        }
                    }
                }
                writeln!(f, "    end")
            }
            Self::Initial(assigns) => {
                writeln!(f, "    initial begin")?;
                for (l, r) in assigns {
                    writeln!(f, "        {l} = {r};")?;
                }
                writeln!(f, "    end")
            }
            Self::CombAssert(a) => {
                writeln!(f, "    always_comb assert ({});", a.cond)
            }
            Self::Logic(d) => writeln!(
                f,
                "    logic{} {}{};",
                d.ty.bracketed(),
                d.name,
                d.ty.unpacked_suffix()
            ),
            Self::Assign { lhs, rhs } => writeln!(f, "    assign {lhs} = {rhs};"),
            Self::AlwaysFf(a) => write!(f, "{a}"),
            Self::AlwaysComb(a) => {
                writeln!(f, "    always_comb begin")?;
                for s in &a.body {
                    fmt_comb_stmt(f, s, 8)?;
                }
                writeln!(f, "    end")
            }
            Self::Instance(inst) => {
                if inst.parameters.is_empty() {
                    writeln!(f, "    {} {} (", inst.module, inst.name)?;
                } else {
                    writeln!(f, "    {} #(", inst.module)?;
                    for (i, (p, v)) in inst.parameters.iter().enumerate() {
                        let sep = if i + 1 < inst.parameters.len() {
                            ","
                        } else {
                            ""
                        };
                        writeln!(f, "        .{p}({v}){sep}")?;
                    }
                    writeln!(f, "    ) {} (", inst.name)?;
                }
                for (i, (port, expr)) in inst.connections.iter().enumerate() {
                    let sep = if i + 1 < inst.connections.len() {
                        ","
                    } else {
                        ""
                    };
                    writeln!(f, "        .{port}({expr}){sep}")?;
                }
                writeln!(f, "    );")
            }
            Self::InitialAssert { cond } => {
                writeln!(f, "    initial begin")?;
                writeln!(f, "        assert ({cond});")?;
                writeln!(f, "    end")
            }
            Self::Verbatim(text) => {
                // Dedent to the common leading whitespace, re-indent to the
                // module body, drop surrounding blank lines.
                let lines: Vec<&str> = text.lines().skip_while(|l| l.trim().is_empty()).collect();
                let lines: Vec<&str> = lines
                    .iter()
                    .rev()
                    .skip_while(|l| l.trim().is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let dedent = lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| l.len() - l.trim_start().len())
                    .min()
                    .unwrap_or(0);
                for l in lines {
                    if l.trim().is_empty() {
                        writeln!(f)?;
                    } else {
                        writeln!(f, "    {}", &l[dedent.min(l.len())..].trim_end())?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for SvAlwaysFf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "    always_ff @(posedge {}) begin", self.clock)?;
        match &self.reset {
            Some(rst) => {
                writeln!(f, "        if (!{rst}) begin")?;
                for a in &self.reset_body {
                    writeln!(f, "            {} <= {};", a.lhs, a.rhs)?;
                }
                writeln!(f, "        end else begin")?;
                for a in &self.clocked_body {
                    writeln!(f, "            {} <= {};", a.lhs, a.rhs)?;
                }
                writeln!(f, "        end")?;
            }
            None => {
                for a in &self.clocked_body {
                    writeln!(f, "        {} <= {};", a.lhs, a.rhs)?;
                }
            }
        }
        writeln!(f, "    end")
    }
}

fn fmt_comb_stmt(f: &mut fmt::Formatter<'_>, stmt: &SvCombStmt, indent: usize) -> fmt::Result {
    let pad = " ".repeat(indent);
    match stmt {
        SvCombStmt::Assign { lhs, rhs } => writeln!(f, "{pad}{lhs} = {rhs};"),
        SvCombStmt::If(s) => {
            writeln!(f, "{pad}if ({}) begin", s.cond)?;
            for t in &s.then_branch {
                fmt_comb_stmt(f, t, indent + 4)?;
            }
            writeln!(f, "{pad}end else begin")?;
            for e in &s.else_branch {
                fmt_comb_stmt(f, e, indent + 4)?;
            }
            writeln!(f, "{pad}end")
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
                    SvBinOp::Sub => "-",
                    SvBinOp::Eq => "==",
                    SvBinOp::Lt => "<",
                    SvBinOp::And => "&&",
                    SvBinOp::Or => "||",
                };
                write!(f, "({l} {op} {r})")
            }
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
