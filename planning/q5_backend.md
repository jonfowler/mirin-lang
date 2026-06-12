# Q5 plan — the back end: monomorphise → flatten → Verilog

Q5 takes the typed HIR (Q3/Q4) all the way to SystemVerilog, ports the old
compiler's back-end passes onto queries, and — at parity — switches the CLI to
`polar-db` and retires `polar-compiler`. This is the largest phase (≈ all of
Q2+Q3 combined), so it is planned as a *de-risking sequence*, not one drop.

Grounded in `ir_pipeline.md` (the back-end pass table), `parametricity.md`
(monomorphisation split), `query_engine.md` §3.2/§4, and the old
`hirtl/*` + `svir/*`.

## 0. Load-bearing facts

- **The one deferred front-end piece is a Q5 prerequisite: call connections.**
  Module instantiation is **a `fn` call** whose callee has `out` params, wired by
  the call's named-argument section and out-arguments — there is no separate
  "port literal" syntax. From `delay.plr`:
  ```polar
  double_delay_named{rstn, downstream => ds}(upstream);   // named section: shorthand `rstn`, out-conn `downstream => ds`
  double_delay_pos{rstn}(ds, out => downstream);           // positional out-arg `out => downstream`
  ```
  `body` lowering currently drops the named-arg section and out-args to
  `Missing`, so these models carry `Unsupported` diagnostics today (they are the
  *non-clean* examples — `delay`). Nothing is silently mis-checked: a `fn` with
  an `out` param driven by a plain equation (`downstream = upstream;`, as in
  `simple_port`) already works; only the **out-arg call** form is missing. And
  `check_directions` (deferred from Q3e) checks exactly these connection
  operators. So Q5 **begins** by lowering call named-arg sections + out-args and
  adding `directions(def)`.
- **Monomorphisation is deferred to a later slice.** The `parametric_*` examples
  are **out of near-term Q5 scope**; build the back end for the non-generic case
  first and revisit Type-kind monomorphisation once the rest emits Verilog. (The
  `MonoInstance` design below is recorded for then, not built now.)
- **The driver is demand-pull from roots.** "Force `verilog` for each top-level
  item." The compiler should take an **optional top-entity argument** (default:
  every top-level item). Which exact set of instances gets emitted is not
  critical to pin down precisely now.
- **Parity is the exit condition, loosely.** `polar-db`'s emitted `.sv` should be
  *equivalent* to `polar-compiler`'s for the working corpus before the old crate
  is retired — exact byte-match is not the priority. The example harness
  (`tests/examples.rs`) is where this is asserted.

## 1. rustc analogy

- **Monomorphisation collector / shimming** (`rustc_monomorphize`): walk from
  roots, collect `MonoItem`s (`Instance = (DefId, GenericArgs)`), dedup by
  interned instance, codegen each once. Polar: the same, with `MonoInstance`
  interned like `DefId`. Type-kind args monomorphise; const generics stay
  polymorphic (rustc's const generics do too).
- **MIR → codegen** ≈ flatten HIR → SV IR → text. The late tree→statement
  flattening (`if`/`when` → statement form with synthetic locals) is rustc's
  MIR-building shape — a tree survives type-checking, a late pass introduces
  basic-block-ish control flow.
- **"Codegen for each reachable instance"** = the `verilog`-forcing driver.

## 2. Passes → queries

The old pipeline (`ir_pipeline.md`), each as a per-def / per-instance query whose
body is largely today's code, narrowed to one key. (`HirInstance` = the input —
either a `DefId` for a non-generic def or a `MonoInstance` for a specialised one.)

| Old pass (`hirtl`/`svir`)                  | Query                                           | Key                      | Notes                                                                                     |
| ------------------------------------------ | ----------------------------------------------- | ------------------------ | ----------------------------------------------------------------------------------------- |
| (front-end gap) named-arg/out-arg lowering | folded into `body`                              | def                      | **prerequisite**; `Missing` today                                                         |
| (front-end gap) `check_directions`         | `directions(def)`                               | def                      | body + sig, no types (Q3e deferral)                                                       |
| `monomorphise` *(deferred — later)*        | `mono_instances(crate)` + `mono_body(instance)` | crate / (def, type-args) | collector + per-instance specialised body; **out of near-term scope** (§0)                |
| `lower_block_expressions`                  | folded into a `thir`/`lowered_body(instance)`   | instance                 | `if`/`when`/block → statement form + synthetic locals                                     |
| `lower_method_calls`                       | same lowered-body query                         | instance                 | `MethodCall` → `Call` via `infer`'s `method_resolutions`                                  |
| `desugar_user_calls` (out-args)            | same lowered-body query                         | instance                 | user calls → expr-statement + out-arg leaves; reads callee `sig_of`                       |
| `flatten_aggregates`                       | `flat_body(instance)`                           | instance                 | erase port/struct types → per-field locals; substitute Const/Domain args; domain stamping |
| `lower_to_sv`                              | `sv_module(instance)`                           | instance                 | flattened body → one `SvModule` (interfaces of submodules only)                           |
| `emit_sv`                                  | `verilog(crate)`                                | crate                    | concatenate `sv_module`s; whole-crate reserved-word check + deterministic print           |

