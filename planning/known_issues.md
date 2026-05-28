# Known issues and design risks

This document records pre-implementation design problems found during a review of
the planned Polar surface syntax. Items are grouped by severity.

---

## Resolved

### 1. `fn` context disambiguation — not an issue

`fn` is used uniformly at the top level and inside `impl` bodies. There is no
deep distinction between a component and a function: a component has temporal
semantics (it does not necessarily produce a direct result), whereas a function
is pure. `impl` methods are syntactic sugar for functions with an explicit `self`
argument. The `self` parameter naturally identifies a method without requiring a
special grammar rule.

The parser already distinguishes the two forms by context (`_item` vs
`impl_body`). This is not a grammatical ambiguity and no special elaboration
check is needed.

---

## Serious issues — need resolution before implementation

### 2. `=>` scoping — conditional introduction rule

The scoping rule for `=>` is conditional on whether the name is already in scope:

- **`x` not in scope:** `=>` introduces `x` with forward-only (let-like) scope
  from this statement forward. Not block-wide — the name is visible after the
  component call, not before.
- **`x` already in scope as `var`:** the connection binds to the existing
  block-wide signal. No new declaration is inserted.
- **`x` already in scope as `let`:** the connection binds to the existing
  binding, with whatever scope the `let` established.

The compiler should track `=>` as its own AST node rather than literally
inserting declarations. This allows it to produce tailored error messages.

Key consequence: implicit introduction from `=>` gives forward-only scope. For
structural feedback (where a signal must be reachable by multiple component
statements), an explicit `var` pre-declaration is required.

These rules are now documented in `port_connections.md` and `cycles_and_scoping.md`.

### 3. `var` declared inside `if`/`match` — scope is unresolved

`cycles_and_scoping.md` does not say whether `var` can appear inside a
conditional branch. This matters because `var` participates in an equation
system. An equation for a `var` that only exists in one branch of a conditional
is not structurally well-formed in RTL — hardware signals do not conditionally
exist.

Two plausible positions:

a. `var` is illegal inside `if`/`match` bodies. It may only appear at the top
   level of a component body. This is the safe position and matches how Verilog
   `wire` declarations work.

b. A `var` inside a conditional is hoisted to the enclosing block, and the
   equation is guarded by the condition. This is what SystemVerilog allows with
   `always_comb` variables.

Neither is currently specified. The language needs an explicit rule before any
implementation of `if`/`match` can proceed.

### 4. Multiple drivers via `=>` — resolved

`=>` always counts as the single assignment for its target (explicit `var` or
implicitly introduced). A second `=> x` in the same block is always a
multiple-driver error, regardless of whether `x` was pre-declared or implicitly
introduced. An explicit equation `x = expr` also counts as an assignment, so
`var x; x = a; comp { output => x }()` is also a multiple-driver error.

---

## Moderate issues — need a decision, may not block first pass

### 5. Direction checking belongs before type inference, after name resolution

`port_connections.md` says `in`/`out` keywords are optional because `=` implies
sink and `=>` implies source. The compiler still needs to verify that the field
name on the LHS of `=` corresponds to an `in` field and the LHS of `=>` to an
`out` field.

Port field directions are **structural**: they are declared explicitly in the port
definition and are never polymorphic or inferred. Once the component name is
resolved (name resolution), its port field directions are known. Direction checking
therefore belongs in a dedicated structural pass **after name resolution but before
type inference**.

This is distinct from type checking, which may involve constraint solving for
width compatibility and clock domains. Direction checking is simpler: look up the
declared field direction, compare to the operator used, emit an error if they
disagree. The two passes should be kept separate.

Update needed in `compiler_architecture.md`: change the current note that places
direction checking in the type-checking stage.

### 6. `var` with no equation — hard error at elaboration

A `var` that is declared but never assigned an equation is an undriven signal.
This is a hard error that should be caught at the end of the elaboration pass
(the completeness check on the equation system). Waiting until RTL lowering is
too late. The rule: every `var` in a block must have exactly one equation whose
LHS resolves to that signal node.

### 7. `var`-after-`let` detection in a single forward pass

`cycles_and_scoping.md` states: "`var` cannot shadow an earlier `let` binding in
the same block." Since `var` has block-wide scope and `let` has forward-only
scope, a single forward pass is sufficient to detect `var`-after-`let`.

The implicit `var` introduction from `=>` is subject to the same rule (see issue
2). The name resolver must apply the check uniformly whether the `var` is explicit
or implicit.

