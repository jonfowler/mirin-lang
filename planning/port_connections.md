# Port connections

This document covers the syntax and semantics of component connection blocks — how fields are wired when instantiating a component.

## Source and sink

Every field in a connection block is either a **sink** or a **source**:

- A **sink** is an input to the component. The component reads from it. You supply an expression.
- A **source** is an output from the component. The component drives it. You supply a binding name.

The operator encodes the direction:

```
component {
  in_dat = x * 4,    // sink: any expression on the RHS
  output => out_df,  // source: a name on the RHS
}();
```

Sinks use `=`. Sources use `=>`.

## The `in` and `out` keywords

The `in` and `out` direction keywords are **optional** in connection blocks. The operator already encodes direction:

- `=` implies sink (`in`)
- `=>` implies source (`out`)

The keywords can be added for clarity:

```
component {
  in  in_dat = x * 4,
  out output => out_df,
}();
```

or omitted entirely:

```
component {
  in_dat = x * 4,
  output => out_df,
}();
```

When present, `in`/`out` are checked for consistency with the field's declared direction but do not change the semantics.

This mirrors how `out` is used consistently elsewhere in the language: in port field declarations (`out valid: bool @clk`) and in component argument position (`out downstream: Stream8{clk}`).

## Name introduction with `=>`

`field => x` desugars to: insert `var x;` before the component statement, then connect to that `var`. All scoping behavior follows from this expansion:

- If `x` is **not yet declared**: `var x;` is inserted and `x` comes into scope as a block-scoped signal.
- If `x` is **already declared as `var`**: the connection binds to the existing signal. No new declaration is inserted.
- If `x` is **in scope as a `let`**: error — the inserted `var x;` would shadow a `let`, which is prohibited.

```
component { in_dat = x, output => out_df }();
// desugars to:
var out_df;
component { in_dat = x, output => out_df }();
```

Type is inferred from the port field when `var out_df` is introduced implicitly.

## Pre-declared names in structural feedback

When multiple components share the same wires, those signals must be pre-declared with `var` before any component is instantiated. The `=>` connection then binds to the existing signal rather than introducing a new one:

```rust
var vld, rdy : bool @clk;
var dat : uint[8] @clk;

const_df {
  vld       => vld,
  dat       => dat,
  rdy        = rdy,
  const_val  = 42,
}();

reg_df {
  in_vld  = vld,
  in_dat  = dat,
  in_rdy => rdy,
}();
```

`vld`, `dat`, and `rdy` are shared between both components — `const_df` drives `vld` and `dat` while `reg_df` drives `rdy`. The `var` declarations give those names block-wide scope so both instantiation blocks can reach them.

## Expressions are not valid on the source side

A source field binds to a name only. An expression on the RHS of `=>` is an error:

```
output => raw_df + 1   // error: expression not valid on source side
```

Transformations on a component output must be applied after capturing the name:

```
output => raw_df,
...
let out_df = raw_df + 1;
```

## Whole-port connections

`var` is still used directly when binding a whole port with bidirectional fields, since no single `=>` connection point exists:

```rust
var p: DF{clk};
```

`p` is a connection node carrying fields that flow in both directions. It is passed as a unit to components that accept or produce a `DF` port.

## Open questions

- Should `in`/`out` keywords be permitted to be used without their corresponding operator (e.g. `out field = name` as an alternative to `field => name`)? Or should the operator always be required when the keyword is present?
- Can `=>` be used outside of connection blocks in any other binding position, or is it strictly a connection-site operator?
- What is the rule when `=>` is used and the RHS name is already in scope as a `let` binding? A `let` binding is a value, not a signal node; connecting a component output to it should be a hard error, not a silent shadow.
- What happens when two `=>` connections in the same block bind to the same undeclared name? The second would implicitly introduce a second `loc` node with the same name, or silently share the first. This should be an error: each name may be the implicit target of at most one `=>` per block, and connecting a second `=>` to a pre-declared `loc` should also be a multiple-driver error.
- Is `output => p.valid` (a field-access path as the RHS of `=>`) legal? For the initial implementation the answer should be no: restrict `=>` RHS to bare identifiers. Field-path targets can be added once lvalue projection semantics are defined.
- Must all `in` fields of a port be connected when instantiating a component? Missing `in` connections should be a hard error unless a default value is declared on that field. Unbound `out` fields (no `=>` and no pre-declared `var`) should be a lint warning. This rule is not currently stated in the docs.

## Potential future feature: let-port-patterns

> This section describes a possible future extension. It is not part of the current design.

When a component call returns a port value, it may be natural to destructure it inline using a let-pattern that mirrors connection block syntax:

```rust
let df{ valid => vld, data => x, rdy = en1 && en2 } = v_df.reg_df();
```

This simultaneously:
- binds the `out` fields to names using `=>` (`valid` → `vld`, `data` → `x`)
- provides expressions for the `in` fields using `=` (`rdy` ← `en1 && en2`)

The `=`/`=>` distinction carries over from connection blocks consistently. The key differences from a connection block are:

- **Scope**: `=>` in a `let`-pattern introduces names with `let`-scoped (sequential, forward-only) lifetime, not `var`-scoped (block-wide) lifetime. The surrounding `let` determines the scope.
- **Hybrid nature**: ordinary destructuring is purely receptive. A let-port-pattern also satisfies requirements — the `=` clauses are not equality guards but value provisions for the port's input fields. This makes it semantically distinct from struct destructuring, which would only ever contain `=>` bindings.

The presence of any `=` clause in a port pattern signals that this is a port pattern rather than a plain struct destructuring.

One open question for this feature is error handling when an `in` field is omitted from the pattern — the compiler would need to require all `in` fields to be accounted for, since leaving one unwired is a hard error rather than a partial match.
