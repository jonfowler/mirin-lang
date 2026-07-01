# The type system

Inference assigns every expression a type, and the backend lowers from those
types — so what a type *is* shapes both. This chapter is the vocabulary: how Mirin
represents a type, and the three decisions behind it. A
type is a structural kind paired with a clock domain; a width is a const
expression, not a number; and types, consts, and domains are one term language
with a single space of inference variables. The chapters on inference and the
backend both assume these.

## A type is a kind and a domain

The core type is `Type::Value { kind, domain }`: a **kind** — the structural part,
one of `uint(W)`, `sint(W)`, `bits(W)`, `bool`, `reset`, `event`, `integer` — and
a **domain**, the clock the value belongs to. The domain is part of the type, not
a label beside it: `uint(8) @clk` and `uint(8)` are different types, and that is
what lets inference catch an unsynchronised crossing as an ordinary type error.

Aggregates carry no domain of their own. `Vec(N, A)` and `(A, B)` hold their
domains entirely in their elements, so a tuple can mix domains — `(uint(8) @a,
uint(8) @b)` is a legal type — while a value leaf cannot. A nominal `struct` or
`port` is one `Type::Port`, carrying the definition, its generic arguments, and a
domain that stamps its fields. (Struct and port share this representation; the
definition's kind records which it was declared as, and only a port's fields carry
`in`/`out` direction.)

## Widths are const expressions

A width is not an integer; it is a `ConstArg` — a small expression. It can be a
literal, a reference to a generic const parameter, a body-local used as a width
(`let w = …; uint(w)`), arithmetic over those (`Op(Add, …)`), a struct-field
projection (`cfg.width`), or an associated const
(`A::bit_size`). So `uint(W)` is parameterised by an *expression* `W`, and a
generic body works with widths it cannot yet reduce to a number.

This is the representation behind the optimistic checking the [Overview](../architecture/overview.md)
described. Inference treats a symbolic width as a hole and the facts about it — two
widths equal, a literal fits — as constraints to discharge later, never needing to
do the arithmetic to produce a type. Reducing a `ConstArg` to a number, when a
call makes it concrete, is the job of [constant evaluation](const-eval.md).

## One term language

Types, consts, and domains are the three kinds of a single `Term`. A generic
argument list is a `Vec<Term>`, and a generic parameter is referenced positionally
by the enclosing definition's index — `Param(i)` in type, const, or domain
position — and substituted out downstream. Inference variables likewise live in
**one index space** (`InferVar`), with the kind of each variable tracked by the
inference table rather than the index. This follows chalk's representation.

The payoff is that cross-kind structure needs no special glue. A value's domain is
a const-free term inside its type; a trait obligation `T @ D` ranges over a type
and a domain together; substitution and resolution walk all three kinds through
one `Folder`. Inference, the next chapter, is built on exactly this term language —
unifying terms, and deferring the const and domain facts it cannot yet settle.
