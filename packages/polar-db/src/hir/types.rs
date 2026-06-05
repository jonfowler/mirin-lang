//! The typed-HIR **type** vocabulary — the language `sig_of` and (later) `infer`
//! speak (`planning/q3_typed_hir.md`). A faithful, leaner port of
//! `polar-compiler`'s `hir::HirType` family.
//!
//! Two principles carried over from the old compiler and the planning docs:
//!
//! - **The domain is a *component* of a value's type, not a parallel attribute**
//!   (`domain_checking.md`): `uint(8) @clk` is a distinct type from `uint(8)`, so
//!   [`Type::Value`] pairs a [`ValueKind`] with a [`Domain`].
//! - **Generic parameters are referenced positionally** by the enclosing def's
//!   index — [`ValueKind::Param`] in type position, [`ConstArg::Param`] in const
//!   (width) position, [`Domain::Param`] in domain position — and substituted out
//!   downstream.
//!
//! Most of these types embed a [`DefId`], which has no `std::fmt::Debug` (its
//! fields need the db), so the `DefId`-carrying types omit `#[derive(Debug)]`.

use crate::nameres::ids::DefId;

/// Owner-relative local id: a value parameter (Q3b) or, later, a body local
/// (Q3c). Index into the owning def's local space; reset to 0 per def.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, salsa::Update)]
pub struct LocalId(pub u32);

/// `in` / `out` direction on a port field or a directed parameter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum Direction {
    In,
    Out,
}

/// A type, with its domain component folded in (domains are part of the type).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum Type<'db> {
    /// A value type: a structural kind plus its clock-domain component.
    Value {
        kind: ValueKind<'db>,
        domain: Domain,
    },
    /// A port interface type, with the port's generic args and its domain.
    Port {
        def: DefId<'db>,
        args: GenericArgs<'db>,
        domain: Domain,
    },
    /// The meta-type `Clock` — a domain witness, never the type of a value.
    Clock,
    /// An inference variable. **Transient**: produced and resolved away within
    /// `infer` (Q3d); never appears in a `sig_of` result.
    Infer(u32),
    /// A type name that did not resolve. Keeps lowering total; surfaces as a
    /// diagnostic rather than an `unwrap`.
    Error,
}

/// The structural part of a value type (everything but the domain).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum ValueKind<'db> {
    /// `uint(W)` — the width is a const arg (literal or generic-param ref).
    UInt {
        width: ConstArg,
    },
    Bool,
    Reset,
    Event,
    Usize,
    /// A user struct, with its generic args.
    Struct {
        def: DefId<'db>,
        args: GenericArgs<'db>,
    },
    /// The enclosing def's i-th generic parameter, in **type** position
    /// (`data: A`). Substituted out by monomorphise/flatten downstream.
    Param(u32),
}

/// The clock-domain component of a type. A subtyping lattice (`@const` is a
/// supertype of every concrete clock); see `domain_checking.md`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum Domain {
    /// No `@…` annotation — a fresh domain variable, inferred later.
    Unspecified,
    /// `@const` — compile-time constant; subtype-compatible with any clock.
    Const,
    /// `@clk` where `clk` is the enclosing def's i-th generic (Domain-kind) param.
    Param(u32),
    /// A concrete clock bound to a local. Produced by body inference (Q3c/d),
    /// not by `sig_of`.
    Clock(LocalId),
    /// A domain inference variable. **Transient**: resolved away within `infer`.
    Infer(u32),
}

/// A compile-time constant in const position (a `uint(W)` width). Arithmetic
/// widths (`uint(N+1)`) are deferred to `const_eval` (Q4); for now a width is a
/// literal or a single generic-param reference.
#[derive(Clone, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum ConstArg {
    Lit(u64),
    /// The enclosing def's i-th generic (Const-kind) parameter.
    Param(u32),
    /// A width expression not yet representable (e.g. arithmetic) — deferred to
    /// `const_eval`. Keeps lowering total.
    Deferred,
}

/// The generic arguments applied at a type-reference site (`Bus(uint(8))`), in
/// the def's declared parameter order.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct GenericArgs<'db>(pub Vec<GenericArg<'db>>);

#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum GenericArg<'db> {
    Type(Type<'db>),
    Const(ConstArg),
    Domain(Domain),
}

/// One declared generic parameter of a def, in declaration order (named section
/// then positional). The index here is what `Param(i)` references.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct GenericParam {
    pub name: String,
    pub kind: GenericParamKind,
    /// `true` if declared in the `{ … }` named section, `false` if positional —
    /// use sites match named args to the former, positional to the latter.
    pub from_named_section: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum GenericParamKind {
    Type,
    Const,
    Domain,
}
