//! Compile-time evaluation over **MIR**. This is the
//! twin of `hir::const_eval` — same model (demand-driven per-output thunks, the
//! shared [`Value`]/[`arith`]/[`project`] core) — but it walks the typed MIR
//! ([`MExpr`]) instead of the HIR body. It evaluates **value-position** const
//! expressions the MIR backend needs: slice endpoints, `const if` conditions,
//! and (transitively) const-fn calls reached from them.
//!
//! Why a separate walker rather than extending `hir::const_eval`: a slice
//! endpoint / const-if condition is an [`MExprId`], and the lossy `ConstArg`
//! bridge can't represent a call (a call in width
//! position stays `Deferred`). The MIR has the full expression tree, so
//! evaluating it directly is strictly more capable. The *type-level* width axis
//! (`ConstArg` in `Type`) stays on the HIR evaluator — only value-position
//! `MExpr` lives here.
//!
//! Negative space: every failure is soft (`None`) — the caller falls back to a
//! symbolic render (a parametric `#()` expression) or the `initial assert` path.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::base::db::SourceRoot;
use crate::hir::const_eval::{
    MAX_DEPTH, MAX_STEPS, Value, apply_binary, apply_unary, assoc_grounded_body, eval_const,
};
use crate::hir::sig::{Param, sig_of};
use crate::hir::types::{Direction, LocalId, Term, Type};
use crate::mir::ir::{Conn, MBlock, MExprId, MExprKind, MStmt, Mir};
use crate::mir::lower::mir_of;
use crate::nameres::def_map::CrateDefMap;
use crate::nameres::ids::DefId;

/// Evaluate a value-position MIR expression with **no** parameter bindings
/// (`ConstParam`s stay symbolic). `Some` only for a fully ground result.
pub fn eval<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    expr: MExprId,
) -> Option<Value<'db>> {
    let mut ev = Evaluator::new(db, krate);
    let frame = Frame::root(db, krate, def);
    ev.eval_expr(&frame, expr, 0)
}

/// Integer convenience over [`eval`].
pub fn eval_int<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    expr: MExprId,
) -> Option<i128> {
    match eval(db, krate, def, expr)? {
        Value::Int(v) => Some(v),
        _ => None,
    }
}

/// Const-fold a def's **return value** (its MIR block's tail / whole-result
/// equation) with no parameter bindings — the const-fn entry point.
pub fn eval_return<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Option<Value<'db>> {
    let mut ev = Evaluator::new(db, krate);
    let frame = Frame::root(db, krate, def);
    let block = frame.mir.block().clone();
    ev.eval_block(&frame, &block, 0)
}

/// Evaluate a boolean condition (a `const if` guard) with const-generic bindings
/// seeded on the root frame — the inline-splice entry point. `const_subst` is the
/// call's `call_subst ∘ self_subst` (indexed by generic param); a `ConstParam(i)`
/// resolves against it. `Some(b)` if it folds, `None` if still symbolic (the
/// generate-if case). Ordinary (no-binding) folding is `eval(...).` on a Bool.
pub fn eval_cond_with<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    expr: MExprId,
    const_subst: &[Option<Term<'db>>],
) -> Option<bool> {
    let mut ev = Evaluator::new(db, krate);
    let frame = Frame::root_with(db, krate, def, const_subst.to_vec());
    match ev.eval_expr(&frame, expr, 0) {
        Some(Value::Bool(b)) => Some(b),
        _ => None,
    }
}

struct Evaluator<'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &'db CrateDefMap<'db>,
    steps: u32,
}

impl<'db> Evaluator<'db> {
    fn new(db: &'db dyn salsa::Database, krate: SourceRoot) -> Self {
        Evaluator {
            db,
            krate,
            map: crate::nameres::def_map::crate_def_map(db, krate),
            steps: 0,
        }
    }
}

