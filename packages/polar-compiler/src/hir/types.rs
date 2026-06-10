//! The typed-HIR **type** vocabulary — the language `sig_of` and `infer` speak
//! (`planning/q3_typed_hir.md`, `planning/q7_terms_and_domains.md`).
//!
//! Three principles:
//!
//! - **The domain is a *component* of a value's type, not a parallel attribute**
//!   (`domain_checking_redux.md`): `uint(8) @clk` is a distinct type from
//!   `uint(8)`, so [`Type::Value`] pairs a [`ValueKind`] with a [`Domain`].
//! - **Generic parameters are referenced positionally** by the enclosing def's
//!   index — [`ValueKind::Param`] in type position, [`ConstArg::Param`] in const
//!   (width) position, [`Domain::Param`] in domain position — and substituted out
//!   downstream.
//! - **One term language** (Q7, chalk's shape): types, consts, and domains are
//!   the three kinds of [`Term`]. Inference variables live in a **single index
//!   space** ([`InferVar`]) whose kind is tracked by the inference table, and a
//!   generic argument list is a `Vec<Term>` ([`GenericArgs`]). Cross-kind
//!   structure (a const inside a domain, `T @ D` obligations) then needs no
//!   glue.
//!
//! Most of these types embed a [`DefId`], which has no `std::fmt::Debug` (its
//! fields need the db), so the `DefId`-carrying types omit `#[derive(Debug)]`.

use crate::nameres::ids::DefId;

/// Owner-relative local id: a value parameter or a body local. Index into the
/// owning def's local space; reset to 0 per def.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, salsa::Update)]
pub struct LocalId(pub u32);

/// An inference variable — **one index space across all term kinds** (chalk's
/// `InferenceVar`). The kind of a variable (type / const / domain) is recorded
/// by the inference table that minted it, not by the index. **Transient**:
/// produced and resolved away within `infer`; never appears in a `sig_of`
/// result.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, salsa::Update)]
pub struct InferVar(pub u32);

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
    /// A type inference variable.
    Infer(InferVar),
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

/// The clock-domain component of a type. A subtyping lattice with a single
/// edge: `@const` is below every concrete clock; see `domain_checking_redux.md`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum Domain {
    /// No `@…` annotation — a fresh domain variable, inferred later.
    Unspecified,
    /// `@const` — compile-time constant; coerces into any clock.
    Const,
    /// `@clk` where `clk` is the enclosing def's i-th generic (Domain-kind) param.
    Param(u32),
    /// A concrete clock bound to a local. Produced by body inference, not by
    /// `sig_of`.
    Clock(LocalId),
    /// A domain inference variable.
    Infer(InferVar),
}

/// A compile-time constant in const position (a `uint(W)` width). Arithmetic
/// widths (`uint(N+1)`) are deferred to `const_eval` (Q4); for now a width is a
/// literal or a single generic-param reference.
#[derive(Clone, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum ConstArg {
    Lit(u64),
    /// The enclosing def's i-th generic (Const-kind) parameter.
    Param(u32),
    /// A body local referenced in const position (`let y: uint(n) = …`). Legal
    /// only when the local's domain is `@const` — checked by `infer` (Q7 C);
    /// evaluation is `const_eval`'s job (Q4c).
    Local(LocalId),
    /// A const inference variable.
    Infer(InferVar),
    /// A width not yet representable here — arithmetic (`N+1`) or an anon-const
    /// body (`uint(cfg.bits())`). Deferred to `NormalConst`/`const_eval` (Q4b/c).
    /// Undecidable equalities involving it are **recorded as residual
    /// obligations**, never silently dropped.
    Deferred,
}

/// The generic arguments applied at a type-reference site (`Bus(uint(8))`), in
/// the def's declared parameter order.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct GenericArgs<'db>(pub Vec<Term<'db>>);

/// The uniform term: what a generic argument is and what an inference variable
/// binds to (chalk's `GenericArgData`). One of the three term kinds.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum Term<'db> {
    Type(Type<'db>),
    Const(ConstArg),
    Domain(Domain),
}

