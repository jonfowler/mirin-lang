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
        width: ConstArg<'db>,
    },
    Bool,
    Reset,
    Event,
    Integer,
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

/// The arithmetic operators of the const-expression fragment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub enum ConstOp {
    Add,
    Sub,
    Mul,
}

/// A compile-time constant in const position (a `uint(W)` width): the
/// restricted const-expression tree (`planning/const_eval.md`). Leaves are
/// literals, generic params, and body locals; `Op`/`Field` carry width
/// arithmetic and config-field projection. Anything bigger (a call, an
/// if/else) is reached *through a `Local` leaf* — the evaluator demands the
/// local's defining expression in the body.
// Manual `Hash` (Box<Type> has none; Assoc hashes its item — equal Assocs
// share it) and `Debug` (DefId has none).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum ConstArg<'db> {
    Lit(i128),
    /// The enclosing def's i-th generic (Const-kind) parameter.
    Param(u32),
    /// A body local referenced in const position (`let y: uint(n) = …`). Legal
    /// only when the local's domain is `@const` — checked by `infer` (Q7 C);
    /// evaluation is `const_eval`'s job (Q4c).
    Local(LocalId),
    /// A const inference variable.
    Infer(InferVar),
    /// Width arithmetic: `uint(n + 1)`, `uint(2 * n)`.
    Op(ConstOp, Box<ConstArg<'db>>, Box<ConstArg<'db>>),
    /// Const field projection: `uint(cfg.bits)`.
    Field(Box<ConstArg<'db>>, String),
    /// An UNEVALUATED associated const (rustc's `ConstKind::Unevaluated`):
    /// `item` is a trait's const DECL (resolved through an impl once
    /// `self_ty` is concrete) or an impl's own const. Equality while generic
    /// is structural; anything else rides the ConstEq obligations and
    /// discharges at monomorphisation (planning/traits.md T4).
    Assoc {
        item: DefId<'db>,
        self_ty: Box<Type<'db>>,
    },
    /// A const expression outside the representable fragment (e.g. a call in
    /// width position — write `let w = f(n); uint(w)` instead). Undecidable
    /// equalities involving it are **recorded as residual obligations**,
    /// never silently dropped.
    Deferred,
}

/// The generic arguments applied at a type-reference site (`Bus(uint(8))`), in
/// the def's declared parameter order.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct GenericArgs<'db>(pub Vec<Term<'db>>);

/// Match a fully-resolved `goal` type against an impl-header `self type`
/// whose `Param(i)` slots (any kind) are holes, binding them into `binding`
/// (rustc's `match_impl`). Domains are ignored — trait impls are domain-blind
/// in v1; the goal's domain flows through the method's own signature instead.
/// Returns false on any structural mismatch or inconsistent re-binding.
pub fn match_header<'db>(
    goal: &Type<'db>,
    header: &Type<'db>,
    binding: &mut [Option<Term<'db>>],
) -> bool {
    let bind = |binding: &mut [Option<Term<'db>>], i: u32, t: Term<'db>| -> bool {
        match binding.get_mut(i as usize) {
            Some(slot @ None) => {
                *slot = Some(t);
                true
            }
            Some(Some(prev)) => *prev == t,
            None => false,
        }
    };
    match (goal, header) {
        (
            _,
            Type::Value {
                kind: ValueKind::Param(i),
                ..
            },
        ) => bind(binding, *i, Term::Type(goal.clone())),
        (Type::Value { kind: gk, .. }, Type::Value { kind: hk, .. }) => match (gk, hk) {
            (ValueKind::UInt { width: gw }, ValueKind::UInt { width: hw }) => match hw {
                ConstArg::Param(i) => bind(binding, *i, Term::Const(gw.clone())),
                _ => gw == hw,
            },
            (ValueKind::Bool, ValueKind::Bool)
            | (ValueKind::Reset, ValueKind::Reset)
            | (ValueKind::Event, ValueKind::Event)
            | (ValueKind::Integer, ValueKind::Integer) => true,
            (ValueKind::Struct { def: gd, args: ga }, ValueKind::Struct { def: hd, args: ha }) => {
                gd == hd && match_header_args(ga, ha, binding)
            }
            _ => false,
        },
        (
            Type::Port {
                def: gd, args: ga, ..
            },
            Type::Port {
                def: hd, args: ha, ..
            },
        ) => gd == hd && match_header_args(ga, ha, binding),
        (Type::Clock, Type::Clock) => true,
        _ => false,
    }
}

fn match_header_args<'db>(
    goal: &GenericArgs<'db>,
    header: &GenericArgs<'db>,
    binding: &mut [Option<Term<'db>>],
) -> bool {
    goal.0.len() == header.0.len()
        && goal.0.iter().zip(&header.0).all(|(g, h)| match (g, h) {
            (Term::Type(g), Term::Type(h)) => match_header(g, h, binding),
            (Term::Const(g), Term::Const(h)) => match h {
                ConstArg::Param(i) => {
                    let i = *i;
                    match binding.get_mut(i as usize) {
                        Some(slot @ None) => {
                            *slot = Some(Term::Const(g.clone()));
                            true
                        }
                        Some(Some(prev)) => *prev == Term::Const(g.clone()),
                        None => false,
                    }
                }
                _ => g == h,
            },
            // Domains are not matched (domain-blind impls).
            (Term::Domain(_), Term::Domain(_)) => true,
            _ => false,
        })
}

/// Does the type contain any STRUCTURAL inference variable (so an impl match
/// must wait)? Domain vars are ignored — trait impls are domain-blind, and
/// unconstrained domains stay unbound until finish defaults them.
pub fn type_has_infer(ty: &Type<'_>) -> bool {
    struct Scan(bool);
    impl<'db> Folder<'db> for Scan {
        fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
            if matches!(t, Type::Infer(_)) {
                self.0 = true;
            }
            super_fold_type(self, t)
        }
        fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
            if matches!(c, ConstArg::Infer(_)) {
                self.0 = true;
            }
            super_fold_const(self, c)
        }
    }
    let mut s = Scan(false);
    s.fold_type(ty);
    s.0
}

