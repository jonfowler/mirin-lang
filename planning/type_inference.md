# Type and domain inference

This document describes Polar's type-checking pass: how HIR is annotated with concrete types, and which constraints are deferred for later passes. It draws on two pieces of prior art:

- **rustc's `InferCtxt`** for the engineering shape — a per-function context that owns inference-variable substitution tables, performs eager unification when an expression is visited, and queues obligations it can't yet solve into a fulfillment context.
- **OutsideIn(X)** (Vytiniotis et al., 2011) for the conceptual layering — the constraint generator is uniform; the solver is parameterised by the constraint domain X. Polar has two such domains today (types and clock domains), and others may appear later (widths, const-eval).

The first-pass goal is end-to-end type-checking of every example in `examples/`, with a structure that generalises cleanly to parametric structs.

## Phases and what we own

| Pass | Owns |
|---|---|
| Lowering (`hir::lower`) | Resolves names, slots args, splits `var` from equations. Leaves untyped slots as `None`. |
| **Type check (`typeck`)** | **Decides every expression's type, every local's type, every `var`'s type. Reports mismatches. Queues unsolved constraints.** |
| Future const-eval | Resolves widths that the type checker queued as `WidthEq` obligations. |
| Future domain-bound solver | Resolves `DomainBound { d, Clock }`-style constraints. |
| RTL lowering | Consumes fully typed HIR. |

The type checker writes back into HIR via two side tables: `expr_types: HashMap<HirId, HirType>` and `local_types: HashMap<LocalId, HirType>`. We do not mutate HIR nodes in place; this keeps the lowering output immutable through the rest of the pipeline.

## Architecture

```text
                  ┌─────────────────────────────────────┐
                  │            TypeCheck                │
                  │                                     │
        ┌─────────┤  per-function InferCtxt:            │
        │         │   ┌────────────────────────────┐   │
   HirFn│         │   │  type_vars: Vec<Resolution>│   │
   ─────┼────────►│   │  domain_vars: Vec<Resolution│  │
        │         │   │  obligations: Vec<Obligation│  │
        │         │   └────────────────────────────┘   │
        │         │                                     │
        │         │  walker:                            │
        │         │   - infer_expr   (returns HirType) │
        │         │   - check_stmt                      │
        │         │   - unify_*       (eager)           │
        │         │                                     │
        │         └─────────────────────────────────────┘
        │
        └─► TypeCheckResult { types, errors, residual_obligations }
```

The walker is generic in *what it generates*; the per-domain unification logic (types vs. domains) lives in `InferCtxt`. When a constraint can be solved immediately, the walker calls `unify_types(...)` / `unify_domains(...)` and any error propagates. When it can't (e.g. two widths that both reference unresolved const variables), the walker pushes an `Obligation` onto the queue — exactly the OutsideIn split.

### Inference variables

```rust
pub struct TypeVar(pub u32);
pub struct DomainVar(pub u32);

pub struct InferCtxt {
    type_vars: Vec<TypeResolution>,
    value_kind_vars: Vec<Option<ValueKind>>,  // structural-only Type-kind vars
    const_vars: Vec<Option<HirExpr>>,         // Const-kind vars (widths)
    domain_vars: Vec<DomainResolution>,
    obligations: Vec<Obligation>,
    locals: HashMap<LocalId, HirType>,
    expr_types: HashMap<HirId, HirType>,
    errors: Vec<TypeError>,
}

pub enum TypeResolution { Unbound, Bound(HirType) }
pub enum DomainResolution { Unbound, Bound(Domain) }
```

All four pools are dense `Vec`s; the variable's `u32` is its index. Lookup is union-find-by-path-compression (mirroring rustc's `UnificationTable`) — when we resolve `?T1` we walk the chain and shorten it for future lookups.

We deliberately separate the pools rather than tagging a single "var" enum. Types, value-kind structural parts, widths, and domains have different unification rules: types unify *structurally* (UInt ≡ UInt, not UInt ≡ Bool); widths normalise to sum-of-monomials before equality; domains form a subtyping lattice with `Const` as the top. Keeping them apart matches the OutsideIn(X) framing — one constraint generator, multiple solvers — and avoids accidentally cross-applying rules.

