# Known issues and design risks

This document records pre-implementation design problems found during a review of
the planned Polar surface syntax. Items are grouped by severity.

---

## Serious issues — need resolution before implementation

### 1. `fn` context disambiguation — resolved

`fn` is used uniformly at the top level and inside `impl` bodies. There is no deep distinction between a component and a function: a component has temporal semantics (it does not necessarily produce a direct result), whereas a function is pure and compositional. `impl` methods are syntactic sugar for functions with an explicit `self` argument.

The parser already distinguishes the two forms by context: `_item` vs `impl_body`. This is not a grammatical ambiguity. The `self` parameter naturally identifies a method body without requiring a special rule.

**Status: not an issue.** The grammar uses `fn` for both forms and context is sufficient.

### 2. `=>` desugaring rule — needs doc update

The scoping rule for `=>` follows from a simple desugaring: `field => x` in a connection block is syntactic sugar for inserting `var x;` before the component statement, then binding to that `var`. All cases flow from this expansion:

- **`x` not yet declared:** `var x;` is inserted; `x` comes into scope as a block-scoped signal.
- **`x` already declared as `var`:** the connection binds to the existing `var` — no new declaration is inserted.
- **`x` currently in scope as a `let` binding:** error. A `let` binding is a value, not a signal node. The desugaring would insert `var x;` which conflicts with the existing `let x` under the rule that `var` cannot shadow `let`.

The docs (`port_connections.md`, `cycles_and_scoping.md`) need to state the desugaring rule explicitly and use it as the source of truth for all `=>` scoping behavior. The current docs describe the cases correctly but do not state the unifying desugaring rule.

### 3. `var` declared inside `if`/`match` — scope is unresolved

`cycles_and_scoping.md` does not say whether `var` can appear inside a
conditional branch. This matters because `var` participates in an equation
system. An equation for a `var` that only exists in one branch of a conditional
is not structurally well-formed in RTL — hardware signals do not conditionally
exist.

Two plausible positions:

a. `var` is illegal inside `if`/`match` bodies. It may only appear at the top
   level of a component or `rec` block. This is the safe position and matches
   how Verilog `wire` declarations work.

b. A `var` inside a conditional is hoisted to the enclosing block, and the
   equation is guarded by the condition. This is what SystemVerilog allows with
   `always_comb` variables.

Neither is currently specified. The language needs an explicit rule before any
implementation of `if`/`match` can proceed.

### 4. Multiple drivers via `=>` — no stated conflict rule

`port_connections.md` establishes that `=>` binds a component output to a name.
For structural feedback, multiple components may each drive a different signal in
the same `var`-declared set. But the doc does not state what happens if two
`=>` connections in the same block both bind to the same name:

```
comp_a { output => x }();
comp_b { output => x }();   // is this a double-drive error or a redefinition?
```

If the second `=>` silently introduces a new `var` that shadows the first, `x`
at that point no longer refers to `comp_a`'s output — which is confusing. If
both bind to the same signal node, that is a multiple-driver error.

The rule should be: a `=>` that introduces an implicit `var` is legal only once
per name per block. A second `=>` to the same undeclared name is an error.
Connecting a second `=>` to a *pre-declared* `var` should also be an error
(multiple drivers). This rule needs to be stated explicitly.

### 5. Direction checking when `in`/`out` are elided requires type information at parse/name-resolution time

`port_connections.md` says `in`/`out` keywords are optional because `=` implies
sink and `=>` implies source. However, the compiler still needs to verify that
the field name on the LHS of `=` actually corresponds to an `in` field of the
component being instantiated, and the field name on the LHS of `=>` corresponds
to an `out` field.

This check requires knowing the type of the component being called — it cannot
be done during parsing or basic name resolution without the type definition in
scope. For a simple single-file program this is fine. For a component defined in
another module, the field directions come from its port type.

No current planning doc describes the order in which component types are resolved
relative to connection-block checking. If connection checking happens before type
resolution is complete (e.g. in a top-down single-pass elaboration), direction
checking may silently pass on an unresolved call and produce a wrong-direction
error only during code generation.