/// The reserved name of the **lifted** implicit domain parameter appended to a
/// pure signature (`domain_checking_redux.md` lifting). Checking-only: the
/// backend emits no clock port for it (a pure fn is combinational).
pub const LIFTED_DOM: &str = "__Dom";

/// One declared generic parameter of a def, in declaration order (named section
/// then positional). The index here is what `Param(i)` references.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct GenericParam {
    pub name: String,
    pub kind: TermKind,
    /// `true` if declared in the `{ … }` named section, `false` if positional —
    /// use sites match named args to the former, positional to the latter.
    pub from_named_section: bool,
}

impl GenericParam {
    /// The synthetic `__Dom` appended by lifting (never written by users).
    pub fn is_lifted_dom(&self) -> bool {
        self.name == LIFTED_DOM && matches!(self.kind, TermKind::Domain(_))
    }
}

/// The kind of a [`Term`] / of a generic parameter / of an inference variable
/// (chalk's `VariableKind`). A domain knows its sort; a const will grow its
/// type (`Const(Type)`) with const_eval (Q4c).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum TermKind {
    Type,
    Const,
    Domain(DomainSort),
}

/// The sort of a domain: `Clock` is the sub-sort of edge-bearing domains;
/// `Domain` is the full sort including `@const`. Registers quantify over
/// `Clock`; lifted pure signatures over `Domain` (so constant folding
/// survives). `@const` does not inhabit `Clock`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum DomainSort {
    Domain,
    Clock,
}

// ----- folding ---------------------------------------------------------------

/// One recursion over the term language, shared by every traversal that maps
/// types to types (substitution, variable resolution, …). Implementors override
/// the hooks they care about and delegate the rest to the `super_fold_*` free
/// functions (rustc's `TypeFolder` shape).
pub trait Folder<'db>: Sized {
    fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
        super_fold_type(self, t)
    }
    fn fold_const(&mut self, c: &ConstArg) -> ConstArg {
        c.clone()
    }
    fn fold_domain(&mut self, d: Domain) -> Domain {
        d
    }
    fn fold_term(&mut self, t: &Term<'db>) -> Term<'db> {
        super_fold_term(self, t)
    }
}

/// The structural recursion for [`Type`]: rebuild the node, folding every
/// component term. Atoms (`Clock`, `Infer`, `Error`, the `Param` kinds) pass
/// through — hooks intercept them *before* delegating here.
pub fn super_fold_type<'db, F: Folder<'db>>(f: &mut F, t: &Type<'db>) -> Type<'db> {
    match t {
        Type::Value { kind, domain } => Type::Value {
            kind: super_fold_kind(f, kind),
            domain: f.fold_domain(*domain),
        },
        Type::Port { def, args, domain } => Type::Port {
            def: *def,
            args: super_fold_args(f, args),
            domain: f.fold_domain(*domain),
        },
        other => other.clone(),
    }
}

pub fn super_fold_kind<'db, F: Folder<'db>>(f: &mut F, k: &ValueKind<'db>) -> ValueKind<'db> {
    match k {
        ValueKind::UInt { width } => ValueKind::UInt {
            width: f.fold_const(width),
        },
        ValueKind::Struct { def, args } => ValueKind::Struct {
            def: *def,
            args: super_fold_args(f, args),
        },
        other => other.clone(),
    }
}

pub fn super_fold_args<'db, F: Folder<'db>>(
    f: &mut F,
    args: &GenericArgs<'db>,
) -> GenericArgs<'db> {
    GenericArgs(args.0.iter().map(|a| f.fold_term(a)).collect())
}

pub fn super_fold_term<'db, F: Folder<'db>>(f: &mut F, t: &Term<'db>) -> Term<'db> {
    match t {
        Term::Type(ty) => Term::Type(f.fold_type(ty)),
        Term::Const(c) => Term::Const(f.fold_const(c)),
        Term::Domain(d) => Term::Domain(f.fold_domain(*d)),
    }
}