/// Structural substitution of `Param(i)` slots (all kinds) from an
/// `Option<Term>` binding — the table-free counterpart of infer's
/// `Substituter`, for use outside an inference context (solver headers,
/// backend composition). Unbound slots pass through unchanged.
pub fn subst_type_opt<'db>(ty: &Type<'db>, subst: &[Option<Term<'db>>]) -> Type<'db> {
    struct S<'a, 'db>(&'a [Option<Term<'db>>]);
    impl<'db> Folder<'db> for S<'_, 'db> {
        fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
            if let Type::Value {
                kind: ValueKind::Param(i),
                ..
            } = t
                && let Some(Some(Term::Type(bound))) = self.0.get(*i as usize)
            {
                return bound.clone();
            }
            super_fold_type(self, t)
        }
        fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
            if let ConstArg::Param(i) = c
                && let Some(Some(Term::Const(bound))) = self.0.get(*i as usize)
            {
                return bound.clone();
            }
            super_fold_const(self, c)
        }
        fn fold_domain(&mut self, d: Domain) -> Domain {
            if let Domain::Param(i) = d
                && let Some(Some(Term::Domain(bound))) = self.0.get(i as usize)
            {
                return bound.clone();
            }
            d
        }
    }
    S(subst).fold_type(ty)
}

/// `subst_type_opt`'s ConstArg counterpart.
pub fn subst_const_opt<'db>(c: &ConstArg<'db>, subst: &[Option<Term<'db>>]) -> ConstArg<'db> {
    struct S<'a, 'db>(&'a [Option<Term<'db>>]);
    impl<'db> Folder<'db> for S<'_, 'db> {
        fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
            if let ConstArg::Param(i) = c
                && let Some(Some(Term::Const(bound))) = self.0.get(*i as usize)
            {
                return bound.clone();
            }
            super_fold_const(self, c)
        }
        fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
            if let Type::Value {
                kind: ValueKind::Param(i),
                ..
            } = t
                && let Some(Some(Term::Type(bound))) = self.0.get(*i as usize)
            {
                return bound.clone();
            }
            super_fold_type(self, t)
        }
    }
    S(subst).fold_const(c)
}

/// `self_ty: trait_def` — a trait reference. Traits take no generic args of
/// their own in v1, so a TraitRef is just the pair (planning/traits.md).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct TraitRef<'db> {
    pub trait_def: DefId<'db>,
    pub self_ty: Type<'db>,
}

/// A predicate on a signature: written bounds (`param T: Add`, `where T:
/// Bits`) and, on trait method decls, the implicit `Self: Trait`. Instantiated
/// into obligations at every call; assumed (the param env) inside the body.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum Predicate<'db> {
    Trait(TraitRef<'db>),
}

impl std::hash::Hash for ConstArg<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            ConstArg::Lit(v) => v.hash(state),
            ConstArg::Param(i) => i.hash(state),
            ConstArg::Local(l) => l.hash(state),
            ConstArg::Infer(v) => v.hash(state),
            ConstArg::Op(op, a, b) => {
                op.hash(state);
                a.hash(state);
                b.hash(state);
            }
            ConstArg::Field(b, f) => {
                b.hash(state);
                f.hash(state);
            }
            ConstArg::Assoc { item, .. } => item.hash(state),
            ConstArg::Deferred => {}
        }
    }
}

impl std::fmt::Debug for ConstArg<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConstArg::Lit(v) => write!(f, "Lit({v})"),
            ConstArg::Param(i) => write!(f, "Param({i})"),
            ConstArg::Local(l) => write!(f, "Local({l:?})"),
            ConstArg::Infer(v) => write!(f, "Infer({v:?})"),
            ConstArg::Op(op, a, b) => write!(f, "Op({op:?}, {a:?}, {b:?})"),
            ConstArg::Field(b, name) => write!(f, "Field({b:?}, {name})"),
            ConstArg::Assoc { .. } => write!(f, "Assoc(..)"),
            ConstArg::Deferred => write!(f, "Deferred"),
        }
    }
}

/// The uniform term: what a generic argument is and what an inference variable
/// binds to (chalk's `GenericArgData`). One of the three term kinds.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum Term<'db> {
    Type(Type<'db>),
    Const(ConstArg<'db>),
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
    fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
        super_fold_const(self, c)
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
pub fn super_fold_const<'db, F: Folder<'db>>(f: &mut F, c: &ConstArg<'db>) -> ConstArg<'db> {
    match c {
        ConstArg::Op(op, a, b) => {
            ConstArg::Op(*op, Box::new(f.fold_const(a)), Box::new(f.fold_const(b)))
        }
        ConstArg::Field(base, name) => ConstArg::Field(Box::new(f.fold_const(base)), name.clone()),
        // The projection's self type folds like any type — substitution and
        // resolution reach through it for free.
        ConstArg::Assoc { item, self_ty } => ConstArg::Assoc {
            item: *item,
            self_ty: Box::new(f.fold_type(self_ty)),
        },
        other => other.clone(),
    }
}

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
