# HIR design

This document describes Polar's High-level Intermediate Representation: the IR that sits between Surface IR (parsed and name-resolved syntax) and type inference. HIR is what every later pass — direction checking aside — actually operates on.

## Purpose

HIR is the first IR that is structured for semantic analysis rather than for matching source syntax. Its goals are:

- Bake in name resolution so no later pass looks up identifiers by string.
- Erase surface sugar: method calls become regular calls, named/positional argument distinctions disappear, declared defaults are substituted at call sites.
- Make cyclic `var` structure explicit: declarations and equations are separate nodes, so the equation system is a uniform thing to walk.
- Hold a slot for a type on every expression — `None` until type inference runs.

What HIR does **not** do:

- Allocate inference variables — that is the type checker's job.
- Resolve inferable parameters (`dom clk`) beyond what was already explicit in the source.
- Const-evaluate widths or other compile-time integers — those stay as `HirExpr` and are checked by a dedicated const-eval pass.

## Identity scheme

HIR uses a dense per-function `LocalId(u32)` for every local binding (parameter, `let`, `var`, implicit `var` from `=>`). HIR owns its own per-function `locals` table; the resolver's `ResolveResult.locals` is consulted only during lowering, to translate surface `NodeId`s into HIR `LocalId`s.

```rust
pub struct LocalId(pub u32);

pub struct HirLocalInfo {
    pub kind: LocalKind,           // Param | Let | Var | ImplicitVar
    pub name: String,              // textual for now; Symbol later
    pub span: SourceSpan,
    pub surface_node: NodeId,      // back-pointer for diagnostics
}
```

Top-level definitions continue to use `DefId` from `resolve.rs`. HIR does not introduce a parallel def table; the existing `ResolveResult.defs` remains authoritative.

Expression nodes carry a `HirId` for cross-references during type inference and diagnostics. For now `HirId` is a thin wrapper around `NodeId`, allocated from the same counter the lowering pass uses for synthesised nodes.

## Top-level structure

```rust
pub struct HirSourceFile {
    pub items: Vec<HirItem>,
}

pub enum HirItem {
    Fn(HirFn),
    Struct(HirStruct),
    Port(HirPort),
    // Impl lands later — out of basic first-pass scope.
}

pub struct HirFn {
    pub def_id: DefId,
    pub name: String,
    pub params: Vec<HirParam>,     // named section first, positional second
    pub return_type: Option<HirType>,
    pub locals: Vec<HirLocalInfo>, // dense table indexed by LocalId
    pub body: HirBlock,
    pub span: SourceSpan,
}

pub struct HirParam {
    pub local: LocalId,
    pub section: ParamSection,     // Named | Positional
    pub kind: ParamKind,           // Value | Param | Dom (from `param`/`dom` keyword)
    pub direction: Option<Direction>,  // `in`/`out` on positional params
    pub ty: HirType,
    pub default: Option<HirExpr>,
    pub span: SourceSpan,
}

pub enum ParamSection { Named, Positional }
```

The `params` vector is the single source of truth for a function's signature. Named-section parameters come first, in declaration order; positional-section parameters follow, also in declaration order. Call-site argument slots match this vector index-for-index (see *Calls*).

## Statements

```rust
pub struct HirBlock {
    pub statements: Vec<HirStmt>,
    pub span: SourceSpan,
}

pub enum HirStmt {
    Let(HirLet),
    VarDecl(HirVarDecl),
    Equation(HirEquation),
    Return(HirExpr),
    Expr(HirExpr),
}

pub struct HirLet {
    pub local: LocalId,
    pub value: HirExpr,
    pub span: SourceSpan,
}

pub struct HirVarDecl {
    pub local: LocalId,
    pub ty: Option<HirType>,
    pub span: SourceSpan,
}

pub struct HirEquation {
    pub lhs: LocalId,
    pub rhs: HirExpr,
    pub span: SourceSpan,
}
```

**`var` is split.** Surface `var x: T = init;` lowers to a `VarDecl { local, ty }` immediately followed by an `Equation { lhs: x, rhs: init }`. Surface `x = expr;` (assignment to a previously-declared var) lowers directly to `Equation`. Surface `comp { out => x }()` also produces an `Equation` whose `rhs` is the relevant component output.

Once all assignment forms are uniform `Equation` nodes, the single-driver / undriven checks become "count `Equation`s with this `lhs`," with no special case for the declaration-with-initializer form.

`Let` keeps its declaration-and-value shape because `let` has different scoping rules (forward-only, supports shadowing) and is not part of the equation system.

## Expressions

