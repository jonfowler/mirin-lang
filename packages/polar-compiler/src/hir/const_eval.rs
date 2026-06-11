//! Compile-time evaluation of `@const` values (`planning/const_eval.md`).
//!
//! The rustc analogue is CTFE (`const_eval_*` interpreting MIR on demand),
//! with one structural divergence: a Polar fn body is an **equation system**,
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
use crate::hir::body::{Block, Body, ConnArg, ExprId, ExprKind, Stmt, body};
use crate::hir::sig::{Param, sig_of};
use crate::hir::types::{ConstArg, ConstOp, Direction, LocalId};
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
    c: &ConstArg,
) -> Option<i128> {
    let mut ev = Evaluator {
        db,
        krate,
        map: crate_def_map(db, krate),
        steps: 0,
    };
    let frame = Frame::root(db, krate, def);
    match ev.eval_const_arg(&frame, c, 0)? {
        Value::Int(v) => Some(v),
        _ => None,
    }
}

struct Evaluator<'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &'db CrateDefMap<'db>,
    steps: u32,
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
        c: &ConstArg,
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
            // Params stay symbolic at the root; Infer/Deferred/Error are
            // not evaluable.
            _ => None,
        }
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
            ExprKind::Number(n) => Some(Value::Int(*n)),
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
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Def(def) = frame.body.expr(*callee).kind else {
                    return None;
                };
                if let Some(op) = self.prelude_op(def) {
                    let [a, b] = args.as_slice() else { return None };
                    if a.out || b.out {
                        return None;
                    }
                    let a = self.eval_expr(frame, a.expr, depth)?;
                    let b = self.eval_expr(frame, b.expr, depth)?;
                    return arith(op, &a, &b);
                }
                if depth >= MAX_DEPTH {
                    return None;
                }
                let callee_frame = self.enter_call(frame, def, args, depth)?;
                self.eval_return(&callee_frame, depth + 1)
            }
            // when / .reg / method calls / Missing / bare defs: not const.
            _ => None,
        }
    }

    /// A callee's return value: the body tail, or its single `return`.
    fn eval_return(&mut self, frame: &Frame<'db>, depth: u32) -> Option<Value<'db>> {
        let block = frame.body.block().clone();
        self.eval_block(frame, &block, depth)
    }

    /// A block's value: its tail expression, or the value of a top-level
    /// `return` statement. (Lets inside the block are reached by demand.)
    fn eval_block(&mut self, frame: &Frame<'db>, block: &Block, depth: u32) -> Option<Value<'db>> {
        if let Some(tail) = block.tail {
            return self.eval_expr(frame, tail, depth);
        }
        for stmt in &block.stmts {
            if let Stmt::Return { value } = stmt {
                return self.eval_expr(frame, *value, depth);
            }
        }
        None
    }

    fn prelude_op(&self, def: DefId<'db>) -> Option<ConstOp> {
        let d = self.map.def_data(def)?;
        if d.module != self.map.prelude() {
            return None;
        }
        match d.name.as_str() {
            "+" => Some(ConstOp::Add),
            "-" => Some(ConstOp::Sub),
            "*" => Some(ConstOp::Mul),
            _ => None,
        }
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
