//! Compile-time evaluation of `@const` values (`planning/const_eval.md`).
//!
//! The rustc analogue is CTFE (`const_eval_*` interpreting MIR on demand),
//! with one structural divergence: a Mirin fn body is an **equation system**,
//! not a statement list, so there is no sequential stepping. Evaluation is
//! **demand-driven per local**: demanding a local finds its `let`, its
//! driving equation, or the call out-connection (`f(x, => l)`) that drives
//! it, and evaluates that expression — memoized per frame, with an
//! in-progress marker for cycle detection. A callee's `out` params are
//! therefore *thunks*: only the outputs the caller actually uses are
//! evaluated.
//!
//! Failure is always soft (`None`): a symbolic width (free generic param),
//! a non-const construct (`when`, `.reg`), a cycle, or a blown budget all
//! leave the const symbolic, and the caller falls back to residual handling.
//!
//! Note (future, `planning/const_eval.md`): when const asserts land, an
//! entered frame must demand its assert-bearing statements even when no
//! output needs them — laziness must not skip a failing assert.

use std::collections::HashMap;

use crate::base::db::SourceRoot;
use crate::hir::body::{Block, Body, ConnArg, ExprId, ExprKind, LocalKind, Stmt, body};
use crate::hir::sig::{Param, sig_of};
use crate::hir::types::{
    ConstArg, ConstOp, Direction, LocalId, Type, ValueKind, match_header, subst_const_opt,
    type_has_infer,
};
use crate::nameres::def_map::{CrateDefMap, crate_def_map};
use crate::nameres::ids::DefId;

/// A const value: the `integer` scalar, a bool, or a structural record
/// (rustc's valtree analogue, names kept for field projection).
#[derive(Clone, PartialEq, Eq)]
pub enum Value<'db> {
    Int(i128),
    Bool(bool),
    Record(DefId<'db>, Vec<(String, Value<'db>)>),
}

const MAX_DEPTH: u32 = 32;
const MAX_STEPS: u32 = 10_000;

/// Evaluate a const-expression tree in `def`'s body with **no** parameter
/// bindings (params stay symbolic — a tree over a free `Param` returns
/// `None`). The entry point for `infer` and the backend.
pub fn eval_const<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    c: &ConstArg<'db>,
) -> Option<i128> {
    match eval_width(db, krate, def, c) {
        WidthEval::Value(v) => Some(v),
        WidthEval::Symbolic | WidthEval::Failed => None,
    }
}

/// The outcome of const-evaluating a width tree, distinguishing a clean
/// integer from the two reasons it can fail: still **symbolic** (a generic
/// `Param`, an inference var, or a deferred call — resolved later, at
/// monomorphisation) versus genuinely **failed** (a *closed* expression that
/// still has no value — divide-by-zero, overflow). The first defers; the
/// second is a hard error (`check_widths` in `infer`).
pub enum WidthEval {
    Value(i128),
    Symbolic,
    Failed,
}

pub fn eval_width<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    c: &ConstArg<'db>,
) -> WidthEval {
    let mut ev = Evaluator {
        db,
        krate,
        map: crate_def_map(db, krate),
        steps: 0,
        symbolic: false,
    };
    let frame = Frame::root(db, krate, def);
    match ev.eval_const_arg(&frame, c, 0) {
        Some(Value::Int(v)) => WidthEval::Value(v),
        // No value: a symbolic leaf anywhere (even through a `Local`'s
        // definition) means defer; otherwise the closed expression truly has
        // no value.
        _ if ev.symbolic => WidthEval::Symbolic,
        _ => WidthEval::Failed,
    }
}

struct Evaluator<'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &'db CrateDefMap<'db>,
    steps: u32,
    /// Set when evaluation gets stuck on a symbolic leaf (`Param`/`Infer`/
    /// `Deferred`/`Error`, or a symbolic associated-const self type). Lets
    /// [`eval_width`] tell "defer" from "genuinely failed".
    symbolic: bool,
}