/// One activation: a def's MIR plus the call-site bindings for its value params
/// and the per-local memo slots.
struct Frame<'db> {
    def: DefId<'db>,
    mir: &'db Mir<'db>,
    bindings: HashMap<LocalId, Value<'db>>,
    /// Const-generic bindings for this activation, indexed by the def's generic
    /// param index (the same index `MExprKind::ConstParam(i)` / `ConstArg::Param(i)`
    /// use). Seeded at the **root** from a caller-supplied subst (the inline
    /// splice's composed `call_subst ∘ self_subst`); empty for an entered callee
    /// frame (a const generic reached through a *nested* call stays symbolic).
    const_subst: Vec<Option<Term<'db>>>,
    slots: RefCell<HashMap<LocalId, Slot<'db>>>,
}

#[derive(Clone)]
enum Slot<'db> {
    Evaluating,
    Done(Option<Value<'db>>),
}

impl<'db> Frame<'db> {
    fn root(db: &'db dyn salsa::Database, krate: SourceRoot, def: DefId<'db>) -> Self {
        Self::root_with(db, krate, def, Vec::new())
    }

    /// A root frame seeded with const-generic bindings (the inline splice site,
    /// which knows the call's const args; ordinary callers pass `Vec::new()`).
    fn root_with(
        db: &'db dyn salsa::Database,
        krate: SourceRoot,
        def: DefId<'db>,
        const_subst: Vec<Option<Term<'db>>>,
    ) -> Self {
        Frame {
            def,
            mir: mir_of(db, krate, def),
            bindings: HashMap::new(),
            const_subst,
            slots: Default::default(),
        }
    }
}

impl<'db> Evaluator<'db> {
    fn tick(&mut self) -> Option<()> {
        self.steps += 1;
        (self.steps <= MAX_STEPS).then_some(())
    }

    fn eval_expr(&mut self, frame: &Frame<'db>, expr: MExprId, depth: u32) -> Option<Value<'db>> {
        self.tick()?;
        match &frame.mir.expr(expr).kind {
            MExprKind::Number(n, _) => Some(Value::Int(*n)),
            MExprKind::Bool(b) => Some(Value::Bool(*b)),
            MExprKind::Local(l) => self.demand(frame, *l, depth),
            // A const generic used as a value. If this frame carries a binding for
            // it (an inline splice seeded the root with the call's const args),
            // re-enter the bound `ConstArg`; otherwise it stays a symbolic hole
            // (defers to monomorphisation), exactly like `ConstArg::Param`.
            MExprKind::ConstParam(i) => match frame.const_subst.get(*i as usize) {
                Some(Some(Term::Const(c))) => {
                    eval_const(self.db, self.krate, frame.def, c).map(Value::Int)
                }
                _ => None,
            },
            // An associated const: ground the impl const body for the (concrete)
            // self type, then evaluate that `ConstArg` via the shared core.
            MExprKind::ConstAssoc { item, self_ty } => self.eval_assoc(*item, self_ty),
            MExprKind::Field { receiver, field } => {
                let r = self.eval_expr(frame, *receiver, depth)?;
                crate::hir::const_eval::project(&r, field)
            }
            MExprKind::Record { ctor, fields } => {
                let owner = self.map.def_data((*ctor)?).and_then(|d| d.owner)?;
                let mut vals = Vec::new();
                for f in fields {
                    let Conn::In(value) = &f.conn else {
                        return None; // an out-field record isn't a const value
                    };
                    vals.push((f.name.clone(), self.eval_expr(frame, *value, depth)?));
                }
                Some(Value::Record(owner, vals))
            }
            MExprKind::If {
                cond,
                then_branch,
                else_branch,
            }
            | MExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => {
                let branch = match self.eval_expr(frame, *cond, depth)? {
                    Value::Bool(true) => then_branch,
                    Value::Bool(false) => else_branch,
                    _ => return None,
                };
                self.eval_block(frame, branch, depth)
            }
            MExprKind::Block(b) => self.eval_block(frame, b, depth),
            // A resolved call: an operator (method-form, by callee name) folds via
            // the shared arithmetic; a plain fn call enters a fresh frame.
            MExprKind::Call {
                callee,
                receiver,
                args,
                ..
            } => self.eval_call(frame, *callee, *receiver, args, depth),
            // Aggregates, indexing, builtins, slices, when, bare defs: not a
            // scalar const value here.
            _ => None,
        }
    }

    /// Operator (method-form: `a.add(b)`, `-a`) or plain const-fn call.
    fn eval_call(
        &mut self,
        frame: &Frame<'db>,
        callee: DefId<'db>,
        receiver: Option<MExprId>,
        args: &[Conn],
        depth: u32,
    ) -> Option<Value<'db>> {
        let name = self.map.def_data(callee).map(|d| d.name.as_str());
        // Method-form operator: the prelude trait methods ARE the const
        // arithmetic, folded by name on evaluated operands (shared with HIR).
        if let Some(recv) = receiver
            && let Some(n) = name
            && is_operator(n)
        {
            let a = self.eval_expr(frame, recv, depth)?;
            if args.is_empty() {
                return apply_unary(n, a);
            }
            let [Conn::In(b)] = args else { return None };
            let b = self.eval_expr(frame, *b, depth)?;
            return apply_binary(n, a, b);
        }
        // A user fn/method call: fresh frame, eval its return value.
        if depth >= MAX_DEPTH {
            return None;
        }
        let callee_frame = self.enter_call(frame, callee, receiver, args, depth)?;
        self.eval_block(&callee_frame, &callee_frame.mir.block().clone(), depth + 1)
    }

