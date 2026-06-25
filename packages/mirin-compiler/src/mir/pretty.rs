//! A textual dump of MIR — the first real consumer (`--emit mir`).
//!
//! Not a parity gate (that's `golden_sv_snapshot`); a human-readable eyeball aid
//! that surfaces what HIR→MIR lowering produced: types-on-node, the unified
//! `Call`, places. Rendering is best-effort and `db`-aware so callee/port names
//! resolve; types are rendered structurally (widths/domains shown, generic args
//! elided).

use std::fmt::Write;

use crate::base::db::SourceRoot;
use crate::hir::types::{ConstArg, ConstOp, Domain, LocalId, Term, Type, ValueKind};
use crate::mir::ir::*;
use crate::nameres::def_map::{CrateDefMap, crate_def_map};
use crate::nameres::ids::DefId;

/// Render a def's MIR as indented text.
pub fn pretty<'db>(db: &'db dyn salsa::Database, krate: SourceRoot, mir: &Mir<'db>) -> String {
    let p = Printer {
        map: crate_def_map(db, krate),
        mir,
    };
    p.run()
}

struct Printer<'a, 'db> {
    map: &'a CrateDefMap<'db>,
    mir: &'a Mir<'db>,
}

impl<'a, 'db> Printer<'a, 'db> {
    fn run(&self) -> String {
        let mut out = String::new();
        if let Some(_v) = self.mir.verilog() {
            out.push_str("= verilog { … }  (inline-verilog body)\n");
        }
        out.push_str("locals:\n");
        for (i, l) in self.mir.locals().iter().enumerate() {
            let tag = if (i as u32) < self.mir.param_count() {
                "param"
            } else {
                "local"
            };
            let _ = writeln!(
                out,
                "  l{i} [{tag} {:?}] {}: {}",
                l.kind,
                l.name,
                self.ty(&l.ty)
            );
        }
        out.push_str("body:\n");
        self.block(self.mir.block(), 1, &mut out);
        out
    }

    fn block(&self, b: &MBlock, depth: usize, out: &mut String) {
        for s in &b.stmts {
            self.stmt(s, depth, out);
        }
        if let Some(t) = b.tail {
            let _ = writeln!(out, "{}tail {}", pad(depth), self.expr(t));
        }
    }

    fn stmt(&self, s: &MStmt, depth: usize, out: &mut String) {
        let ind = pad(depth);
        match s {
            MStmt::Let { local, value } => {
                let _ = writeln!(out, "{ind}let l{} = {}", local.0, self.expr(*value));
            }
            MStmt::VarDecl { local } => {
                let _ = writeln!(out, "{ind}var l{}", local.0);
            }
            MStmt::Equation { lhs, rhs } => {
                let _ = writeln!(out, "{ind}{} = {}", self.place(lhs), self.expr(*rhs));
            }
            MStmt::Return { value } => {
                let _ = writeln!(out, "{ind}return {}", self.expr(*value));
            }
            MStmt::Expr(e) => {
                let _ = writeln!(out, "{ind}{}", self.expr(*e));
            }
            MStmt::When { event, body, init } => {
                let _ = writeln!(out, "{ind}when {} {{", self.expr(*event));
                self.block(body, depth + 1, out);
                if let Some(init) = init {
                    let _ = writeln!(out, "{ind}}} init {{");
                    self.block(init, depth + 1, out);
                }
                let _ = writeln!(out, "{ind}}}");
            }
            MStmt::For {
                index,
                elem,
                iter,
                body,
            } => {
                let idx = index.map(|i| format!("l{}, ", i.0)).unwrap_or_default();
                let _ = writeln!(
                    out,
                    "{ind}for ({idx}l{}) in {} {{",
                    elem.0,
                    self.expr(*iter)
                );
                self.block(body, depth + 1, out);
                let _ = writeln!(out, "{ind}}}");
            }
        }
    }

    fn place(&self, p: &Place) -> String {
        let mut s = format!("l{}", p.base.0);
        for proj in &p.projections {
            match proj {
                Projection::Field(f) => {
                    let _ = write!(s, ".{f}");
                }
                Projection::Index(i) => {
                    let _ = write!(s, "[{}]", self.expr(*i));
                }
            }
        }
        s
    }