Recommend adding an explicit note to `compiler_architecture.md` that direction
checking in connection blocks is a type-check-phase responsibility, not a
parse-phase responsibility.

---

## Moderate issues — need a decision, may not block first pass

### 6. `var` with no equation — what is the error?

`cycles_and_scoping.md` shows the counter pattern where `var count;` is followed
by `count = ...;`. It does not specify what happens if the equation is never
written:

```
var x: uint[8] @clk;
// no assignment to x anywhere in the block
return x;
```

In RTL terms `x` is an undriven wire — a synthesis error if passed to output.
The compiler should report this. The question is *when*: at elaboration (as a
completeness check on the equation system) or during type/clock checking.

Given that `var` is intended precisely as a signal node in an equation system,
checking completeness at the end of name resolution or elaboration is more useful
than waiting until RTL lowering. Flag as a required elaboration-phase check.

### 7. `var` cannot shadow `let` — but the rule is hard to enforce without a two-pass scope

`cycles_and_scoping.md` states: "`var` cannot shadow an earlier `let` binding in
the same block." This is a good rule but it creates a practical constraint: the
name resolver must have already seen the `let` binding at the point it processes
the `var` declaration.

Since `var` has *block-wide* scope and `let` has *forward-only* scope, a single
forward pass through the block is sufficient to detect `var`-after-`let` for the
same name. However, the combination interacts with `=>` implicit introduction: if
`output => x` implicitly introduces `var x`, and `x` was previously bound by
`let`, this is the same prohibited pattern. The name resolver must treat implicit
`var` introduction via `=>` under the same rule.

### 8. `var` inside `impl` method bodies — unresolved

`cycles_and_scoping.md` lists this as an open question. It needs a decision
before `impl` bodies are semantically checked.

Arguments for allowing it: methods on port types (e.g. `Stream8::connect`) may
need to introduce local signal names. Arguments against: methods lower to
ordinary functions with `self` as an argument; within the lowered form the
cyclic signal semantics of `var` do not make sense if there is no enclosing
component body to attach the signal to.

The safest initial rule: `var` is only legal in component bodies, not in `impl`
method bodies. Methods that need stateful local signals
should be expressed as sub-components. This can be relaxed later.

### 9. `#clk` inference through cyclic `var` equations

`syntax.md` notes that `#clk` inference is not yet specified. For the common
pattern:

```
var count;
count = (count + 1).reg{rstn}(0);
```

the type of `count` must be inferred from the equation. The equation contains
`.reg{rstn}()` which should establish the clock domain. But the equation also
references `count` itself — so inference has to traverse a cycle.

If inference is done as constraint solving this is fine (the register introduces
a constraint that links `count`'s clock domain to `rstn`'s clock domain). If
inference is done as a simple forward walk it will fail to see the clock
constraint on the first pass over `count`.

This is not a blocker for the first parser slice, but it must be resolved before
`#clk` inference is implemented. The elaboration spec should note that clock
inference for `var` bindings requires a fixpoint pass, not a single forward
traversal.

### 10. `inline fn` — `inline` as a keyword vs identifier

The planning docs mention `inline fn` as a modifier form. `inline` is currently
treated as a plain identifier by the grammar (the `identifier` rule is
`/[A-Za-z_][A-Za-z0-9_]*/`). If `inline` is added as a keyword it will become
reserved, which may silently break any code that uses `inline` as a signal or
variable name.

The word: `($) => $.identifier` declaration in tree-sitter means `inline` would
become a reserved keyword competing with the general identifier rule. In
tree-sitter, the `word` rule controls which keyword is treated as the "main"
identifier-like terminal for error recovery purposes; adding `inline` as a
keyword does not require changing `word`, but it does require reserving the
string.

If `inline` is made a keyword before any code exists that uses it as an
identifier the breakage is low. Do it early, or choose a less collision-prone
spelling such as `#[inline]` attribute syntax.

### 11. Port connection completeness — `in` fields vs `out` fields

`port_connections.md` and the other connection docs do not state whether all `in`
fields must be connected when instantiating a component. The footnote in the
"future: let-port-patterns" section mentions that leaving an `in` field unwired
is "a hard error" in the pattern context, but no equivalent rule is stated for
ordinary connection blocks.