    fn eval_assoc(&mut self, item: DefId<'db>, self_ty: &Type<'db>) -> Option<Value<'db>> {
        if let Type::Value {
            kind: crate::hir::types::ValueKind::Param(_),
            ..
        } = self_ty
        {
            return None; // symbolic self type — defer
        }
        // The grounded impl-const body is a self-contained `ConstArg` — hand it to
        // the shared `ConstArg` evaluator. (Its own `def` context is the impl
        // const; a closed value needs no frame.)
        let grounded = assoc_grounded_body(self.db, self.krate, item, self_ty)?;
        eval_const(self.db, self.krate, item, &grounded).map(Value::Int)
    }

    /// The thunk: a local's value by finding its defining site — `let`, a
    /// whole-local driving equation, or a call out-connection — anywhere in the
    /// MIR block tree (locals are body-unique, so a nested hit is unambiguous).
    fn demand(&mut self, frame: &Frame<'db>, local: LocalId, depth: u32) -> Option<Value<'db>> {
        self.tick()?;
        if let Some(v) = frame.bindings.get(&local) {
            return Some(v.clone());
        }
        match frame.slots.borrow().get(&local) {
            Some(Slot::Evaluating) => return None, // const-evaluation cycle
            Some(Slot::Done(v)) => return v.clone(),
            None => {}
        }
        frame.slots.borrow_mut().insert(local, Slot::Evaluating);
        let block = frame.mir.block().clone();
        let v = self.demand_in_block(frame, &block, local, depth);
        frame
            .slots
            .borrow_mut()
            .insert(local, Slot::Done(v.clone()));
        v
    }

    fn demand_in_block(
        &mut self,
        frame: &Frame<'db>,
        block: &MBlock,
        local: LocalId,
        depth: u32,
    ) -> Option<Value<'db>> {
        for stmt in &block.stmts {
            match stmt {
                MStmt::Let { local: l, value } if *l == local => {
                    return self.eval_expr(frame, *value, depth);
                }
                // A whole-local driving equation (`l = …;`) — the MIR LHS is a
                // bare `Place` (no projections).
                MStmt::Equation { lhs, rhs } if lhs.base == local && lhs.projections.is_empty() => {
                    return self.eval_expr(frame, *rhs, depth);
                }
                // A statement call driving `local` via an out-connection.
                MStmt::Expr(e) => {
                    if let Some(v) = self.try_out_call(frame, *e, local, depth) {
                        return v;
                    }
                }
                _ => {}
            }
        }
        // Recurse into nested expression blocks (if/const-if/block bodies).
        for stmt in &block.stmts {
            let nested = match stmt {
                MStmt::Let { value, .. } => Some(*value),
                MStmt::Expr(e) => Some(*e),
                MStmt::Equation { rhs, .. } => Some(*rhs),
                MStmt::Return { value } => Some(*value),
                // Loop-varying / clocked / decls are not const.
                MStmt::VarDecl { .. } | MStmt::For { .. } | MStmt::When { .. } => None,
            };
            if let Some(e) = nested
                && let Some(v) = self.demand_in_expr_blocks(frame, e, local, depth)
            {
                return Some(v);
            }
        }
        None
    }

    fn demand_in_expr_blocks(
        &mut self,
        frame: &Frame<'db>,
        expr: MExprId,
        local: LocalId,
        depth: u32,
    ) -> Option<Value<'db>> {
        match &frame.mir.expr(expr).kind {
            MExprKind::If {
                then_branch,
                else_branch,
                ..
            }
            | MExprKind::ConstIf {
                then_branch,
                else_branch,
                ..
            } => self
                .demand_in_block(frame, then_branch, local, depth)
                .or_else(|| self.demand_in_block(frame, else_branch, local, depth)),
            MExprKind::Block(b) => self.demand_in_block(frame, b, local, depth),
            _ => None,
        }
    }

    /// If `expr` is a call with an out-connection targeting `local`, evaluate the
    /// callee's corresponding out param (its own thunk). `Some(result)` if this
    /// call drives `local`, `None` if it doesn't.
    #[allow(clippy::option_option)]
    fn try_out_call(
        &mut self,
        frame: &Frame<'db>,
        expr: MExprId,
        local: LocalId,
        depth: u32,
    ) -> Option<Option<Value<'db>>> {
        let MExprKind::Call {
            callee,
            receiver,
            args,
            ..
        } = &frame.mir.expr(expr).kind
        else {
            return None;
        };
        // The out-connection's position among the positional args.
        let pos = args.iter().position(|c| match c {
            Conn::Out(p) => p.base == local && p.projections.is_empty(),
            Conn::In(_) => false,
        })?;
        Some(self.eval_out_param(frame, *callee, *receiver, args, pos, depth))
    }

    fn eval_out_param(
        &mut self,
        caller: &Frame<'db>,
        def: DefId<'db>,
        receiver: Option<MExprId>,
        args: &[Conn],
        pos: usize,
        depth: u32,
    ) -> Option<Value<'db>> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let callee = self.enter_call(caller, def, receiver, args, depth)?;
        let sig = sig_of(self.db, self.krate, def);
        let positional: Vec<&Param<'db>> = sig
            .params
            .iter()
            .filter(|p| !p.from_named_section)
            .collect();
        let param = positional.get(pos)?;
        if param.direction != Some(Direction::Out) {
            return None;
        }
        self.demand(&callee, param.local, depth + 1)
    }

    /// Build the callee frame: in-args evaluated eagerly in the caller frame
    /// (cheap and pure), out params left to demand. A method receiver binds the
    /// callee's leading `self` param.
    fn enter_call(
        &mut self,
        caller: &Frame<'db>,
        def: DefId<'db>,
        receiver: Option<MExprId>,
        args: &[Conn],
        depth: u32,
    ) -> Option<Frame<'db>> {
        let sig = sig_of(self.db, self.krate, def);
        let positional: Vec<&Param<'db>> = sig
            .params
            .iter()
            .filter(|p| !p.from_named_section)
            .collect();
        let mut bindings = HashMap::new();
        let mut next = 0;
        // A receiver fills the first positional param (`self`).
        if let Some(recv) = receiver {
            let p = positional.first()?;
            bindings.insert(p.local, self.eval_expr(caller, recv, depth)?);
            next = 1;
        }
        for c in args {
            match c {
                Conn::In(e) => {
                    let p = positional.get(next)?;
                    next += 1;
                    bindings.insert(p.local, self.eval_expr(caller, *e, depth)?);
                }
                // Out-args are demanded by the caller, not bound here; they still
                // consume a positional slot.
                Conn::Out(_) => {
                    next += 1;
                }
            }
        }
        Some(Frame {
            def,
            mir: mir_of(self.db, self.krate, def),
            bindings,
            // A nested callee's const generics are not bound here (deferred — the
            // splice only seeds the root frame); they stay symbolic.
            const_subst: Vec::new(),
            slots: Default::default(),
        })
    }

    /// A block's value: its tail, or a top-level `return` / whole-result
    /// equation. Lets inside the block are reached by demand.
    fn eval_block(&mut self, frame: &Frame<'db>, block: &MBlock, depth: u32) -> Option<Value<'db>> {
        if let Some(tail) = block.tail {
            return self.eval_expr(frame, tail, depth);
        }
        for stmt in &block.stmts {
            match stmt {
                MStmt::Return { value } => return self.eval_expr(frame, *value, depth),
                MStmt::Equation { lhs, rhs }
                    if lhs.projections.is_empty()
                        && frame.mir.local(lhs.base).result_base.is_some() =>
                {
                    return self.eval_expr(frame, *rhs, depth);
                }
                _ => {}
            }
        }
        None
    }
}

