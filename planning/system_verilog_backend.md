# SystemVerilog backend

This document is the design for Polar's SystemVerilog backend, the stages after
type checking. It supersedes the earlier stub. The first-pass scope is the
eight examples in `examples/` — concretely, every example must lower to a
SystemVerilog module that elaborates cleanly under a standards-compliant tool
(verilator/slang) and behaves the same as the Polar source.

## High-level pipeline

```text
typed HIR                     (post-typeck; ports/structs still aggregate)
  │
  ▼  flatten_aggregates       (HIR → HIR pass)
typed HIR'                    (no port/struct types; everything is value-typed)
  │
  ▼  re-check (optional)      (run typeck/single-driver on HIR' as a sanity pass)
  │
  ▼  sv_lower                 (HIR' → SV IR, structure only)
SV IR                         (modules, ports, decls, always_ff blocks)
  │
  ▼  sv_emit                  (SV IR → SystemVerilog text)
.sv file(s) in output directory
```

The flattening pass produces a strict subset of HIR (the original `HirType`
enum minus its `Port` and `Struct` cases at value positions). Because the
output is still HIR, the existing typeck and single-driver passes can re-run
on it — this is a deliberate sanity check, not a redundancy.

`sv_lower` walks flattened HIR and builds a thin SV-shaped IR. `sv_emit` is a
formatting-only pass that turns SV IR into text.

## Key decisions

### No monomorphisation for value-level parameters

SystemVerilog already has parametric modules:

```systemverilog
module add #(parameter int N = 8) (
    input  logic [N-1:0] a, b,
    output logic [N-1:0] out
);
    assign out = a + b;
endmodule
```

So a Polar function `fn add{param N: usize}(a: uint(N), b: uint(N)) -> uint(N)`
maps directly to one SV module with `parameter int N` — not one stamped module
per instantiation. This means:

- `param N: usize` parameters pass straight through to SV `parameter`
  declarations.
- `uint(N) @clk` becomes `logic [N-1:0]` in SV, with `N` referring to the
  module's parameter.
- Width-equation obligations like `N+N ~ 2*N` that the Polar checker leaves as
  residual obligations need not be discharged before emission. They can either
  be checked algebraically (early Polar-level error) or carried into the
  emitted SV as expressions and left to the SV elaborator. The first pass
  does the former where both sides are ground (already handled inline by
  typeck) and the latter for parametric cases.

Specialisation (real monomorphisation) is required only for structural
polymorphism, which is **out of first-pass scope**:

1. Port-type parameters (`fn pipeline{P: Port}(p: P)`). The emitted module's
   port list depends on which fields `P` has, so each concrete `P` needs its
   own module.
2. Higher-order functions (taking a function as a value). SV has no equivalent.

When these return, a `specialise` pass slots in between typeck and flattening.

### Port and struct flattening happens in HIR

Polar aggregates (ports, structs) do not survive into SystemVerilog at value
positions. SV ports are flat lists of `logic` declarations; SV registers and
nets do not carry per-field directions. The cleanest place to handle this is
an HIR-to-HIR pass so:

- The flat shape is checkable by the existing type/direction/single-driver
  machinery (with structural types disabled).
- The SV lowering step has nothing aggregate to think about.
- The pass can be tested in isolation by inspecting HIR'.

Ports flatten everywhere they appear: in function signatures, in local
declarations, in equations, and in the result type. Structs flatten the same
way; the only difference is structs carry no per-field direction.

We chose flattening over SV packed structs/interfaces because:

- Polar structs and ports can be parametric over types (`struct Bus(A: Type)`).
  SV packed structs cannot.
- Flattening uses one delimiter rule uniformly and matches what hand-written
  SV designers do for these patterns anyway.
- SV interfaces would handle ports but not structs; using one mechanism is
  simpler.

The trade-off is wider port lists in emitted SV. We treat that as acceptable
for first pass.

### Field delimiter: `__`