    /// Render an expression as `kind:type`.
    fn expr(&self, id: MExprId) -> String {
        let e = self.mir.expr(id);
        format!("{}:{}", self.kind(&e.kind), self.ty(&e.ty))
    }

    fn kind(&self, k: &MExprKind<'db>) -> String {
        match k {
            MExprKind::Missing => "<missing>".to_owned(),
            MExprKind::Number(v, _) => v.to_string(),
            MExprKind::Bool(b) => b.to_string(),
            MExprKind::Local(l) => format!("l{}", l.0),
            MExprKind::ConstParam(i) => format!("N{i}"),
            MExprKind::ConstAssoc { item, .. } => format!("{}::<const>", self.def_name(*item)),
            MExprKind::Def(d) => self.def_name(*d),
            MExprKind::VecLit(es) => format!("[{}]", self.exprs(es)),
            MExprKind::TupleLit(es) => format!("({})", self.exprs(es)),
            MExprKind::VecRepeat { elem, len } => {
                format!("[{}; {}]", self.expr(*elem), self.cst(len))
            }
            MExprKind::Index { base, index } => {
                format!("{}[{}]", self.expr(*base), self.expr(*index))
            }
            MExprKind::Field { receiver, field } => format!("{}.{field}", self.expr(*receiver)),
            MExprKind::Call {
                callee,
                substs,
                receiver,
                args,
                named,
            } => {
                let recv = receiver
                    .map(|r| format!("{}.", self.expr(r)))
                    .unwrap_or_default();
                let subst = if substs.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", self.terms(substs))
                };
                format!(
                    "{recv}call {}{subst}({}{})",
                    self.def_name(*callee),
                    self.args(args),
                    self.named(named),
                )
            }
            MExprKind::Builtin {
                method,
                receiver,
                args,
            } => format!("{}.{:?}({})", self.expr(*receiver), method, self.args(args)),
            MExprKind::Record { ctor, fields } => {
                let name = ctor.map(|c| self.def_name(c)).unwrap_or("?".to_owned());
                let fs: Vec<String> = fields
                    .iter()
                    .map(|f| self.named_conn(&f.name, &f.conn))
                    .collect();
                format!("{name} {{ {} }}", fs.join(", "))
            }
            MExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => format!(
                "if {} {{ {} }} else {{ {} }}",
                self.expr(*cond),
                self.inline_block(then_branch),
                self.inline_block(else_branch),
            ),
            MExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => format!(
                "const if {} {{ {} }} else {{ {} }}",
                self.expr(*cond),
                self.inline_block(then_branch),
                self.inline_block(else_branch),
            ),
            MExprKind::Slice {
                base,
                lo,
                hi,
                width,
            } => {
                let o = |e: &Option<MExprId>| e.map(|e| self.expr(e)).unwrap_or_default();
                let tail = match (hi, width) {
                    (Some(_), _) => format!("..{}", o(hi)),
                    (_, Some(_)) => format!("..+{}", o(width)),
                    _ => "..".to_owned(),
                };
                format!("{}[{}{tail}]", self.expr(*base), o(lo))
            }
            MExprKind::When { event, body, init } => {
                let init = init
                    .map(|i| format!(" init {}", self.expr(i)))
                    .unwrap_or_default();
                format!(
                    "when {} {{ {} }}{init}",
                    self.expr(*event),
                    self.inline_block(body)
                )
            }
            MExprKind::Block(b) => format!("{{ {} }}", self.inline_block(b)),
        }
    }

    /// A block rendered on one line (for expression-position blocks).
    fn inline_block(&self, b: &MBlock) -> String {
        let mut parts: Vec<String> = Vec::new();
        for s in &b.stmts {
            let mut tmp = String::new();
            self.stmt(s, 0, &mut tmp);
            parts.push(tmp.trim_end().to_owned());
        }
        if let Some(t) = b.tail {
            parts.push(self.expr(t));
        }
        parts.join("; ")
    }

    fn exprs(&self, es: &[MExprId]) -> String {
        es.iter()
            .map(|e| self.expr(*e))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn conn(&self, c: &Conn) -> String {
        match c {
            Conn::In(e) => self.expr(*e),
            Conn::Out(p) => format!("=> {}", self.place(p)),
        }
    }

    /// `name = value` for an in-connection, `name => place` for an out one.
    fn named_conn(&self, name: &str, c: &Conn) -> String {
        match c {
            Conn::In(e) => format!("{name} = {}", self.expr(*e)),
            Conn::Out(p) => format!("{name} => {}", self.place(p)),
        }
    }

    fn args(&self, args: &[Conn]) -> String {
        args.iter()
            .map(|c| self.conn(c))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn named(&self, named: &[MNamedArg]) -> String {
        if named.is_empty() {
            return String::new();
        }
        let body = named
            .iter()
            .map(|n| self.named_conn(&n.name, &n.conn))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" {{{body}}}")
    }

    fn terms(&self, ts: &[Term<'db>]) -> String {
        ts.iter()
            .map(|t| match t {
                Term::Type(t) => self.ty(t),
                Term::Const(c) => self.cst(c),
                Term::Domain(d) => dom(d),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn def_name(&self, d: DefId<'db>) -> String {
        self.map
            .def_data(d)
            .map(|dd| dd.name.clone())
            .unwrap_or_else(|| "<def>".to_owned())
    }

    fn ty(&self, t: &Type<'db>) -> String {
        match t {
            Type::Value { kind, domain } => format!("{}@{}", self.vkind(kind), dom(domain)),
            Type::Vec { len, elem } => format!("Vec({}, {})", self.cst(len), self.ty(elem)),
            Type::Tuple(ts) => format!(
                "({})",
                ts.iter().map(|t| self.ty(t)).collect::<Vec<_>>().join(", ")
            ),
            Type::Port { def, domain, .. } => format!("{}@{}", self.def_name(*def), dom(domain)),
            Type::Clock => "Clock".to_owned(),
            Type::Infer(v) => format!("?t{}", v.0),
            Type::Error => "!err".to_owned(),
        }
    }

    fn vkind(&self, k: &ValueKind<'db>) -> String {
        match k {
            ValueKind::UInt { width } => format!("uint({})", self.cst(width)),
            ValueKind::SInt { width } => format!("sint({})", self.cst(width)),
            ValueKind::Bits { width } => format!("bits({})", self.cst(width)),
            ValueKind::Bool => "bool".to_owned(),
            ValueKind::Reset => "reset".to_owned(),
            ValueKind::Event => "event".to_owned(),
            ValueKind::Integer => "int".to_owned(),
            ValueKind::Param(i) => format!("T{i}"),
        }
    }

    fn cst(&self, c: &ConstArg<'db>) -> String {
        match c {
            ConstArg::Lit(n) => n.to_string(),
            ConstArg::Param(i) => format!("N{i}"),
            ConstArg::Local(l) => format!("l{}", l.0),
            ConstArg::Infer(v) => format!("?c{}", v.0),
            ConstArg::Op(op, a, b) => format!("({} {} {})", self.cst(a), op_str(*op), self.cst(b)),
            ConstArg::Field(b, f) => format!("{}.{f}", self.cst(b)),
            ConstArg::Assoc { item, .. } => format!("{}::<const>", self.def_name(*item)),
            ConstArg::Deferred => "<deferred>".to_owned(),
            ConstArg::Symbol(s) => s.clone(),
        }
    }
}

fn dom(d: &Domain) -> String {
    match d {
        Domain::Unspecified => "?".to_owned(),
        Domain::Const => "const".to_owned(),
        Domain::Param(i) => format!("D{i}"),
        Domain::Clock(LocalId(l)) => format!("l{l}"),
        Domain::Infer(v) => format!("?d{}", v.0),
    }
}

fn op_str(op: ConstOp) -> &'static str {
    match op {
        ConstOp::Add => "+",
        ConstOp::Sub => "-",
        ConstOp::Mul => "*",
        ConstOp::Div => "/",
        ConstOp::Rem => "%",
    }
}

fn pad(depth: usize) -> String {
    "  ".repeat(depth)
}
