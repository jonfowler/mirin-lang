# First-pass syntax subset

This document defines the small Mirin surface syntax subset that current examples, tooling, and parser work should target.

## Scope

- `fn` declarations
- `struct` declarations
- `port` declarations
- inline `mod` declarations
- named argument sections
- positional argument sections
- clocked types with `@clk`
- `uint(n)` / `sint(n)` integer vectors (signed = two's complement; no
  implicit signedness mixing), `bits(n)` raw vectors (Eq only, no
  arithmetic, hex-default printing — `planning/bits.md`), and
  compile-time `integer`
- clock-associated resets with `Reset @clk`
- `let` bindings (sequential, forward-only) and `var` signal declarations (block-scoped, supports cyclic equations)
- component connection blocks: `=` for sinks (inputs), `=>` for sources (outputs, introduces a `var`-scoped name)
- method-style calls such as `value.reg{...}()`
- `Vec(N, A)` vectors: `[a, b, c]` / `[e; N]` construction, `v[i]`
  indexing (`bits(n)[i]` yields bool) — `planning/vectors.md`
- inline verilog as a fn body: `fn f(…) = verilog { raw SV with ${name} / ${const expr} splices }`; bare `$` passes through (`planning/inline_verilog.md`)

## Conventions

- Use Rust-like spacing for bindings and fields: `name: Type`
- `:` always introduces a *type*; `=` and `=>` always bind *values/connections*.
  Record constructors therefore use `=` (`packet { valid = false, payload = 0 }`),
  matching named arguments — NOT Rust's `field: value`. A port constructor may
  bind an opposite-direction field with `=>` (`df { data = d, ready => r }`),
  the record-literal analogue of a named-arg out-connection.
- Use trailing commas inside braced lists and record literals
- Keep named interface arguments in braces and ordinary value arguments in parentheses
- Use the `param` keyword for compile-time parameters and `dom` for clock-domain bindings; both place the name into the type environment as well as the value environment
- A named `param`/`dom` without a default is inferred from the call site; with a default, the default is the single fallback (no inference). Positional `param`/`dom` must always be supplied explicitly.

## Reserved words

Following Rust, the keywords are reserved: they may not be used as a binding name (def, parameter, result, field, local). The set that would otherwise *leak* into identifier position — and is rejected by a check in `syntax_errors` — is `in`, `out`, `dom`, `param`, `as`, `verilog`, `crate`, `self`. Their keyword uses (a port `out` field, `for x in v`, the `{dom}`/`(param)` sections, the method receiver `self`, a `crate::`/`self.` path) are not binding names and stay legal. Builtin *type* names (`Type`, `Clock`, `uint`, …) are NOT reserved — they live in the type namespace and are shadowable, exactly like Rust's primitive types (`let u32 = 5;`). (The grammar already rejects the hard keywords — `fn`, `let`, `when`, `else`, `super`, … — everywhere; tree-sitter's native `reserved` word-sets need CLI ≥ 0.25, but we pin 0.24.7, so the leaking contextual keywords are caught semantically instead.)

## Domains

- For now, only clock domains are in scope as part of the type system
- Write clocked values as `T @clk`
- Write resets as `Reset @clk`
- Treat generalized metadata as deferred design work, not part of the initial syntax subset

## Components

Components use:

- an optional named argument section
- an optional positional argument section
- an optional return type
- a block body

```rust
fn multAdd
  { dom clk: Clock, rstn: Reset @clk = high, c: uint[8] @clk = 0, }
  ( a: uint[8] @clk, b: uint[8] @clk )
  -> uint[8] @clk
  {
    let mult = a * b;
    let mult = mult[8:0];
    let mult = mult.reg{rstn}();
    let add = mult + c;
    return add;
  }
```

## Structs

Structs are positive data types and use Rust-like field syntax.

The more detailed current design for parameterized structs and ports is documented in `planning/structs_and_ports.md`.

```rust
struct Packet {
  valid: bool,
  payload: uint[8],
}
```

## Ports

Ports can carry directions on individual fields and may themselves take named parameters.

```rust
port Stream8
  { dom clk: Clock }
  {
    out valid: bool @clk,
    out data: uint[8] @clk,
    in ready: bool @clk,
  }
```

### Port position rule

- Keep data in positive positions where sensible
- A write input is an explicit exception
- If a port appears in an argument position but is being driven by the callee, mark that argument direction explicitly

```rust
fn connectStream
  { dom clk: Clock }
  ( upstream: Stream8{clk}, out downstream: Stream8{clk} )
  {
    downstream.valid = upstream.valid;
    downstream.data = upstream.data;
    upstream.ready = downstream.ready;
  }
```

## Bindings and cycles

Mirin uses two distinct binding forms:

- `let x = expr` — sequential lexical binding. Forward-only scope. Supports shadowing for pipeline-style code. The binder is a pattern: `let (a, b) = pair;` destructures (nested patterns allowed; planning/tuples.md).
- `var x: T` — block-scoped signal declaration. Participates in cyclic equations for register feedback and mutual structural wiring.