### Why "Rust-style" eager unification

When we walk `(acc + data)`, we know immediately that the two operands' types must match. Equating them on the spot lets *one* of them (e.g. a literal `0` of type `uint(?N) @const`) propagate concrete information into the other branch (`uint(8) @clk`). Deferring this to a post-walk solver loses that local context and produces worse errors.

The cases that genuinely *can't* be solved at the walk site go onto the obligation queue. In rustc this is the fulfillment context for trait obligations; in Polar the same shape handles things like "two widths I cannot evaluate yet are supposed to be equal."

### Why the obligation queue exists at all

The walker can't always make progress:

- **Widths** are `HirExpr`s. Unifying `uint(8)` with `uint(N+1)` requires evaluating `N+1`. We defer this as `WidthEq { lhs: HirExpr, rhs: HirExpr }`.
- **Clock-kind constraints** on register-like operations need to assert "the domain on this slot is a real `Clock`, not `@const`." We can detect when a slot resolves to a concrete domain immediately; if it resolves to a `Var(?D)` that's still ambiguous, we defer as `DomainKind { domain: Domain, expected_kind: DomainKind, span }`.
- **`@const` upcast to `@clk`** is allowed but only known to be safe at the use site. We currently treat the unifier as accepting `Const` ≡ `Clock(_)`; with MLsub-style bound tracking this becomes proper subtyping.

The queue's other job is producing residual constraints for later passes. The first pass discharges what it can and hands the rest to const-eval / domain-bound passes that haven't been built yet. We name those passes now, in the obligation enum, so adding them is purely additive.

## Walking HIR

```rust
fn check_fn(&mut self, hir_fn: &HirFn) {
    // 1. Seed: each parameter's declared type goes into locals[].
    for param in &hir_fn.params {
        self.locals.insert(param.local, param.ty.clone());
    }
    // 2. Visit body. Statements drive return-type and equation checks.
    self.check_block(&hir_fn.body, hir_fn.return_type.as_ref());
    // 3. After-walk: try to discharge remaining obligations once more,
    //    using the now-richer substitution. Whatever survives lands in
    //    TypeCheckResult.residual_obligations.
    self.flush_obligations();
}

fn check_stmt(&mut self, stmt: &HirStmt, expected_return: Option<&HirType>) {
    match stmt {
        HirStmt::Let(l)    => { let t = self.infer_expr(&l.value); self.locals.insert(l.local, t); }
        HirStmt::VarDecl(v) => { let t = v.ty.clone().unwrap_or_else(|| self.fresh_type_var()); self.locals.insert(v.local, t); }
        HirStmt::Equation(eq) => {
            let lhs = self.locals[&eq.lhs].clone();
            let rhs = self.infer_expr(&eq.rhs);
            self.unify_types(&lhs, &rhs, eq.span.clone());
        }
        HirStmt::Return(e) => {
            let t = self.infer_expr(e);
            if let Some(r) = expected_return { self.unify_types(r, &t, e.span.clone()); }
        }
        HirStmt::Expr(e)   => { let _ = self.infer_expr(e); }
    }
}
```

### `infer_expr` — the eager bit

The walker computes a type for each expression and records it in `expr_types`. Rough cases:

| Expr | Type produced |
|---|---|
| `Const(Integer(_))` | `Value(UInt { width: fresh_const_var }, Const)`. The width is left as a fresh `HirExpr` that const-eval will resolve from context. |
| `Const(Bool(_))` | `Value(Bool, Const)` |
| `Local(id)` | `locals[&id].clone()` |
| `Call(call)` for user fns (and `+`/`*`) | look up callee signature, zip args against params, unify each arg's inferred type against the param's declared type. `+` and `*` are synthesised prelude HirFns with the same parametric shape (`{N: usize, dom D: Clock}(uint(N) @D, uint(N) @D) -> uint(N) @D`) — same code path as any other generic call. |
| `Call(call)` for a struct constructor | the struct's declared fields are the callee's positional params (HIR lowering already slotted user-named fields into declared order). Unify each arg's type against the field's declared type. Return `Value(Struct { def }, fresh_domain_var)`. |