### 8. `var` inside `impl` method bodies — unresolved

`cycles_and_scoping.md` lists this as an open question. It needs a decision
before `impl` bodies are semantically checked.

The safest initial rule: `var` is only legal in component bodies, not in `impl`
method bodies. Methods that need stateful local signals should be expressed as
sub-components.

### 9. `dom clk` inference through cyclic `var` equations

For the common counter pattern:

```
var count;
count = (count + 1).reg{rstn}(0);
```

the clock domain is inferrable from `rstn`: if `rstn: Reset @clk`, then `.reg{rstn}()`
constrains the output to be `@clk`, and `count` is inferred to be `@clk`. The
constraint chain runs through `rstn`, not through `count` itself, so a forward
walk that processes the reset argument first can resolve this case.

The general fixpoint concern applies when inference truly depends on a cycle
through the `var` itself, without an external anchoring constraint like a typed
reset. That case requires constraint solving, not a simple forward walk.

The elaboration spec should note: try to resolve clock domains from explicit
arguments first (resets, explicit `@clk` annotations); only fall back to a
fixpoint pass when no such anchor exists.

### 10. `inline fn` — deferred

`inline fn` as a modifier form is not part of the current design scope. The
keyword `inline` is not reserved. This can be revisited when the function/component
hierarchy question is more settled.

### 11. Port connection completeness — `in` fields vs `out` fields

The compiler should require that all `in` fields are connected unless a default
value is provided in the port declaration. Missing `in` connections are a hard error.

`out` fields that are not bound (no `=>` and no pre-declared `var`) should be a
lint warning. The output is simply discarded, which is legal but often unintentional.

---

## Minor notes

### 12. `=>` RHS must be a bare name — not a field access

`port_connections.md` correctly restricts `=>` RHS to bare identifiers for the
initial implementation. Field-path targets (`output => p.valid`) complicate the
as-if `var` introduction rule and should be deferred.

### 13. `var` duplicate declarations in the same block

Two `var` declarations with the same name in the same block are a hard error.
`var` declares a signal node for the equation system — unlike `let`, which is
explicitly designed for shadowing. Redeclaring the same node is ambiguous.

### 14. `let` shadowing `var` — equation system is indexed by identity

`cycles_and_scoping.md` documents this correctly. Once `var count` is shadowed by
`let count = count + offset`, the original `var` signal still has its equation.
The equation system is indexed by signal node identity, not by name — so the
shadowing does not affect the `var`'s equation.

### 15. Structural feedback — clock domain mismatch detection point

When two components share `var`-declared wires, clock domain mismatches are
caught at the connection site during type checking: the type of the port field
(which carries its clock domain) is unified with the type of the `var` binding.
The type checker must apply this check against every connection that references
the `var`, not just the first.

### 16. Grammar ambiguity: return type `{ ... }` vs block `{ ... }` — resolved

`fn f(x: T) -> ReturnType { body }` was ambiguous in the tree-sitter grammar because
`type_named_arguments` used `{ }` as delimiters, matching the block opener.

**Fix**: a separate `return_type_expression` rule was introduced that excludes
`type_named_arguments`. `component_definition` and `function_definition` use
`return_type_expression` for their return type field. Since `{` is never a valid suffix
in a return type position, the parser unambiguously treats `{` after the return type as
the block opener. Named type arguments (e.g. `DF{clk}()`) are still available in
parameter type positions via the full `type_expression` rule.

---

## Summary of open items

| Issue | Target doc | Action |
|---|---|---|
| `=>` with pre-existing `let` binding | `port_connections.md`, `cycles_and_scoping.md` | Document as-if rule and tailored error |
| `var` in conditional branches | `cycles_and_scoping.md` | Add explicit legality rule |
| Direction checking is a structural pass | `compiler_architecture.md` | Move before type inference |
| Undriven `var` | `cycles_and_scoping.md` | Add completeness rule |
| `var` in `impl` methods | `cycles_and_scoping.md`, `impl.md` | Resolve open question |
| Clock inference through cyclic `var` | `compiler_architecture.md` | Note anchor-first approach |
| Connection completeness for `in` fields | `port_connections.md` | Add explicit rule |
| `=>` RHS field access | `port_connections.md` | Restrict to bare names for now |
| Duplicate `var` declarations | `cycles_and_scoping.md` | Add error rule |