/// One activation: a def's body plus the call-site bindings for its value
/// params and the per-local memo slots.
struct Frame<'db> {
    body: &'db Body<'db>,
    bindings: HashMap<LocalId, Value<'db>>,
    slots: std::cell::RefCell<HashMap<LocalId, Slot<'db>>>,
}

#[derive(Clone)]
enum Slot<'db> {
    Evaluating,
    Done(Option<Value<'db>>),
}

impl<'db> Frame<'db> {
    fn root(db: &'db dyn salsa::Database, krate: SourceRoot, def: DefId<'db>) -> Self {
        Frame {
            body: body(db, krate, def),
            bindings: HashMap::new(),
            slots: Default::default(),
        }
    }
}

impl<'db> Evaluator<'db> {
    fn tick(&mut self) -> Option<()> {
        self.steps += 1;
        (self.steps <= MAX_STEPS).then_some(())
    }

    fn eval_const_arg(
        &mut self,
        frame: &Frame<'db>,
        c: &ConstArg<'db>,
        depth: u32,
    ) -> Option<Value<'db>> {
        self.tick()?;
        match c {
            ConstArg::Lit(v) => Some(Value::Int(*v)),
            ConstArg::Local(l) => self.demand(frame, *l, depth),
            ConstArg::Op(op, a, b) => {
                let a = self.eval_const_arg(frame, a, depth)?;
                let b = self.eval_const_arg(frame, b, depth)?;
                arith(*op, &a, &b)
            }
            ConstArg::Field(base, name) => {
                let base = self.eval_const_arg(frame, base, depth)?;
                project(&base, name)
            }
            // An associated const: resolve the impl for the (concrete) self
            // type, substitute the header binding into the impl's value, and
            // recurse (rustc: Instance::resolve + CTFE on the impl's const).
            ConstArg::Assoc { item, self_ty } => self.eval_assoc(frame, *item, self_ty, depth),
            // Params stay symbolic at the root; Infer/Deferred/Error are
            // not evaluable — all "stuck on symbolic", not "failed".
            _ => {
                self.symbolic = true;
                None
            }
        }
    }

    fn eval_assoc(
        &mut self,
        frame: &Frame<'db>,
        item: DefId<'db>,
        self_ty: &Type<'db>,
        depth: u32,
    ) -> Option<Value<'db>> {
        if type_has_infer(self_ty)
            || matches!(
                self_ty,
                Type::Value {
                    kind: ValueKind::Param(_),
                    ..
                }
            )
        {
            self.symbolic = true;
            return None; // still symbolic
        }
        let owner = self.map.def_data(item)?.owner?;
        let (binding, value) = match self.map.def_data(owner)?.kind {
            // A trait's const DECL: select the impl by header match.
            crate::nameres::ids::DefKind::Trait => {
                let name = self.map.def_data(item)?.name.clone();
                let mut found = None;
                for data in self.map.trait_impls(owner) {
                    let hsig = sig_of(self.db, self.krate, data.impl_def);
                    let Some(header) = &hsig.return_type else {
                        continue;
                    };
                    let mut binding = vec![None; hsig.generic_params.len()];
                    if match_header(self_ty, header, &mut binding) {
                        let cdef = data.consts.iter().find(|(n, _)| *n == name)?.1;
                        found = Some((
                            binding,
                            sig_of(self.db, self.krate, cdef).const_value.clone()?,
                        ));
                        break;
                    }
                }
                found?
            }
            // Already an impl's const: bind its prefix from the self type.
            _ => {
                let value = sig_of(self.db, self.krate, item).const_value.clone()?;
                // Re-derive the binding by matching the impl header. The
                // impl def is found through the owner's trait impls.
                let mut found = None;
                'outer: for (_, impls) in self.map.all_trait_impls() {
                    for data in impls {
                        if data.consts.iter().any(|(_, d)| *d == item) {
                            let hsig = sig_of(self.db, self.krate, data.impl_def);
                            let Some(header) = &hsig.return_type else {
                                continue;
                            };
                            let mut binding = vec![None; hsig.generic_params.len()];
                            if match_header(self_ty, header, &mut binding) {
                                found = Some(binding);
                            }
                            break 'outer;
                        }
                    }
                }
                (found?, value)
            }
        };
        let grounded = subst_const_opt(&value, &binding);
        self.eval_const_arg(frame, &grounded, depth + 1)
    }

    /// The thunk: a local's value by finding its defining site — `let`,
    /// whole-local driving equation, or a call out-connection — anywhere in
    /// the body's statement tree (locals are body-unique, so a nested block
    /// hit is unambiguous).
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
        let v = self.demand_uncached(frame, local, depth);
        // A `Param` the body never drives (and the caller never bound) is an
        // unbound input — symbolic, deferred to instantiation, not a failed
        // eval. An OUT param is a `Param` too but IS driven, so the search
        // above returns `Some` and we don't reach here.
        if v.is_none() && frame.body.local(local).kind == LocalKind::Param {
            self.symbolic = true;
        }
        frame
            .slots
            .borrow_mut()
            .insert(local, Slot::Done(v.clone()));
        v
    }

    fn demand_uncached(
        &mut self,
        frame: &Frame<'db>,
        local: LocalId,
        depth: u32,
    ) -> Option<Value<'db>> {
        let block = frame.body.block().clone();
        self.demand_in_block(frame, &block, local, depth)
    }

    fn demand_in_block(
        &mut self,
        frame: &Frame<'db>,
        block: &Block,
        local: LocalId,
        depth: u32,
    ) -> Option<Value<'db>> {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { local: l, value } if *l == local => {
                    return self.eval_expr(frame, *value, depth);
                }
                Stmt::Equation { lhs, rhs } => {
                    if let ExprKind::Local(l) = frame.body.expr(*lhs).kind
                        && l == local
                    {
                        return self.eval_expr(frame, *rhs, depth);
                    }
                }
                // A statement-position call driving `local` through an
                // out-connection: `f(x, => local);`.
                Stmt::Expr(e) => {
                    if let Some(v) = self.try_out_call(frame, *e, local, depth) {
                        return v;
                    }
                }
                _ => {}
            }
        }
        // Recurse into nested blocks (if/when/block statements and lets).
        for stmt in &block.stmts {
            let nested = match stmt {
                Stmt::Let { value, .. } => Some(*value),
                Stmt::Expr(e) => Some(*e),
                Stmt::Equation { rhs, .. } => Some(*rhs),
                Stmt::Return { value } => Some(*value),
                Stmt::VarDecl { .. } => None,
                // Loop-varying bindings are not const.
                Stmt::For { .. } => None,
                // Clocked state is never a const value.
                Stmt::When { .. } => None,
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
        expr: ExprId,
        local: LocalId,
        depth: u32,
    ) -> Option<Value<'db>> {
        match &frame.body.expr(expr).kind {
            ExprKind::If {
                then_branch,
                else_branch,
                ..
            } => self
                .demand_in_block(frame, then_branch, local, depth)
                .or_else(|| self.demand_in_block(frame, else_branch, local, depth)),
            ExprKind::Block(b) => self.demand_in_block(frame, b, local, depth),
            _ => None,
        }
    }

    /// If `expr` is a call with an out-connection targeting `local`, evaluate
    /// the callee's corresponding out param (its own thunk). Returns
    /// `Some(result)` if this call drives `local`, `None` if it doesn't.
    #[allow(clippy::option_option)]
    fn try_out_call(
        &mut self,
        frame: &Frame<'db>,
        expr: ExprId,
        local: LocalId,
        depth: u32,
    ) -> Option<Option<Value<'db>>> {
        let ExprKind::Call { callee, args, .. } = &frame.body.expr(expr).kind else {
            return None;
        };
        let pos = args.iter().position(|a| {
            a.out && matches!(frame.body.expr(a.expr).kind, ExprKind::Local(l) if l == local)
        })?;
        let ExprKind::Def(def) = frame.body.expr(*callee).kind else {
            return Some(None);
        };
        Some(self.eval_out_param(frame, def, args, pos, depth))
    }

    fn eval_out_param(
        &mut self,
        caller: &Frame<'db>,
        def: DefId<'db>,
        args: &[ConnArg],
        pos: usize,
        depth: u32,
    ) -> Option<Value<'db>> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let callee = self.enter_call(caller, def, args, depth)?;
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
    /// (cheap and pure), out params left to demand.
    fn enter_call(
        &mut self,
        caller: &Frame<'db>,
        def: DefId<'db>,
        args: &[ConnArg],
        depth: u32,
    ) -> Option<Frame<'db>> {
        let sig = sig_of(self.db, self.krate, def);
        let positional: Vec<&Param<'db>> = sig
            .params
            .iter()
            .filter(|p| !p.from_named_section)
            .collect();
        let mut bindings = HashMap::new();
        for (i, a) in args.iter().enumerate() {
            if a.out {
                continue;
            }
            let p = positional.get(i)?;
            let v = self.eval_expr(caller, a.expr, depth)?;
            bindings.insert(p.local, v);
        }
        Some(Frame {
            body: body(self.db, self.krate, def),
            bindings,
            slots: Default::default(),
        })
    }

    fn eval_expr(&mut self, frame: &Frame<'db>, expr: ExprId, depth: u32) -> Option<Value<'db>> {
        self.tick()?;
        match &frame.body.expr(expr).kind {
            ExprKind::Number(n, _) => Some(Value::Int(*n)),
            ExprKind::TypedLiteral { value, .. } => Some(Value::Int(*value)),
            ExprKind::Bool(b) => Some(Value::Bool(*b)),
            ExprKind::Local(l) => self.demand(frame, *l, depth),
            ExprKind::Field { receiver, field } => {
                let r = self.eval_expr(frame, *receiver, depth)?;
                project(&r, field)
            }
            ExprKind::Record { ctor, fields } => {
                let owner = self.map.def_data((*ctor)?).and_then(|d| d.owner)?;
                let mut vals = Vec::new();
                for f in fields {
                    if f.out {
                        return None;
                    }
                    let v = self.eval_expr(frame, f.value, depth)?;
                    vals.push((f.name.clone(), v));
                }
                Some(Value::Record(owner, vals))
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let c = self.eval_expr(frame, *cond, depth)?;
                let branch = match c {
                    Value::Bool(true) => then_branch,
                    Value::Bool(false) => else_branch,
                    _ => return None,
                };
                self.eval_block(frame, branch, depth)
            }
            ExprKind::Block(b) => self.eval_block(frame, b, depth),
            // Operator desugar (`a + b` → `a.add(b)`): the prelude trait
            // methods ARE the const arithmetic — match by method name on
            // evaluated operands (no inference data needed down here).
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                if args.is_empty() {
                    // Unary: `-x` → `x.neg()`.
                    let a = self.eval_expr(frame, *receiver, depth)?;
                    return match (method.as_str(), a) {
                        ("neg", Value::Int(v)) => Some(Value::Int(-v)),
                        ("not", Value::Bool(v)) => Some(Value::Bool(!v)),
                        _ => None,
                    };
                }
                let [b] = args.as_slice() else { return None };
                if b.out {
                    return None;
                }
                let a = self.eval_expr(frame, *receiver, depth)?;
                let b = self.eval_expr(frame, b.expr, depth)?;
                match method.as_str() {
                    "add" => arith(ConstOp::Add, &a, &b),
                    "sub" => arith(ConstOp::Sub, &a, &b),
                    "mul" => arith(ConstOp::Mul, &a, &b),
                    "div" => arith(ConstOp::Div, &a, &b),
                    "rem" => arith(ConstOp::Rem, &a, &b),
                    "eq" => match (a, b) {
                        (Value::Int(x), Value::Int(y)) => Some(Value::Bool(x == y)),
                        (Value::Bool(x), Value::Bool(y)) => Some(Value::Bool(x == y)),
                        _ => None,
                    },
                    "ne" => match (a, b) {
                        (Value::Int(x), Value::Int(y)) => Some(Value::Bool(x != y)),
                        (Value::Bool(x), Value::Bool(y)) => Some(Value::Bool(x != y)),
                        _ => None,
                    },
                    "lt" => match (a, b) {
                        (Value::Int(x), Value::Int(y)) => Some(Value::Bool(x < y)),
                        _ => None,
                    },
                    "le" => match (a, b) {
                        (Value::Int(x), Value::Int(y)) => Some(Value::Bool(x <= y)),
                        _ => None,
                    },
                    "gt" => match (a, b) {
                        (Value::Int(x), Value::Int(y)) => Some(Value::Bool(x > y)),
                        _ => None,
                    },
                    "ge" => match (a, b) {
                        (Value::Int(x), Value::Int(y)) => Some(Value::Bool(x >= y)),
                        _ => None,
                    },
                    "and" => match (a, b) {
                        (Value::Bool(x), Value::Bool(y)) => Some(Value::Bool(x && y)),
                        _ => None,
                    },
                    "or" => match (a, b) {
                        (Value::Bool(x), Value::Bool(y)) => Some(Value::Bool(x || y)),
                        _ => None,
                    },
                    _ => None,
                }
            }
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Def(def) = frame.body.expr(*callee).kind else {
                    return None;
                };
                if depth >= MAX_DEPTH {
                    return None;
                }
                let callee_frame = self.enter_call(frame, def, args, depth)?;
                self.eval_return(&callee_frame, depth + 1)
            }
            // when / .reg / Missing / bare defs: not const.
            _ => None,
        }
    }

    /// A callee's return value: the body tail, or its single `return`.
    fn eval_return(&mut self, frame: &Frame<'db>, depth: u32) -> Option<Value<'db>> {
        let block = frame.body.block().clone();
        self.eval_block(frame, &block, depth)
    }

    /// A block's value: its tail expression, or the value of a top-level
    /// `return` (a unit fn's bare `Stmt::Return`, or — when the fn has a return
    /// type — the desugared whole-result equation `return = EXPR`). Lets inside
    /// the block are reached by demand.
    fn eval_block(&mut self, frame: &Frame<'db>, block: &Block, depth: u32) -> Option<Value<'db>> {
        if let Some(tail) = block.tail {
            return self.eval_expr(frame, tail, depth);
        }
        for stmt in &block.stmts {
            match stmt {
                Stmt::Return { value } => return self.eval_expr(frame, *value, depth),
                Stmt::Equation { lhs, rhs }
                    if matches!(&frame.body.expr(*lhs).kind,
                        ExprKind::Local(l) if frame.body.local(*l).result_base.is_some()) =>
                {
                    return self.eval_expr(frame, *rhs, depth);
                }
                _ => {}
            }
        }
        None
    }
}

fn arith<'db>(op: ConstOp, a: &Value<'db>, b: &Value<'db>) -> Option<Value<'db>> {
    let (Value::Int(a), Value::Int(b)) = (a, b) else {
        return None;
    };
    let v = match op {
        ConstOp::Add => a.checked_add(*b)?,
        ConstOp::Sub => a.checked_sub(*b)?,
        ConstOp::Mul => a.checked_mul(*b)?,
        // `checked_div`/`checked_rem` yield None on divide-by-zero and on the
        // `i128::MIN / -1` overflow — both surface as a non-const result.
        ConstOp::Div => a.checked_div(*b)?,
        ConstOp::Rem => a.checked_rem(*b)?,
    };
    Some(Value::Int(v))
}

fn project<'db>(v: &Value<'db>, field: &str) -> Option<Value<'db>> {
    let Value::Record(_, fields) = v else {
        return None;
    };
    fields
        .iter()
        .find(|(n, _)| n == field)
        .map(|(_, v)| v.clone())
}