For `Call`, inferable named params (`dom clk`) become *fresh `DomainVar`s* at the call site. Each arg's inferred domain unifies with the corresponding param's domain — which threads `dom clk` through `rstn`'s `Reset @clk`, the receiver's `self @clk`, and the result's `uint(N) @clk` until they all agree.

This is exactly the substitution rustc applies when instantiating a generic function: fresh variables stand in for each generic parameter, get unified with use sites, and the answer is read out at the end. Polar's "generics" right now are the inferable `dom clk` named params and the `uint(N)` widths; parametric structs (`struct Bus(A: Type)`) will plug in here unchanged when they return — they just add more fresh-variable slots at instantiation.

Note that operators are also calls. `a + b` lowers to a `HirCall` against the prelude `+` DefId; `.reg(...)` likewise. There is no `HirExprKind::Binary` and no method-call shape at the HIR layer. `+`, `*`, `reg`, and `posedge` all have polymorphic signatures handled uniformly by the substitution path: each is a synthesised prelude `HirFn` with its `generic_params` declared on the resolve-side def, and `build_sig_subst` allocates fresh `ValueKind::Var`, `ConstVar`, or `DomainVar` per kind. No bespoke paths remain — see `planning/parametricity.md` for the const-kind inference design.

## Unification rules

### `unify_types(a, b, span)`

```text
unify_types(a, b):
    a' = resolve(a)
    b' = resolve(b)
    match (a'.kind, b'.kind):
        (Var(α), Var(β)) if α == β    => ok
        (Var(α), _)                    => bind α -> b'
        (_, Var(β))                    => bind β -> a'
        (Value(vt₁), Value(vt₂))       => unify_value_kinds(vt₁.kind, vt₂.kind)
                                          unify_domains(vt₁.domain, vt₂.domain)
        (Port(p₁), Port(p₂)) if p₁.def == p₂.def => ok
        (Clock, Clock)                 => ok
        otherwise                      => error(TypeMismatch { a, b, span })
```

`resolve` follows the substitution chain to a representative. Binding `α -> b'` updates the substitution table (with occurs-check to prevent `α -> List(α)`-style loops).

`unify_value_kinds` handles widths specially:

```text
unify_value_kinds(UInt{w₁}, UInt{w₂}):
    if both are literal constants: compare equal or error
    else: push obligation WidthEq { lhs: w₁, rhs: w₂ }
```

This is the deferred path. The const-eval pass that lands later will discharge `WidthEq` once both sides reduce to literals.

### `unify_domains(a, b, span)`

```text
unify_domains(a, b):
    a' = resolve(a)
    b' = resolve(b)
    match (a', b'):
        (Var(α), Var(β)) if α == β => ok
        (Var(α), _)                 => bind α -> b'
        (_, Var(β))                 => bind β -> a'
        (Const, Const)              => ok
        (Const, Clock(_))           => ok   // const <: clock (subtype rule)
        (Clock(_), Const)           => ok
        (Clock(x), Clock(y)) if x==y => ok
        (Unspecified, x)            => convert Unspecified to a fresh DomainVar bound to x
        (x, Unspecified)            => same
        otherwise                   => error(DomainMismatch { a, b, span })
```

The `Const` ≡ `Clock(_)` rule is intentionally lenient for now. It matches the existing `domain_checking.md` framing (`Const` is a supertype of every concrete clock). When MLsub-style bound tracking lands, this becomes proper lower-bound subtyping: each variable carries an upper and lower bound and the lattice is enforced more precisely. The current rule will be a special case of the new one — no caller has to change shape.

The `Unspecified` -> fresh-var step exists because the lowering pass emits `Domain::Unspecified` for untyped slots. Inference replaces these with real variables on first encounter; subsequent uses share them.

## Obligation queue

```rust
pub enum Obligation {
    WidthEq { lhs: HirExpr, rhs: HirExpr, span: SourceSpan },
    // Phase D: width predicates over normalised sum-of-monomials forms.
    // Produced by `unify_widths` when normalised equality fails locally;
    // simplified to fixpoint at end-of-fn and propagated to callers.
    ConstEq { lhs: NormalConst, rhs: NormalConst, span: SourceSpan },
    DomainKind { domain: Domain, expected: DomainKind, span: SourceSpan },
    // Future: ConstEval(HirExpr), TraitLike(...), etc.
}

pub enum DomainKind { ClockDomain }   // currently only this; future: NegativeEdge, etc.
```