/// Is a def name one of the prelude operator methods the evaluator folds?
fn is_operator(name: &str) -> bool {
    matches!(
        name,
        "add"
            | "sub"
            | "mul"
            | "div"
            | "rem"
            | "neg"
            | "not"
            | "eq"
            | "ne"
            | "lt"
            | "le"
            | "gt"
            | "ge"
            | "and"
            | "or"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;
    use crate::nameres::def_map::crate_def_map;
    use crate::nameres::ids::Namespace;

    /// Const-fold the return value of fn `name` in `src`.
    fn fold(src: &str, name: &str) -> Option<i128> {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src.to_owned());
        let krate = vfs.source_root(&mut db, "t.mrn");
        let map = crate_def_map(&db, krate);
        let def = map
            .resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def");
        match eval_return(&db, krate, def)? {
            Value::Int(v) => Some(v),
            _ => None,
        }
    }

    #[test]
    fn arithmetic_and_locals() {
        assert_eq!(fold("fn f() -> integer { 2 + 3 * 4 }", "f"), Some(14));
        assert_eq!(
            fold("fn f() -> integer { let w = 4; w * 2 - 1 }", "f"),
            Some(7)
        );
    }

    #[test]
    fn const_fn_call_folds() {
        // A user const-fn call in value position folds through a fresh frame.
        let src = "fn g(x: integer) -> integer { x + 10 }\n\
                   fn f() -> integer { g(5) * 2 }\n";
        assert_eq!(fold(src, "f"), Some(30));
    }

    #[test]
    fn const_if_picks_branch() {
        let src = "fn f() -> integer { const if 3 > 2 { 7 } else { 9 } }\n";
        assert_eq!(fold(src, "f"), Some(7));
    }

    #[test]
    fn symbolic_param_does_not_fold() {
        // A free const generic stays symbolic (None), not a wrong value.
        assert_eq!(
            fold("fn f {const n: integer} () -> integer { n + 1 }", "f"),
            None
        );
    }

    #[test]
    fn out_param_thunk_folds() {
        // `widths(3, => a, => b)` — each out param is its own thunk; the demanded
        // local resolves through the call's out-connection.
        let src = "fn widths(n: integer, out a: integer, out b: integer) { a = n + 1; b = 2 * n; }\n\
                   fn f() -> integer { var a: integer; var b: integer; \
                   widths(3, => a, => b); a + b }\n";
        assert_eq!(fold(src, "f"), Some(10)); // (3+1) + (2*3) = 4 + 6
    }
}