The intermediate per-instance lowering steps (block/method/out-arg) may collapse
into one `lowered_body(instance)` query rather than three, since they are
sequential rewrites with no new external dependency — decide when porting.

## 3. SV IR + emit

Port `svir/ir.rs` faithfully (it is already shallow and query-friendly): `SvFile`
of `SvModule { parameters, ports, items }`, items ∈ `Logic | Assign | AlwaysFf |
AlwaysComb | Instance`, `SvExpr`/`SvBinOp`, and the deterministic pretty-printer
(`emit_sv`) including the reserved-word hard-error. `SvModule` carries no `'db`
(it's post-lowering, names resolved to strings), so it is a clean salsa value.

## 4. Prerequisites (Q5a)

Before any back-end pass, finish lowering the **call-connection** forms `body`
currently drops to `Missing`:
- **Named-argument sections on calls** (`f{ name = value, name => target, name }`):
  match each against the callee's named params (a `dom`/value arg, or an out-arg
  connection). Lower to a real HIR call-with-connections node.
- **Out-arguments** — named (`downstream => ds`) and positional (`out => target`
  / `=> target`): the source-arrow that binds a caller local to an `out`
  param/field. Lower the binding leaf so out-arg desugaring (Q5c) and flatten can
  route it. A user-fn call with `out` connections is what becomes an SV
  `Instance`.
- **`directions(def)`**: now buildable (the `=`/`=>` connection operators exist
  in the body) — checks operator vs. port-field/param direction, finishing the
  Q3e check set.

## 5. Sub-slices (de-risking order)

A thin vertical slice first — prove the whole query→Verilog pipeline on the
simplest programs — then widen to instantiation, then parametrics.

- **Q5a — finish body lowering + `directions`.** Named-arg/out-arg connections +
  the direction check. Unblocks instantiation and closes Q3e. (No SV yet.)
- **Q5b — SV IR + a vertical slice. _(done)_** Ported `svir/ir.rs` (combinational
  subset); added `sv_module(def)` + `verilog(crate)` for the **non-generic,
  non-aggregate, single-fn** scalar-combinational case. Driver forces `verilog`.
  Byte-parity with `polar-compiler` on `add_constant`.
- **Q5c — registers + control flow. _(done)_** Extended the SV IR with
  `AlwaysFf`/`AlwaysComb` and lowered the clocked + control-flow forms directly
  in `backend/lower.rs` (no separate `lowered_body` query yet): `e.reg(rst,init)`
  → `always_ff` with reset (bound local is the register); `when ev { d }` →
  reset-less `always_ff` (synthetic `__block_N`); `if c {a} else {b}` →
  `always_comb`. Register clock derived from the value's domain. Shadowed `let`s
  uniquified. Byte-parity with `polar-compiler` on `when_counter`/`pipeline`/
  `shift_register`/`if_expression` modulo `__block_N` numbering; all verilator-
  clean. (The `lowered_body`/out-arg desugar + method→module split move to Q5d
  alongside instantiation, where they first matter.)