`discharge_obligations` runs after the walk as a fixpoint loop: each iteration simplifies every `ConstEq` obligation using current `const_vars` bindings (substituting bound vars into the normalised form, then canonicalising), and either discharges it (`lhs - rhs == 0`), errors (ground difference, non-zero), or keeps it for another iteration. What survives the fixpoint is the fn's residual constraint set — attached to `TypeCheckResult.fn_residuals` keyed by `DefId`.

At a call site, the callee's residuals get their `Param(i)` references rewritten through the call's `GenericArgs`, then pushed as fresh `ConstEq` obligations in the caller's queue — the caller's own discharge loop handles them. This propagates residuals up the call graph until they hit a monomorphic instantiation, where `lower_to_sv` emits surviving residuals as `initial begin assert(lhs == rhs); end` (Phase D′).

This is structurally the same as GHC's `Wanted` constraint solver and rustc's "select all obligations" loop in `FulfillmentContext`. The Polar twist: residuals that *do* survive to monomorphisation become SystemVerilog elaboration-time checks rather than runtime exceptions.

## Errors

```rust
pub enum TypeError {
    TypeMismatch { expected: HirType, got: HirType, span: SourceSpan },
    DomainMismatch { expected: Domain, got: Domain, span: SourceSpan },
    UnknownStructField { struct_def: DefId, field: String, span: SourceSpan },
    MissingStructField { struct_def: DefId, field: String, span: SourceSpan },
    DuplicateStructField { field: String, span: SourceSpan },
    AssignmentToImmutable { local: LocalId, span: SourceSpan },
    // ...
}
```

Errors carry resolved types (substitution applied) so diagnostics show the user a concrete shape rather than `?T7`.

## What's deferred from the first go

To keep the first implementation bounded, the following are punted:

- **Width const-eval.** `WidthEq` obligations accumulate but are not solved. Pure-literal width matches are still checked structurally; symbolic widths are accepted and queued.
- **MLsub bound tracking.** Domain subtyping is enforced via the lenient `Const`-or-equal rule; a richer bounded-polymorphism solver lands later.
- **Clock-kind enforcement on `.reg`.** The "register requires a real clock, not `@const`" rule (per `planning/domain_checking.md`) is queued as a `DomainKind` obligation but not discharged. With the lenient `Const`/`Clock` unification above, an all-`@const` register call would slip through; the obligation queue exists to catch it once the kind solver lands.
- **Path expressions / method dispatch.** Still out of scope; `impl` is in `todo-examples/`.

## How this generalises to parametric structs

When `struct Bus(A: Type)` returns, the work is small:

- A struct's `DefId` gains a list of type/clock parameters (and later `param` bindings).
- At a `Value(Struct { def, args })` use site, the type checker takes the struct's parameter list, allocates a fresh `TypeVar` / `DomainVar` per parameter, substitutes them into the struct's field types, and proceeds. This is the same machinery already used for inferable named params.
- Unification of two `Value(Struct { def, args })`s requires `args` to unify element-wise — identical to how rustc unifies generic instances.

The split between *generation* (`infer_expr`, etc.) and *solving* (`unify_*`, `flush_obligations`) means generation already operates over a tree that *could* carry parameters; we just don't have any in the surface today. Adding them is a matter of extending `PortTypeRef` / `Struct { def }` to carry `Vec<HirType>` and updating the unification cases accordingly. No rewrite.

## How this generalises to a real domain solver

Today's `unify_domains` is a single function. The OutsideIn(X) shape says: replace it with calls into a domain-specific solver `X`. Concretely:

- `unify_domains` becomes `domain_solver.unify(a, b)`.
- The current rules go into `LatticeDomainSolver` — top is `Const`, bottom-elements are concrete `Clock(_)`s, equality where applicable.
- A future MLsub solver replaces this with bounded-variable tracking. The walker doesn't change.

This is the value of the OutsideIn split: the constraint *generator* (walking HIR, emitting "these two domains must agree") is stable across all of these solver upgrades. Only the solver implementation changes.