Tuples are Rust-shaped: `(A, B)` types (each element may carry its own `@domain`), `(a, b)` expressions, `x.0` projection, arity ≥ 2 (planning/tuples.md). `for` binders are patterns too: `for (i, x) in v.enumerate()`.

State feedback example:

```rust
var count: uint[8] @clk;
count = (count + 1).reg{rstn}(0);
```

See `planning/cycles_and_scoping.md` for full scoping rules.

## Component connection blocks

When instantiating a component, fields are connected using a braced argument block. The operator encodes direction:

- `field = expr` — sink: the component reads from this expression (input)
- `field => name` — source: the component drives this signal; `name` is introduced as a block-scoped signal if not already declared

```rust
reg_df {
  in_dat  = x * 4,
  output => out_df,
}();
```

The `in`/`out` direction keywords are optional and checked for consistency when present. See `planning/port_connections.md` for the full connection syntax.

## Modules

Mirin follows Rust's module system. The first implemented slice is the inline
form — `mod name { items… }` — which introduces a named scope nesting the same
item set (fns, structs, ports, impls, and further `mod`s):

```rust
mod arith {
  fn multAdd { dom clk: Clock } ( a: uint(8) @clk, b: uint(8) @clk ) -> uint(8) @clk {
    let mult = a * b;
    return mult.reg(rstn, 0) + 0;
  }
}
```

File-based modules use `mod foo;`: the body is loaded from `foo.mrn` in the
current file's directory, and that module's own children live under `foo/`
(e.g. `main.mrn`'s `mod util;` → `util.mrn`; `util.mrn`'s `mod cfg;` →
`util/cfg.mrn`). A `.mrn` file joins the crate only when some ancestor declares
it with `mod`.

A module is a name-resolution scope only — it does not change what Verilog is
generated. Bare names resolve in the current module, then the prelude; crossing
a module boundary needs a path or `use`:

```rust
use crate::parts::add3;          // absolute, from the crate root
use super::sibling;              // from the parent module
use a::{b, c::d, e as f};        // groups, nesting, rename
use a::*;                        // glob (lowest priority)
```

Paths use 2018-style relative resolution with `crate::`/`super::`/`self::`
anchors, in both `use` and expression position (`crate::m::g`). Names live in
two namespaces — **modules** and **everything else** (types, functions,
constructors). A module may share a name with a non-module item (a `mod df`
beside a `port DF = df {…}`), since module names appear only in path-prefix
position. But a type and its constructor share the item namespace, so they must
differ: `struct S = S {…}` is a name collision.

Items are **private by default** — visible only in the defining module and its
descendants. `pub` opens them up; the parenthesised forms narrow that reach:

```rust
pub fn f() { … }            // visible everywhere
pub(crate) struct S = s {…} // visible anywhere in the crate
pub(super) fn g() { … }     // visible in the parent module's subtree
pub(in crate::a) fn h() {…} // visible within module a's subtree
pub use crate::a::f;        // re-export: f becomes part of this module's surface
```

A plain `use` is private to its module; only `pub use` re-exports a name so
others can reach it through this module. Naming a private item from outside its
scope is an error. See `planning/modules.md` for the full design and staging.

Note: a path written directly in expression position resolves but does not yet
lower to hardware — import the item with `use` to call it. Type-position paths
(`a::B`) are likewise deferred; `use` the type instead.

## `impl` blocks

Methods are introduced via `impl` blocks. Generics are **binder-first** — the
braces after the `impl` keyword *declare* generic params (Rust's `impl<T>`):

```rust
impl Option { … }                          // no generics
impl {dom clk: Clock} Stream8 { … }        // binder declares clk; self @clk applies it
impl {dom clk: Clock, A: Type} Bus(A) { … }  // generic owner — APPLIED
```

A **generic owner is applied** in the header (`Bus(A)`), with its params
declared in the binder — the same shape as a trait impl's self type
(`impl {param n: integer} Add for uint(n)`). The owner is a genuine type, not a
bare constructor; each method monomorphises per instantiation of `A` at its
call sites. A non-generic owner needs no application (`Option`, `Stream8`).
Domains still attach through `self @clk`. (Trait impls — `impl {binders} Trait
for SelfType` — share the binder-first shape; see `planning/traits.md`.)

Resolver populates `impl_methods: (owner_def, method_name) → method_def`;
calls dispatch through that table. See the resolver and
`planning/ir_pipeline.md` for the wiring.

## Open questions kept out of the first parser slice

- exact inference rules for `dom clk`
- generics and const generics beyond simple examples
- generalized metadata syntax
- clock inference for cyclic `var` equations: inferring the clock domain of a `var` from its own equation requires a fixpoint pass. In practice the domain can almost always be resolved from an external anchor (the reset passed to `.reg`); see `planning/domain_checking.md`.
- `inline fn` as a modifier and keyword reservation.