```rust
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: Option<HirType>,       // filled by type inference
    pub span: SourceSpan,
    pub id: HirId,
}

pub enum HirExprKind {
    Const(ConstValue),             // literal, domain inferred as @const
    Local(LocalId),
    Call(HirCall),                 // every "do something with arguments" lowers here
}

pub enum ConstValue {
    Integer(u64),
    Bool(bool),
}
```

For literals: at HIR construction the lowering pass tags the expression's `ty` as `Some(HirType { kind: UInt { width: <inferred> }, domain: Some(Domain::Const), .. })` (or similar for `bool`). Inferring the width from context — which most pure literals need — is left to type checking, so width may stay as a placeholder `HirExpr` until then.

## Types

```rust
pub struct HirType {
    pub kind: HirTypeKind,
    pub span: SourceSpan,
}

pub enum HirTypeKind {
    Var(TypeVar),                  // type-inference variable
    Value(ValueType),              // scalars and structs
    Port(PortTypeRef),              // ports — no top-level domain
    Clock,                         // meta-kind
    Usize,                         // meta-kind
}

pub struct ValueType {
    pub kind: ValueKind,
    pub domain: Domain,
}

pub enum ValueKind {
    UInt { width: HirExpr },
    Bool,
    Reset,
    Struct { def: DefId },
}

pub struct PortTypeRef { pub def: DefId }
pub struct TypeVar(pub u32);

pub enum Domain {
    Const,                         // attached to literals
    Clock(LocalId),                // refers to a Clock-typed param or const
    Unspecified,                   // type inference must fill this in
}
```

**Compound vs value is factored at the top.** A `let x;` introduces `?T = Var(_)`. The unifier later narrows it to `Value(...)` or `Port(...)` based on use sites, and a domain only appears on the `Value` branch. Ports carry no top-level domain because clocking flows through the port's own `dom clk` (or similar) parameter into per-field types.

**Domain lives on `ValueType`.** This matches `planning/domain_checking.md`: `uint(8) @clk` is a single value type. The unifier reasons about one `HirType` value at a time, and a domain mismatch is just a value-type mismatch. Note that domain *inference* is somewhat orthogonal to the rest of type inference and is expected to be implemented as its own constraint set within the type checker.

**Early failure for `@` on a port.** `lower_type` rejects `Stream8 @clk` once parametric port application returns to the grammar; until then the annotation is tolerated to keep first-pass examples expressible without parametric types.

**Width is an `HirExpr`.** `uint(8)` and `uint(bits)` use the same `HirExpr` slot; a dedicated const-eval / const-check pass verifies that the width position is actually a compile-time constant. This avoids a parallel `ConstExpr` enum that would have to grow alongside `HirExpr` whenever `usize` arithmetic is extended.

## Items beyond functions

Basic-first-pass HIR also lowers `struct` and `port` items:

```rust
pub struct HirStruct {
    pub def_id: DefId,
    pub name: String,
    pub fields: Vec<HirStructField>,
    pub span: SourceSpan,
}

pub struct HirStructField {
    pub name: String,
    pub ty: HirType,
    pub span: SourceSpan,
}

pub struct HirPort {
    pub def_id: DefId,
    pub name: String,
    pub params: Vec<HirParam>,         // shape mirrors HirFn: named section first, then positional
    pub fields: Vec<HirPortField>,
    pub span: SourceSpan,
}

pub struct HirPortField {
    pub direction: Direction,
    pub name: String,
    pub ty: HirType,
    pub span: SourceSpan,
}
```

`impl` blocks remain out of scope; they need method resolution + path expressions, both of which require type information.

## Calls

The unified call shape:

```rust
pub struct HirCall {
    pub callee: DefId,
    pub args: Vec<HirArg>,         // one entry per param of the callee, in declared order
    pub span: SourceSpan,
}

pub enum HirArg {
    Provided { expr: HirExpr, source: HirArgSource },
    Inferable,                     // an inferable param (dom clk) the type checker must fill
}

pub enum HirArgSource {
    Given,                         // user wrote this argument at the call site
    Default,                       // substituted from the param's declared default
}
```

`HirCall::args.len()` always equals the callee's parameter count. The lowering pass populates each slot:

1. Source-level named arguments are matched to slots by name. Direction checking has already verified that named-argument names exist on the callee.
2. Source-level positional arguments fill consecutive positional slots starting at the first positional-section parameter.
3. Remaining empty slots are filled as `Default(...)` if the param has a declared default, `Inferable` if the param is a named `param`/`dom` binding with no default, or an error otherwise (missing required argument).
4. Method calls (`x.reg(rstn, 0)`) are desugared at this stage: the receiver `x` fills the `self` parameter slot of the callee.

Each `HirArg` keeps its own span. For `Provided { source: Given }`, the span points into source; for `Provided { source: Default }`, into the default expression in the callee's declaration; for `Inferable`, at the call site.

