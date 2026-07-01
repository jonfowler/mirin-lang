# The `return` place (referrable result variable)

Status: landed — the `return` place, named single results (`-> (output: DF)`),
and named tuple-result parts (`-> (sum: T, carry: U)`). See "Named results".

## What it is

`return` is a referrable place: the function's result binding, a var-like
signal node of the declared return type. The body can drive its leaves and read
its `in` leaves, instead of (or alongside) building the whole result in a tail.

```mirin
fn reg_fwd(self @clk, rst: Reset @clk) -> DF @clk {
    var en: bool @clk;
    ...
    en = !reg_vld || return.ready;   // read the returned port's `in` leaf
    return.valid = reg_vld;          // drive an `out` leaf by place
    return.data  = reg_data;
}
```

This is the dual of `self`: where `self`'s leaves are the module's *parameter*
ports, `return`'s leaves are its *result* ports. A returned port's `out` fields
are module outputs the body drives; its `in` fields (the downstream's
backpressure) are module inputs the body reads — folded exactly as for an `out`
port parameter (planning/tuples.md, dataflow_stage.mrn).

## Model — `return` is MIR's `_0`

In rustc's MIR the return value is local `_0`: a real place the body assigns to
and reads from, and the `return` terminator yields whatever is in it. `return`
here is the same idea. There is no new IR:

- **Lowering (body.rs).** When a fn has a return type, a synthetic local named
  `return` (kind `Var`, declared type = the return type) is allocated in the
  base scope, visible block-wide. The name `return` is reserved, so it can
  never collide with a user local. `return.f` lowers to `Field { Local(return),
  f }`; bare `return` to `Local(return)`. `return EXPR;` and the top-block tail
  desugar to a whole-result equation `return = EXPR` — so a body that mixes a
  tail/`return` with `return.f = …` is one consistent equation system. (A unit
  fn has no result place; its tail/`return` stays a side-effecting call.)
- **infer.rs.** The result local is seeded with the *same* freshened return
  type the body is checked against, so a returned value's domain and the result
  port's domain are one variable. A whole-result drive (`return = EXPR`) joins
  like a return (`merge_branch`) — domains JOIN, aggregates check invariantly —
  exactly as the old `Stmt::Return` did; a per-leaf `return.f = …` is an
  ordinary equation (coerces at the leaf).
- **Driver checks (check.rs).** The result place is a `Var`, so the usual
  single-assignment and completeness checks apply: per-leaf drives must not
  overlap, and a field-driven struct/scalar result must cover every leaf
  (`field b of return is never driven`). A partially field-driven *port* result
  is exempt from completeness — its owed set depends on direction folding (the
  documented port gap, same as `self`/out-params).
- **Backend (lower.rs).** The result local emits as the `result` ports — its
  base SV name is `result`, not `return` (an SV reserved word; a scalar return
  would otherwise be the invalid bare port `return`). It declares no nets of
  its own (the result ports already come from the signature). So `return.valid
  = x` lowers to `assign result__valid = x`, and `return.ready` reads the
  `result__ready` input port. `drive_result` remains for unit fns only.
- **const_eval.rs / tests.** A const fn's return value is found at the
  whole-result equation as well as at a bare tail/`Stmt::Return`.

## Generated SV

An unnamed `return` emits under base `result` (`result__valid`, … / bare
`result` for a scalar) — an *escape* of the reserved keyword `return`.
`return_place.mrn` is `df_example.mrn` rewritten in the place style and emits
the identical module (it even drops the intermediate backpressure wire —
`return.ready` is read directly). Named results use their bound names instead
(see below).

## Named results

The result(s) can be named in the signature; the name(s) become the referrable
result place(s), and the `return` keyword does not exist then.

```mirin
fn reg_fwd(self @clk, rst: Reset @clk) -> (output: DF @clk) {
    output.valid = reg_vld;            // drive a part by leaf
    let en = !reg_vld || output.ready; // read an in-leaf
}
fn add_flag(x: uint(8), y: uint(8)) -> (sum: uint(8), carry: bool) {
    sum = x + y;                       // → port `sum`
    carry = x == y;                    // → port `carry`
}
```

- **One model, via `Signature.result_places`.** `sig_of` always produces a
  list of result places: `[{name:"return", ty, sv_base:"result"}]` for a normal
  return, the named place(s) for a named return. A single named result names
  the whole result; two or more name the parts of a tuple result (a
  `Type::Tuple` return type). `body.rs` allocates one var-like local per place;
  the unnamed one is literally named `return`, so the keyword resolves only when
  unnamed.
- **Named results surface in the SV, the keyword is escaped.** Only the
  unnamed `return` is escaped (to base `result`, since `return` is an SV
  reserved word); a named result/tuple part uses its **bound name** as the port
  base — `output` → `output__valid`, `sum` → `sum` (decided with Jon). A
  `LocalData.result_base` (= the `ResultPlace.sv_base`) carries it; the backend
  keys on it for both the module's own ports and the connection to a callee's
  result ports (`local_name`, `is_result_local`, `emit_instance`), never on the
  source name. A bound name that collides with an SV reserved word is the
  user's to fix (the `reserved_words` pass catches it).
- **The whole-result drive** (`return EXPR;`/tail, or `output = EXPR`) targets
  the *sole* result place — present for an unnamed or single-named result,
  absent for a multi-part named return (drive the parts). `return` itself is
  undefined under a named result
  (`fail-expected/return-when-named.mrn`).

`examples/working/named_result.mrn` covers all three forms.