Flattened field names use `__` (double underscore) as the separator: a port
local `p` of type `Stream8` produces locals `p__valid`, `p__data`, `p__ready`.
Nested aggregates chain the separator: `bus__payload__lo`.

Polar identifiers may not contain `__`. The Polar grammar today produces
camelCase identifiers, so this rule rules out only generated/structural names,
not realistic user code. The flattening pass relies on this to keep its
generated names collision-free.

### Each `fn` is one SV module

In first-pass scope there are no calls between user functions in any example,
so each Polar `fn` becomes a self-contained SV module and there is no
inter-module instantiation step to design. Calls between user functions land
as soon as a real example requires them; the natural representation is a
`HirInstance` statement added after flattening, which the SV emitter renders
as a module instance with explicit port connections.

### Reset: synchronous, active-low

For first pass, every reset is treated as **synchronous and active-low**:

```systemverilog
always_ff @(posedge clk) begin
    if (!rstn) acc <= '0;
    else       acc <= acc + data;
end
```

The sensitivity list contains only the clock edge — no `negedge rstn`. The
reset condition is `!rstn`, matching the `rstn` naming convention (the `n`
suffix indicates active-low).

The `high`/`low` polarity literal on `rstn: Reset @clk = high` is captured in
HIR (as a `ConstValue::Bool`) but the SV emitter ignores it for first pass.
Configurable polarity (and async vs sync reset) is deferred work — the model
needs more design before it should affect emission.

### Output destination

The CLI takes `--out <dir>` (default: `./sv/`) and writes one `.sv` file per
input `.plr`. The CLI's current "print the CST" mode moves behind `--emit cst`.

When the output directory does not exist, the CLI creates it. When the
directory exists and contains files that the current run does not produce,
those files are left alone (no clean-up); this keeps the CLI safe to run in
shared directories.

### SV reserved-word collisions: hard error

Polar's grammar does not reserve `input`, `output`, `module`, `logic`, etc.,
but SystemVerilog does. When the emitter encounters a user identifier that
collides with a SV reserved word, it raises an error rather than mangling the
name. The user must rename the conflicting identifier in their Polar source.

The static SV reserved-word set lives next to the emitter. The negative test
case is `fail-examples/sv-reserved-word.plr`, which uses `input` as a
parameter name and should produce an emitter error with the offending
identifier and its source span.

## Other decisions, less load-bearing

### `reg` lowering

Inline `always_ff` per the reset section above. A reusable `polar_reg
#(WIDTH)` module would force every register into a module instance, which is
heavier than necessary — SV's `always_ff` is already the canonical idiom and
inline emission matches what hand-written RTL looks like.

### Separate RTL IR: skipped

Flattened HIR is already RTL-shaped (no aggregates, every value has a type and
domain, no method-call sugar, register lowering is local to the `reg` call).
The SV IR is thin — modules, ports, decls, `always_ff`, `assign`, expressions
— and stays a formatting concern, not an analysis concern. If real
optimisation work appears later (CSE, dead-code), an RTL IR can slot in
between flattened HIR and SV IR without restructuring callers.

## The flattening pass

This pass is the core of the backend. The rest is mostly mechanical.

### Inputs and outputs

- Input: typed HIR file (every `HirExpr.ty` filled, every domain resolved).
- Output: HIR file in which:
  - No `HirType` has `HirTypeKind::Value(ValueType { kind: Struct {..}, .. })`.
  - No `HirType` has `HirTypeKind::Port(_)`.
  - Every `HirParam.ty` is a value type, a clock, or one of the scalar
    primitives.
  - Every `HirEquation`, `HirLet`, and `HirReturn` works only with value-typed
    expressions.

### Naming

For an aggregate local named `p` of type `Stream8` with fields `valid`,
`data`, `ready`, the flattened locals are named `p__valid`, `p__data`,
`p__ready` (lowering preserves the user-facing identifier as a prefix to keep
diagnostics readable).

