# Proposal: `view` — read-only port observers

## Motivation

Mirin's ports carry per-field direction: a `DF` interface
declares which side drives which field, and direction-checking enforces that
each field is driven exactly once. But code that only *observes* a port —
without driving anything — is common: assertions, monitor logic, predicates
like "is this beat transferring?". Today every observer counts as a participant
in the driver-linearity rules, which is wrong: an observer doesn't drive,
doesn't conflict with the real driver, and shouldn't compete for the role.

`view` introduces a binding form that says "this binding is a read-only
observer of a port" — a strictly positive snapshot of every field, regardless
of declared direction.

## Syntax

`view` is a **binding-form keyword**, in the same slot as `in` / `out` —
before the name, not on the type. The type stays the port:

```mirin
fn trans (view self: DF @clk) -> bool {
  self.valid && self.ready
}
```

A `view` binding has read access to every field of the port. The underlying
port type is unchanged; `view` only changes what the binding can do with it.

## Direction-check / linearity

- A `view` binding contributes **zero** drivers for every field.
- Multiple `view` bindings to the same port are fine — observers don't compete.
- The existing "every `in` field driven exactly once, every `out` field driven
  exactly once" rule applies only to non-`view` bindings.

So adding monitors / assertions / predicates over an interface never affects
who drives what.

## Worked example: handshake-transfer predicate on `DF`

```mirin
port DF = df {
  in  ready: bool,
  out valid: bool,
  out data:  uint(8),
}

impl DF {
  fn trans (view self: DF) -> bool {
    self.valid && self.ready
  }
}

// Either side of the interface can ask "is the handshake firing?":
fn producer { dom clk: Clock } (out p: DF @clk) {
  // ... drive p.valid / p.data, read p.ready ...
  let firing = p.trans();
  // ...
}

fn consumer { dom clk: Clock } (p: DF @clk) {
  // ... drive p.ready, read p.valid / p.data ...
  let firing = p.trans();
  // ...
}
```

Both producer and consumer can call `p.trans()`. Each side sees both fields
(one as its own output, the other as its input), so the predicate is
well-defined from either side. Method dispatch looks up `trans` in
`impl_methods[(DF, "trans")]` exactly like any other method — `view self` is
just a different acceptable receiver form.

## Method-dispatch interaction

`view self: T` and `self: T` are two acceptable shapes for the *same* method
slot, not two distinct overloads. `impl T { fn trans(view self: T) -> bool }`
defines one method named `trans` on `T`; the `view` modifier records that the
body promises to read-only.

(If we later need both `self: T` and `view self: T` versions of the same name,
that's overloading by receiver-form, which we don't want — too clever.)

## Implementation sketch

Mostly a binding-flag change with downstream filtering:

- Surface IR / grammar: `Parameter` gains a `view: bool` flag; the parser
  accepts `view` before the name.
- HIR: `HirParam` carries the flag through.
- Direction-check: skip driver-counting for `view`-flagged params.
- Flatten: a `view` port param expands per-field, but every leaf gets
  `fn_body_dir = Some(In)` regardless of declared field direction — the body
  only reads.
- SV emission: each view-leaf becomes an `input logic …` port on the emitted
  module. No new SV construct.

The underlying port type, `impl_methods` lookup, and method-call paths don't
change.

## Future: `out view`

A natural extension is the callee-provided view — the callee surfaces a view
of one of its internal ports, exposed as an optional named-section output:

```mirin
fn pipeline { dom clk: Clock, out view probe: DF @clk = unattached } (...)
```

The caller binds `probe` to a `view`-shaped local and can read the internal
port's fields after the call. Unattached by default — only consumers that want
introspection pay the wiring cost. Detailed shape (especially `unattached` and
how multiple call sites compose) is deferred to a follow-up proposal.