For RTL correctness, unconnected `in` fields would produce undriven inputs —
likely a synthesis warning or error. The compiler should require that all `in`
fields are connected unless a default value is provided in the port declaration.

`out` fields that are not bound (i.e. no `=>` connection and no pre-declared
`var`) are less problematic: the output is simply discarded. Whether this should
be a warning (unused output) or silently allowed needs a policy decision.

Recommend: missing `in` field connection is an error; unbound `out` field is a
lint warning.

---

## Minor notes

### 12. `=>` RHS must be a bare name — not a field access

`port_connections.md` correctly states that `output => raw_df + 1` is an error.
What the doc does not cover is whether `output => p.valid` (a field access) is
legal.

If `p` is a pre-declared `var` of a port type, binding a component output
directly to a field of `p` would be useful. However, it complicates the
"introduce a new `var`" shorthand because `p.valid` is not a bare name — it is
an lvalue projection. The grammar would need to distinguish bare names from field
paths on the RHS of `=>`.

For the initial implementation, restrict `=>` RHS to bare identifiers only.
Field-path targets on `=>` can be added later once the lvalue semantics are
defined.

### 13. `var` duplicate declarations in the same block

`cycles_and_scoping.md` states "`var` cannot shadow `let`" but does not state
whether two `var` declarations with the same name in the same block are
permitted.

```
var x: uint[8] @clk;
var x: bool @clk;   // redeclaration — error or shadow?
```

Based on the principle that `var` declares a signal node for the equation system,
two `var` declarations with the same name in the same block should be an error
(unlike `let`, which is explicitly designed for shadowing/rebinding). Add this
rule explicitly.

### 14. `let` shadowing `var` — potential confusion with equation system

`cycles_and_scoping.md` documents this correctly: once a `var count` is shadowed
by `let count = count + offset`, the equation on the original `var` is still
live. The feedback signal still exists; only the name is shadowed.

There is no double-drive risk here because `let` is a forward-only value binding
and cannot appear on the LHS of an equation. The shadowing is safe by
construction. However the docs should clarify that the equation system is indexed
by the *signal node identity*, not by the name — so shadow-then-equation is
impossible, not just detected.

### 15. Structural feedback — clock domain mismatch detection point

When two components share `var`-declared wires and are in different clock
domains, the clock domain mismatch is caught during type checking when the port
field types are checked against the `var` type annotation. The `var` declaration
forces an explicit type (or an inferred one), and the clock domain is part of
that type.

The detection point is therefore: at the connection site, when the type of the
port field (carrying its clock domain) is unified with the type of the `var`
binding. This is the right place. No separate rule is needed, but the type
checker must ensure that the `@clk` annotation on a `var` is checked against
every connection that references it, not just the first one.

---

## Summary of items needing doc updates

| Issue                                   | Target doc                                     | Action                                                        |
| --------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------- |
| `fn` keyword transition                 | `syntax.md`, `compiler_architecture.md`        | Add note about `cmp` → `fn` migration and parser context rule |
| `=>` with pre-existing `let` binding    | `port_connections.md`, `cycles_and_scoping.md` | Add explicit error rule                                       |
| `var` in conditional branches           | `cycles_and_scoping.md`                        | Add explicit legality rule                                    |
| Multiple `=>` to same name              | `port_connections.md`                          | Add multiple-driver rule                                      |
| Direction checking is type-phase work   | `compiler_architecture.md`                     | Add note to stage 5                                           |
| Undriven `var`                          | `cycles_and_scoping.md`                        | Add completeness rule                                         |
| `var` in `impl` methods                 | `cycles_and_scoping.md`, `impl.md`             | Resolve open question                                         |
| Clock inference through cyclic `var`    | `syntax.md`, `compiler_architecture.md`        | Note fixpoint requirement                                     |
| `inline` keyword reservation            | `syntax.md`                                    | Add note                                                      |
| Connection completeness for `in` fields | `port_connections.md`                          | Add explicit rule                                             |
| `=>` RHS field access                   | `port_connections.md`                          | Restrict to bare names for now                                |
| Duplicate `var` declarations            | `cycles_and_scoping.md`                        | Add error rule                                                |