- **Q5d — flatten + instantiation. _(done)_** Done directly in `backend/lower.rs`
  (no separate `flat_body` query): struct/port params, return, and locals erase
  to per-field scalar leaves (`base__field`) via `flatten_leaves` — field access
  projects, record literals rebuild, aggregate `.reg` emits one `always_ff` per
  field, and a port equation becomes one connection per field (sink chosen by the
  leaf's module direction). A user `fn`/method call → an `SvInstance` (positional
  match `[receiver?] ++ args`, named match the named section, `out`-args bind
  callee `out` params to caller places, return → binding / `result` / a fresh
  `__call_N`); methods qualify their module name by owner (`Option__reg`). The
  driver emits every `fn`/method crate-wide (modules erased), prelude excluded.
  Byte-equivalent to `polar-compiler` on `packet_struct`/`simple_port`/`delay`/
  `delay_impl`/`multi_call`/`use_across_modules`/`pub_use_reexport` (modulo
  synthetic `__call_N`/`__block_N` numbering and module ordering); all
  verilator-clean. **Deferred to Q5-mono:** parametric type/width substitution
  (`parameterized_port`, `parametric_*`, `equal_width_fn`'s `#(parameter …)`).
- **Q5e — parity + CLI swap. _(done)_** Module emission switched to **source
  order** (across files by path, within a file by byte position) — the whole
  non-parametric corpus is now byte-identical to `polar-compiler` modulo only the
  synthetic `__call_N`/`__block_N` numbering (left as-is; semantically identical,
  loose-parity bar). New `polar-db` **bin** (`src/main.rs`): recursive FS loader
  (root + `mod foo;` files into the `Vfs`), runs the query stack, prints
  diagnostics (exit 1) / IO errors (exit 2), writes `verilog(crate)` to
  `<out>/<stem>.sv`; `--emit cst` debug aid. `polar-db` is now the primary CLI.
  Verification: `non_parametric_corpus_is_verilator_clean` lints the 15
  non-parametric examples with verilator (`-Wall` minus cosmetic/expected lints),
  gated on verilator being installed. **`polar-compiler` is kept as the parity
  oracle** (Jon's call) until Q5-mono lands — not retired yet.
- **Q5-mono — monomorphisation.** Done in three sub-slices in `backend/lower.rs`
  (no separate `mono_body` query needed for the non-fn-generic cases):
  - **Q5-mono-a/b _(done)_** — Const-kind generics → `#(parameter int N)` +
    `[N-1:0]` widths (`sv_type` resolves `Param(i)` via the def's generic names);
    `flatten_leaves` substitutes a struct/port's generic args into field types
    (`build_subst`/`subst_type` — positional args ↔ positional params), so
    `Bus(uint(8))`/`Buf{clk}(8)`/`DF{clk}(uint(8))` flatten with concrete widths.
    Parity on `parametric_width_fn`/`parametric_width_port`/`parametric_struct`/
    `parameterized_port`/`counter`.
  - **Q5-mono-c _(done)_** — `infer` records an undecidable `uint(n)`~`uint(m)`
    width equality as a `width_residual`; `sv_module` emits `initial assert
    (n == m)` (new `SvItem::InitialAssert`/`SvBinOp::Eq`). Parity on
    `equal_width_fn`. (First sliver of the deferred Q4b residual machinery.)
  - **Q5-mono-d _(done)_** — true **Type-kind fn monomorphisation**. Front-end
    fix: `param A: Type` now classifies as **Type-kind** (the `: Type` wins over
    the `param` keyword), which exposed and fixed an `infer` bug — `substitute`
    didn't recurse into struct/port *args*, so `Bus(A)` wasn't instantiated.
    Back end: `verilog` skips type-generic fns from direct emission; `build_module`
    takes a `self_subst` for the def's own generics; `emit_instance` binds a
    type-generic callee's Type params by matching the call's actual arg types
    (`match_type`), names the copy `Callee__Arg` (`mono_name`), records a
    `MonoReq`, and the driver emits one specialised module per unique instance
    (worklist, appended after the source-ordered concrete modules). Omitted
    defaulted params wire their default at the instance (`sig::Param` gained
    `default`; `default_value` renders `high`→`1'b1` etc.). `parametric_struct_
    extended` byte-identical to polar-compiler — **whole 22-example corpus now at
    parity** (non-parametric ones modulo synthetic `__call_N`/`__block_N`).

Each slice promotes more of `tests/examples.rs` from "runs" to "emits matching
Verilog".

## 6. Decisions to settle

1. **Verification of parity.** (a) **Diff against `polar-compiler`** — emit from
   both, assert byte-equal per example (direct "at parity" check while the oracle
   still exists); (b) **golden `.sv` files** checked in; (c) **verilator** on the
   new output. (a) is the most direct and self-updating against the oracle; (c)
   reuses the existing lint harness for semantic sanity. Likely (a) during the
   port, (c) retained after retirement.
2. **One `lowered_body` query or three?** Collapse block/method/out-arg into a
   single per-instance lowering query, vs. mirror the old three passes. Fewer
   query nodes vs. closer to the reference.
3. **`MonoInstance` representation.** Interned `(DefId, GenericArgs)` (like
   `DefId`); identity is the type-args. Confirm Const/Domain args are *excluded*
   from the mono key (they stay polymorphic — only Type-kind monomorphises).
4. **How far this session / does scope hold to parity-and-retire**, or stop at a
   vertical slice (Q5b/c) and leave parametrics + retirement for later.