For nested aggregates (struct inside port, port inside struct), the
delimiter chains: `bus__payload__lo`, `pipe__packet__valid`. Polar's grammar
today admits these nestings even though no example uses them; the algorithm
handles them by recursion.

For a function returning an aggregate, the synthetic name is `result`. So
`fn f() -> Packet` flattens to a function with no return and `out` parameters
`result__valid: bool` and `result__payload: uint(8)`. (Naming the return
explicitly in surface syntax — `fn f() -> p: Packet` — is a deferred surface
extension; the synthetic name is the current rule.)

### Direction model for ports

A port field's direction is declared from the port's *internal* perspective:
`out valid` means "the producer of this port drives `valid`." A function's
parameter direction (whether the parameter has the `out` keyword) determines
whether the function body is the producer or the consumer of that port.

The combined "function-body direction" for each field:

| param direction | field declared | function-body sees field as |
|---|---|---|
| (none — input) | `out` | **input** (function reads upstream's output) |
| (none — input) | `in`  | **output** (function drives upstream's input) |
| `out`           | `out` | **output** (function drives downstream's output) |
| `out`           | `in`  | **input** (function reads downstream's input) |

For a whole-port equation `sink = source`, each field becomes one flattened
equation. The driven side (the LHS in the flat equation) is whichever side
sees that field as a function-body output. The other side sees the same field
as a function-body input. If both see input, both see output, or directions
don't pair correctly, the flattening pass reports a direction error.

### Algorithm sketch

```text
flatten_file(file):
    expansion := compute_expansion_table(file.items)
    rewrite each HirItem::Fn:
        rewrite params (replace each aggregate param with one param per
                        expanded field, preserving direction)
        rewrite return type (replace aggregate return with synthetic
                             `out result__...` params; set return to None)
        rewrite body:
            allocate new LocalIds for each aggregate local's fields
            rewrite statements in order

rewrite_stmt:
    Let { lhs, value }:
        if value is aggregate-typed:
            split value into per-field values (recursive)
            emit one Let per field, lhs := lhs__field
        else: keep as-is
    VarDecl { local, ty }:
        if ty is aggregate: emit one VarDecl per field local
        else: keep
    Equation { lhs, rhs }:
        if lhs is aggregate-typed local:
            split: for each field, emit one Equation
                   (direction-aware for ports)
        else: keep
    Return e:
        if e is aggregate-typed: emit one Equation per field
                   driving result__field from e__field, then Return ()
        else: keep
```

### `split` (per-field value extraction)

This is the function that turns an aggregate-typed expression into per-field
expressions. The cases:

| expression shape | per-field value |
|---|---|
| `HirExprKind::Local(id)` | `Local(id__field)` for each field |
| struct/record constructor call | the call's slot for that field, recursively split |
| `.reg(rstn, reset_val)` on an aggregate | one `.reg(rstn, reset_val__field)` per field; the reset value is split the same way (recursing into constructors as needed) |
| user-function call returning aggregate | (out of first-pass scope; reserved for the `HirInstance` form) |

For ports, an additional consideration: the only aggregate-producing form in
first-pass examples is `Local(id)`. There are no port constructors at value
position in the current grammar (ports are produced by being declared, not
by being literally constructed). So `split` for ports reduces to the
`Local` case.

### Worked example 1 — `simple_port.plr`

Source:

```polar
port Stream8 { dom clk: Clock } = stream8 {
  out valid: bool @clk,
  out data: uint(8) @clk,
  in  ready: bool @clk,
}

fn connectStream
  { dom clk: Clock }
  ( upstream: Stream8 @clk, out downstream: Stream8 @clk )
  {
    downstream = upstream;
  }
```

Field-direction table (from §"Direction model"):

| param          | field | declared | function-body direction |
|---|---|---|---|
| `upstream`     | valid | out      | **input** |
| `upstream`     | data  | out      | **input** |
| `upstream`     | ready | in       | **output** |
| `downstream`   | valid | out      | **output** |
| `downstream`   | data  | out      | **output** |
| `downstream`   | ready | in       | **input** |

After flattening, the signature becomes (named-section first, positional
second; preserving Polar order):

```
fn connectStream {
    dom clk: Clock,
} (
    upstream__valid: bool @clk,
    upstream__data:  uint(8) @clk,
    out upstream__ready: bool @clk,
    out downstream__valid: bool @clk,
    out downstream__data:  uint(8) @clk,
    downstream__ready: bool @clk,
) { … }
```

The equation `downstream = upstream` splits into one per field. For each
field, the side that sees the field as a function-body output is the LHS:

```
downstream__valid = upstream__valid    // both ports' `valid` is `out`;
                                       //   downstream is fn-out, upstream is fn-in
downstream__data  = upstream__data     // same shape
upstream__ready   = downstream__ready  // both ports' `ready` is `in`;
                                       //   upstream is fn-out, downstream is fn-in
```

Emitted SV:

```systemverilog
module connectStream (
    input  logic       clk,
    input  logic       upstream__valid,
    input  logic [7:0] upstream__data,
    output logic       upstream__ready,
    output logic       downstream__valid,
    output logic [7:0] downstream__data,
    input  logic       downstream__ready
);
    assign downstream__valid = upstream__valid;
    assign downstream__data  = upstream__data;
    assign upstream__ready   = downstream__ready;
endmodule
```

### Worked example 2 — `packet_struct.plr`

Source (parameter renamed to `inp` and local renamed to `held` so neither
collides with a SV reserved word; the negative case lives in
`fail-examples/sv-reserved-word.plr`):

```polar
struct Packet = packet { valid: bool, payload: uint(8) }

fn registerPacket
  { dom clk: Clock, rstn: Reset @clk = high }
  ( inp: Packet @clk )
  -> Packet @clk
  {
    let held = inp.reg(rstn, packet { valid: false, payload: 0 });
    return held;
  }
```

After flattening:

- `inp: Packet @clk` becomes `inp__valid: bool @clk`, `inp__payload: uint(8) @clk`.
- Return `Packet @clk` becomes synthetic `out result__valid: bool @clk`,
  `out result__payload: uint(8) @clk`.
- `let held = inp.reg(rstn, packet { ... })` splits per field, with
  `split` extracting `false`/`0` from the record constructor's slots:
  ```
  let held__valid   = inp__valid.reg(rstn, false)
  let held__payload = inp__payload.reg(rstn, 0)
  ```
- `return held` splits into two equations driving the result params, plus
  a void return:
  ```
  result__valid   = held__valid
  result__payload = held__payload
  ```

Emitted SV:

```systemverilog
module registerPacket (
    input  logic       clk,
    input  logic       rstn,
    input  logic       inp__valid,
    input  logic [7:0] inp__payload,
    output logic       result__valid,
    output logic [7:0] result__payload
);
    logic       held__valid;
    logic [7:0] held__payload;

    always_ff @(posedge clk) begin
        if (!rstn) held__valid <= 1'b0;
        else       held__valid <= inp__valid;
    end

    always_ff @(posedge clk) begin
        if (!rstn) held__payload <= '0;
        else       held__payload <= inp__payload;
    end

    assign result__valid   = held__valid;
    assign result__payload = held__payload;
endmodule
```

### Worked example 3 — hypothetical: nested aggregate

To check the algorithm generalises, consider:

```polar
struct Inner = inner { a: bool, b: uint(4) }
port  Outer { dom clk: Clock } = outer {
  out wrapped: Inner @clk,
  in  ack: bool @clk,
}

fn passthrough { dom clk: Clock } ( p: Outer @clk, out q: Outer @clk ) {
  q = p;
}
```

Expansion is recursive. `Outer` has two fields, one of which is a struct.
Expanding `p`:

| local            | type    | function-body direction |
|---|---|---|
| `p__wrapped__a`  | bool    | input  (Outer.wrapped is `out`) |
| `p__wrapped__b`  | uint(4) | input  |
| `p__ack`         | bool    | output (Outer.ack is `in`)      |

`q` (with `out` param keyword) flips directions for all three. The equation
`q = p` splits into:

```
q__wrapped__a = p__wrapped__a
q__wrapped__b = p__wrapped__b
p__ack        = q__ack
```

The same algorithm, with `split` recursing into `Inner`, produces this with
no special-casing.

### Worked example 4 — `accumulator.plr` (no aggregates; sanity)

Source has no ports or structs. The flattening pass is the identity for the
function body. The signature is also unchanged: each parameter is already a
value type.

```polar
fn accumulator
  { dom clk: Clock, rstn: Reset @clk = high }
  ( data: uint(8) @clk )
  -> uint(8) @clk
  {
    var acc: uint(8) @clk = (acc + data).reg(rstn, 0);
    return acc;
  }
```

The synthetic `out result: uint(8) @clk` is only introduced if the return is
aggregate-typed; for a scalar return, the function keeps its return type and
the SV emitter introduces a single `output logic [7:0] result` port from the
scalar.

(The convention of using `result` as the SV port name for an anonymous return
applies uniformly — aggregate and scalar.)

Emitted SV:

```systemverilog
module accumulator (
    input  logic       clk,
    input  logic       rstn,
    input  logic [7:0] data,
    output logic [7:0] result
);
    logic [7:0] acc;

    always_ff @(posedge clk) begin
        if (!rstn) acc <= '0;
        else       acc <= acc + data;
    end

    assign result = acc;
endmodule
```

### Worked example 5 — `counter.plr` (`param` binding)

Source:

```polar
fn counter
  { dom clk: Clock, rstn: Reset @clk = high }
  ( param bits: usize )
  -> uint(bits) @clk
  { … }
```

`param bits: usize` does not flatten (it's not an aggregate) and is not
monomorphised. It becomes a SV `parameter int`:

```systemverilog
module counter #(parameter int bits = 0) (
    input  logic                clk,
    input  logic                rstn,
    output logic [bits-1:0]     result
);
    logic [bits-1:0] count;

    always_ff @(posedge clk) begin
        if (!rstn) count <= '0;
        else       count <= count + 1;
    end

    assign result = count;
endmodule
```

The width expression `uint(bits)` lowers to `logic [bits-1:0]`. No const-eval
needed at Polar time — SV's elaborator handles it per instantiation.

### Re-checks after flattening

Because the output is still HIR, the existing passes can re-run as sanity
checks:

- **typeck** — should produce zero errors on flattened HIR (it just walks
  value types and domains). If it complains, the flattening pass has a bug.
- **single-driver check** — should pass: each aggregate equation that
  contributed multiple driver counts in the original now contributes the same
  per-field counts in the flat form. A struct constructor used as an
  initialiser correctly counts once per field. (This is where the re-check
  earns its keep — driver-count bugs in `split` would surface here.)
- **direction check** — confirms each flat equation has its function-body
  output on the LHS, catching any field-pairing mistakes in the flattening
  algorithm.

These re-checks are not currently mandatory in the CLI pipeline, but the
flattening pass's test harness should exercise them on every example.

## SV IR shape (sketch)

The IR sits between flattened HIR and emitted text. It's intentionally
shallow.

```rust
pub struct SvFile {
    pub modules: Vec<SvModule>,
}

pub struct SvModule {
    pub name: String,
    pub parameters: Vec<SvParameter>,   // const params
    pub ports: Vec<SvPort>,             // flattened
    pub items: Vec<SvItem>,
}

pub enum SvItem {
    Logic(SvLogicDecl),                 // logic [W-1:0] name;
    Assign { lhs: SvExpr, rhs: SvExpr },
    AlwaysFf {
        clock: SvSensitivity,           // posedge clk (no reset edge — sync reset)
        reset: Option<SvResetClause>,   // active-low rstn for first pass
        body: Vec<SvSeqStmt>,
    },
    // Future: Instance(SvInstance) once user-fn calls land.
}

pub enum SvExpr {
    Ident(String),
    Lit(String),                        // emitted as-is (e.g. "8'd0", "'0")
    BinOp(BinOp, Box<SvExpr>, Box<SvExpr>),
    Index(Box<SvExpr>, Box<SvExpr>),    // a[i]
    Slice(Box<SvExpr>, Box<SvExpr>, Box<SvExpr>),  // a[hi:lo]
}

// …reset/sensitivity helpers
```

The emitter is a deterministic pretty-printer with stable line breaks.

## First-pass scope

In scope:

- Every example in `examples/` produces a SV file that elaborates under
  verilator or slang and behaves identically to the Polar source (verified
  by either a small simulation harness or visual inspection of the diff,
  pending verifier infrastructure).
- `param` bindings lower to SV `parameter`.
- Aggregate flattening (ports and structs) at all positions: params, locals,
  returns, register operands, struct constructors.
- `reg` lowers to inline `always_ff` with synchronous active-low reset.
- Width arithmetic in types (`uint(N)`, `uint(N+1)`) emits SV `logic [N-1:0]`,
  `logic [N+1-1:0]`.
- CLI accepts `--out <dir>` (default `./sv/`) and writes one SV file per
  Polar input.

Out of scope (deferred):

- User-function instantiation. None of the first-pass examples instantiate
  another user function. The natural extension is a new `HirInstance` shape
  introduced by a post-flattening lowering step, emitted as a SV module
  instance with named port connections.
- Specialisation (port-type and HOF parameters).
- Connection-block syntax (`=>`, `=`). Not in current HIR lowering.
- Asynchronous reset, alternate reset polarity (`high`/`low` literal is
  captured but ignored by the emitter).
- Negative-edge / dual-edge clocking, frequency relationships.
- WidthEq obligation discharge at Polar-level for parameter expressions;
  rely on the SV elaborator for now, surface its errors back to the user.

## Implementation order

Assuming the three prereq passes (single-driver, width const-eval, domain
finalisation) land first:

1. **flatten_aggregates** in `src/hir/flatten.rs`. Tests assert HIR' contains
   no `Port`/`Struct` value types and no aggregate-typed equations/lets.
2. **sv_ir** in `src/sv_ir.rs`. Just the data types and a `Display` for
   debugging.
3. **sv_lower** in `src/sv_lower.rs`. Walks HIR' and builds SV IR. Tests
   compare structured SV IR for each example.
4. **sv_emit** in `src/sv_emit.rs`. Text emission. Tests snapshot the SV
   output for each example. Includes the SV reserved-word check (raises a
   diagnostic with the offending identifier and its source span).
5. **CLI** — `--out <dir>` argument (default `./sv/`), `--emit sv|cst`
   selector. The output directory is created if missing.
6. **Verification** — pipe each example's SV output through verilator/slang
   in CI to catch elaboration regressions.
(Source clean-up — renaming `input`/`output` in `examples/packet_struct.plr`
and `examples/shift_register.plr` so they reach SV emission — was done up
front. The reserved-word negative case lives in
`fail-examples/sv-reserved-word.plr`.)

## Open questions / future work

- Naming for the synthetic return port. `result` is the chosen default;
  surface syntax for naming the return (`fn f() -> p: Packet`) is a
  reasonable future extension.
- How to surface SV elaborator errors back to the user with Polar source
  spans. The lowering pass already has all the span info; threading it
  through the SV output (as comments and/or sourcemap) is straightforward
  but unscoped here.
- Whether the re-checks on flattened HIR run by default or only in tests.
  Recommend: tests only, to keep CLI fast; switch on with a `--verify`
  flag.
- Configurable reset polarity and async vs sync reset. The `high`/`low`
  literal is captured but ignored; a follow-up design pass should decide how
  these surface in HIR and SV.