Surface-level binary operators (`a + b`, `a * b`), method calls (`x.reg(rstn, 0)`), and struct constructors (`packet { valid: false, payload: 0 }`) all desugar to `HirCall` at lowering time:

- Operators have prelude `DefId`s (`+`, `*`). HIR lowering produces `Call { callee: +-def, args: [Given(a), Given(b)] }`.
- Methods slot the receiver into the callee's `self` param. `x.reg(rstn, 0)` becomes `Call { callee: reg-def, args: [Inferable, Given(x), Given(rstn), Given(0)] }`.
- Struct constructors slot user-provided named fields into declared field order. `packet { payload: 0, valid: false }` becomes `Call { callee: Packet-def, args: [Given(false), Given(0)] }` — the constructor's "signature" is the field list. Missing, unknown, or duplicate field names are caught during HIR lowering so type-checking sees a well-formed call.

There is **no `Callee::Builtin` variant**. Primitive operations such as `.reg` are registered as compiler-provided definitions in a *prelude* during resolver initialisation: each gets a real `DefId`, a `HirFn`-shaped signature, and is therefore indistinguishable from a user-defined function for the purposes of call resolution. This keeps later passes from having to branch on call kind.

### Prelude

For the first pass the prelude contains one entry:

- `reg : fn{dom clk: Clock}(self @clk, rst: Reset @clk, reset_val: uint(N)) -> uint(N) @clk`

The prelude is built into the `ResolveResult.defs` table by the resolver at initialisation. Future primitives (e.g. `concat`, comparison operators if they become functions, etc.) are added the same way.

## Lowering: Surface IR + ResolveResult → HIR

The lowering pass runs after name resolution and direction checking. It does **all** of the following in a single walk:

1. **Names baked in.** Identifier expressions in surface IR become `HirExprKind::Local(LocalId)` (after translation from `NodeId`) or `HirExprKind::Call` with the appropriate `DefId`. The resolver's side table is no longer consulted by later passes.
2. **Method desugaring.** `x.f(args)` becomes `Call { callee: f_def_id, args: [Given(x), ...] }` with `x` slotted into the callee's `self` parameter.
3. **Default substitution.** Missing named-arg slots whose param has a declared default get `HirArg::Provided { source: Default, expr }` filled in. The default expression has already been name-resolved in its declaration scope (this happens during name resolution).
4. **Inferable marker.** Missing slots whose param is a named `param`/`dom` binding with no default become `HirArg::Inferable`. No attempt is made to anchor these from explicit `Reset @clk` arguments — that is left to type inference, which already needs the same machinery.
5. **`var` split.** Surface `VarStatement` with an `init` becomes `VarDecl` + `Equation`.
6. **Surface-AST helpers gone.** `PostfixExpression`, `NamedArgument`, `PathExpression`, etc., disappear; everything is `HirExprKind`.

Things the lowering pass does **not** do:

- Allocate inference variables for untyped slots. `HirExpr::ty` and `HirVarDecl::ty` stay `None`; type inference owns variable allocation.
- Resolve inferable arguments through cyclic `var` equations, or even through anchored `Reset @clk` arguments. All `dom clk` slots that the user omitted become `Inferable` and are handed to type inference.
- Const-evaluate widths or any other compile-time integer. `uint(N)` keeps `N` as an `HirExpr`; a separate const-check pass validates that the expression is in fact const, and a const-eval pass produces the numeric value when needed.

## What runs against HIR

| Pass | Reads | Writes |
|---|---|---|
| Const check | `HirExpr` in const positions (widths, etc.) | Diagnostics |
| Type / domain inference | `HirExpr` (without `ty`) | `HirExpr.ty` populated; `Domain::Unspecified` replaced |
| Equation completeness | `HirVarDecl`, `HirEquation` | Diagnostics (undriven, multi-driver) |
| RTL lowering (future) | Fully-typed HIR | RTL IR |

## Open questions

- **Symbol interning.** `HirLocalInfo::name` and `HirFn::name` remain `String` for now. Moving to an interner is the right long-term direction (per `planning/surface_ir_discussion.md`) but is not part of this slice.
- **Implicit `var` from `=>`.** Surface IR already marks these via `LocalKind::ImplicitVar`. In HIR they appear as ordinary `HirVarDecl` nodes, but the lowering pass needs to decide whether to emit the synthetic `VarDecl` at the source point of the `=>` (forward-only visibility) or hoist it to block scope. Documented under `planning/known_issues.md` #2; the conservative initial behaviour is to emit at the source point so visibility matches the resolver's forward-only model.
- **`if`/`match` and `var`.** Not in first-pass scope but needs a rule before either control flow form reaches HIR. See `known_issues.md` #3.
- **`impl` method bodies.** Out of scope for first-pass HIR. See `known_issues.md` #8.
