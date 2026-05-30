use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::surface_ir::{
    Block, Expression, FunctionDefinition, Identifier, ImplBlock, Item, LetStatement,
    NamedArgument, NamedParameter, NodeId, Parameter, PortDefinition, PositionalArgument,
    PostfixOperation, SourceFile, Statement, StructDefinition, TypeArgument, TypeExpression,
    TypeSuffix, VarStatement,
};
use crate::{SourceExcerpt, SourcePosition, SourceSpan};

/// Unique ID for a top-level definition (component, struct, port, impl).
///
/// Modeled on rustc's `DefId`: an index into the `defs` table, separate from
/// `NodeId`. The separation makes the def-vs-local distinction explicit in the
/// type system and leaves room for cross-file/cross-crate identity later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DefId(pub u32);

/// What an identifier resolves to. Mirrors rustc's `Res<Id>`: the category
/// (`Def` vs `Local`) is encoded in the variant, with the kind for definitions
/// carried inline so callers don't need a second lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Res {
    /// A top-level definition. `DefKind` identifies the flavor.
    Def(DefKind, DefId),
    /// A local binding: parameter, `let`, `var`, or implicit `var` from `=>`.
    /// The `NodeId` points to the binding name's identifier node, which is
    /// also the key into `ResolveResult::locals` for full info.
    Local(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Fn,
    Struct,
    Port,
    Impl,
    /// A function defined inside an `impl T { … }` block. Lives outside the
    /// global name table — callers reach it via `Type::method` or by method
    /// dispatch on a receiver of type `T`. `owner` is the `T` def.
    Method {
        owner: DefId,
    },
    /// A primitive type baked into the language (`uint`, `bool`, …). Exists
    /// only so the def table can carry a `DefId` for the type — used as the
    /// owner key in `impl_methods` so prelude methods like `uint::reg`
    /// dispatch through the same table as user-defined `impl T { fn m … }`.
    /// At type-expression sites the surface parser handles these as keywords;
    /// the resolver never sees them in type position.
    BuiltinType,
    /// Term-level constructor for a struct or port. Allocated for the name
    /// after `=` in `struct Packet = packet { … }`. Mirrors rustc's
    /// `DefKind::Ctor`: a separate def keeps the type/term split honest, so
    /// `Packet` (the type) and `packet` (the constructor expression) resolve
    /// to distinct entries. `owner` is the struct or port `DefId`.
    Ctor {
        owner: DefId,
    },
}

/// Kind of a generic parameter on a struct, port, or fn.
///
/// Mirrors rustc's `GenericParamDefKind` but with HDL-flavoured options. Type
/// params introduce a name that can appear in field types (`struct Bus(A: Type)`).
/// Const params are compile-time integers passed in (`param N: usize`). Domain
/// params bind a clock (`dom clk: Clock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericParamKind {
    Type,
    Const,
    Domain,
}

/// One entry in a def's generic parameter list.
///
/// The `local` field points to the parameter name's `NodeId` (the same id the
/// resolver registered as a local). HIR lowering uses it to translate
/// param-name references in field/return types into the right
/// `HirTypeKind::Param(index)`.
#[derive(Debug, Clone)]
pub struct GenericParamInfo {
    pub name: String,
    pub kind: GenericParamKind,
    pub local: NodeId,
    pub span: SourceSpan,
}

/// How a local binding was introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// Parameter (named or positional) of the owning def. `direction`
    /// mirrors the surface `in`/`out` keyword; later passes use it to
    /// decide whether a param is writable from inside the function body.
    Param {
        owner: DefId,
        direction: Option<crate::surface_ir::Direction>,
    },
    /// `let x = ...` — sequential, forward-only scope, supports shadowing.
    Let,
    /// `var x` — block-wide scope via pre-scan; participates in equations.
    Var,
    /// Introduced by `output => x` when `x` was not already in scope —
    /// forward-only scope.
    ImplicitVar,
}

#[derive(Debug, Clone)]
pub struct DefInfo {
    /// Duplicated with `Res::Def(DefKind, _)` for ergonomics: lets callers
    /// query a def's kind from a `DefId` alone, the way rustc exposes
    /// `tcx.def_kind(def_id)`.
    pub kind: DefKind,
    pub name: String,
    pub span: SourceSpan,
    /// Generic parameters declared on this def — type, const, and domain
    /// kinds, in declaration order. Empty for everything except `Struct`,
    /// `Port`, and (eventually) `Fn`. The position in this list is the index
    /// referenced by `HirTypeKind::Param(i)`.
    pub generic_params: Vec<GenericParamInfo>,
}

#[derive(Debug, Clone)]
pub struct LocalInfo {
    pub kind: LocalKind,
    pub name: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct ResolveError {
    pub kind: ResolveErrorKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveErrorKind {
    /// An identifier that does not refer to any definition, parameter, or binding.
    UndefinedName(String),
    /// Two top-level definitions share the same name.
    DuplicateDef(String),
    /// Two `var` declarations in the same block share the same name.
    DuplicateVar(String),
    /// A `var` declaration appears in the same block after a `let` binding with the same name.
    VarAfterLet(String),
    /// A `=>` source connection targets a name that resolves to a `let` binding (not a signal node).
    SourceOnLetBinding(String),
    /// A `=>` source connection targets a name that is not a valid signal node (e.g. a parameter or def).
    InvalidSourceTarget(String),
    /// A `self` parameter appears in a top-level `fn`, not inside an `impl` block.
    SelfOutsideImpl,
    /// An `impl` block targets a name that has no preceding struct/port definition.
    ImplOfUnknownType(String),
    /// A `struct` or `port` declaration is missing its `= constructor` clause.
    /// Polar requires every nominal type to declare its term-level constructor
    /// name explicitly, the way `struct Foo<T> { … }` in Rust pairs with the
    /// `Foo { … }` constructor — except Polar uses a distinct constructor name
    /// (e.g. `struct Bus(A: Type) = bus { … }`).
    MissingConstructor(String),
}

impl fmt::Display for ResolveErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveErrorKind::UndefinedName(name) => {
                write!(f, "undefined name `{name}`")
            }
            ResolveErrorKind::DuplicateDef(name) => {
                write!(f, "`{name}` is defined more than once in this file")
            }
            ResolveErrorKind::DuplicateVar(name) => {
                write!(
                    f,
                    "`{name}` is declared more than once as `var` in this block"
                )
            }
            ResolveErrorKind::VarAfterLet(name) => {
                write!(
                    f,
                    "cannot declare `var {name}` after a `let {name}` binding in the same block"
                )
            }
            ResolveErrorKind::SourceOnLetBinding(name) => {
                write!(
                    f,
                    "`{name}` is a `let` binding; source arrows (`=>`) require a `var` signal"
                )
            }
            ResolveErrorKind::InvalidSourceTarget(name) => {
                write!(
                    f,
                    "`{name}` is not a valid source arrow target; only `var` signals may be wired this way"
                )
            }
            ResolveErrorKind::SelfOutsideImpl => {
                write!(f, "`self` parameter is only valid inside an `impl` block")
            }
            ResolveErrorKind::ImplOfUnknownType(name) => {
                write!(
                    f,
                    "cannot `impl` `{name}`: no struct or port with that name"
                )
            }
            ResolveErrorKind::MissingConstructor(name) => {
                write!(
                    f,
                    "`{name}` is missing its `= constructor` clause; add one like `struct {name} = {} {{ … }}`",
                    name.to_lowercase()
                )
            }
        }
    }
}

pub fn render_resolve_errors(
    errors: &[ResolveError],
    source: &str,
    path: Option<&Path>,
    f: &mut impl fmt::Write,
) -> fmt::Result {
    for (index, error) in errors.iter().enumerate() {
        if index > 0 {
            writeln!(f)?;
        }
        writeln!(f, "error: {}", error.kind)?;
        if let Some(path) = path {
            writeln!(
                f,
                " --> {}:{}:{}",
                path.display(),
                error.span.start.row + 1,
                error.span.start.column + 1
            )?;
        } else {
            writeln!(
                f,
                " --> {}:{}",
                error.span.start.row + 1,
                error.span.start.column + 1
            )?;
        }
        if let Some(excerpt) = excerpt_for_span(source, &error.span) {
            writeln!(f, "  |")?;
            writeln!(f, "{:>2} | {}", excerpt.line_number, excerpt.line_text)?;
            writeln!(
                f,
                "  | {}{}",
                " ".repeat(excerpt.highlight_start),
                "^".repeat(
                    excerpt
                        .highlight_end
                        .saturating_sub(excerpt.highlight_start)
                )
            )?;
        }
    }
    Ok(())
}

fn excerpt_for_span(source: &str, span: &SourceSpan) -> Option<SourceExcerpt> {
    let line_text = source.lines().nth(span.start.row)?.to_owned();
    let start = span.start.column.min(line_text.len());
    let end = if span.start.row == span.end.row {
        span.end
            .column
            .max(start + 1)
            .min(line_text.len().max(start + 1))
    } else {
        line_text.len().max(start + 1)
    };
    Some(SourceExcerpt {
        line_number: span.start.row + 1,
        line_text,
        highlight_start: start,
        highlight_end: end,
    })
}

/// The output of name resolution.
#[derive(Debug, Default)]
pub struct ResolveResult {
    /// Maps each identifier use-site (by its `NodeId`) to what it resolves to.
    pub resolutions: HashMap<NodeId, Res>,
    pub errors: Vec<ResolveError>,
    /// Info for each top-level definition, indexed by `DefId.0`.
    pub defs: Vec<DefInfo>,
    /// Info for each local binding (param, let, var, implicit var),
    /// keyed by the binding name identifier's `NodeId`.
    pub locals: HashMap<NodeId, LocalInfo>,
    /// Method dispatch table keyed on `(owner_type_def_id, method_name)`.
    /// Populated from `impl T { fn m … }` items. Methods do not appear in
    /// the global name table — calls reach them either via method dispatch
    /// (`x.m(…)` where `x: T`) or eventually via path syntax (`T::m(…)`,
    /// not yet implemented).
    pub impl_methods: HashMap<(DefId, String), DefId>,
}

impl ResolveResult {
    pub fn def_info(&self, id: DefId) -> &DefInfo {
        &self.defs[id.0 as usize]
    }

    pub fn local_info(&self, id: NodeId) -> &LocalInfo {
        &self.locals[&id]
    }

    /// Find a definition by name. Returns the `DefId` of the first matching
    /// entry, prelude or user-defined. Names are unique across the def table
    /// (duplicate user definitions are rejected during collection), so the
    /// result is unambiguous when it exists.
    pub fn def_id(&self, name: &str) -> Option<DefId> {
        self.defs
            .iter()
            .enumerate()
            .find(|(_, d)| d.name == name)
            .map(|(i, _)| DefId(i as u32))
    }
}

/// Builtin functions made available to every source file. These are pre-populated
/// in the def table so that calls to them go through the same `DefId`-based path
/// as user-defined functions.
///
/// The entries with non-identifier names (`+`, `*`, ...) are reachable only via
/// HIR lowering, which converts surface-level operator syntax into a `HirCall`
/// against the corresponding prelude `DefId`. The user cannot name them
/// directly because they don't tokenise as identifiers.
const PRELUDE_FN_NAMES: &[&str] = &["reg", "+", "*", "posedge"];

/// Builtin primitive types that get a `DefId` in the prelude so they can act
/// as `impl_methods` owners. The surface parser treats these as keywords in
/// type position; they're never resolved by name from a type expression.
/// Pre-seeded methods (e.g. `uint::reg`, `Clock::posedge`) live in
/// `impl_methods` so method dispatch on `recv.method(...)` flows through the
/// same path for primitives and user types alike.
///
/// `Type` is the kind name for type-kinded generic parameters. It appears
/// only in parameter position (`struct Bus(A: Type)`); the resolver
/// recognises it there and tags the parameter as `GenericParamKind::Type`.
const PRELUDE_TYPE_NAMES: &[&str] = &["uint", "bool", "Clock", "Event", "Type"];

/// Identifier-shaped literals (`true`, `false`, `high`, `low`). The resolver
/// neither errors on them nor emits a `Res` — HIR lowering recognises them by
/// name and emits a `Const` node directly.
fn is_builtin_literal(name: &str) -> bool {
    matches!(name, "true" | "false" | "high" | "low")
}

/// Whether a parameter's declared type is the literal `Type` builtin — used
/// to identify type-kinded generic parameters. The type expression must be
/// the bare identifier `Type` with no suffixes or domain annotation.
fn is_type_kind_annotation(ty: Option<&TypeExpression>) -> bool {
    let Some(ty) = ty else {
        return false;
    };
    ty.name.text == "Type" && ty.suffixes.is_empty() && ty.domain.is_none()
}

fn prelude_span() -> SourceSpan {
    SourceSpan {
        start_byte: 0,
        end_byte: 0,
        start: SourcePosition { row: 0, column: 0 },
        end: SourcePosition { row: 0, column: 0 },
    }
}

pub fn resolve_file(file: &SourceFile) -> ResolveResult {
    let mut ctx = Ctx::with_prelude();

    // Pass 1: collect all top-level definition names.
    for item in &file.items {
        ctx.collect_item(item);
    }

    // Pass 2: resolve each item.
    for item in &file.items {
        ctx.resolve_item(item);
    }

    // Pass 3: backfill the prelude `reg` entry for every value-shaped type
    // that doesn't already have a user-defined `reg`. The prelude `reg`
    // accepts any value type as `self`, so structs, ports, and the primitive
    // `uint` all dispatch to it by default. A user-defined `impl T { fn reg }`
    // wins because impl-block resolution (pass 2) ran first.
    ctx.backfill_prelude_reg();

    ctx.result
}

// ----- internals -----

#[derive(Default)]
struct Ctx {
    result: ResolveResult,
    /// Top-level definitions in scope, mapped by name to their `DefId` and kind.
    /// The kind is duplicated here (also in `DefInfo`) so resolution sites can
    /// build `Res::Def(kind, id)` without a second lookup.
    global_defs: HashMap<String, (DefKind, DefId)>,
}

impl Ctx {
    fn with_prelude() -> Self {
        let mut ctx = Self::default();
        for &name in PRELUDE_FN_NAMES {
            let id = DefId(ctx.result.defs.len() as u32);
            ctx.result.defs.push(DefInfo {
                kind: DefKind::Fn,
                name: name.to_owned(),
                span: prelude_span(),
                generic_params: Vec::new(),
            });
            ctx.global_defs.insert(name.to_owned(), (DefKind::Fn, id));
        }
        for &name in PRELUDE_TYPE_NAMES {
            let id = DefId(ctx.result.defs.len() as u32);
            ctx.result.defs.push(DefInfo {
                kind: DefKind::BuiltinType,
                name: name.to_owned(),
                span: prelude_span(),
                generic_params: Vec::new(),
            });
            ctx.global_defs
                .insert(name.to_owned(), (DefKind::BuiltinType, id));
        }
        // Seed `Clock::posedge`. Like `uint::reg`, the prelude method has
        // no user-visible `HirFn` — typeck recognises the callee `DefId`
        // and applies a hand-rolled signature (`Clock @D -> Event @D`).
        let clock_def_id = ctx
            .result
            .def_id("Clock")
            .expect("`Clock` was just added to the prelude");
        let posedge_def_id = ctx
            .result
            .def_id("posedge")
            .expect("`posedge` was just added to the prelude");
        ctx.result
            .impl_methods
            .insert((clock_def_id, "posedge".to_owned()), posedge_def_id);

        // Declare reg's generic parameters so typeck's `fresh_args_for_def`
        // allocates a fresh `?A` and `?clk` per call site.
        // Signature: `fn reg { A: Type, dom clk: Clock }(self: A @clk,
        // rst: Reset @clk, reset_val: A) -> A @clk`.
        let reg_def_id = ctx
            .result
            .def_id("reg")
            .expect("`reg` was just added to the prelude");
        ctx.result.defs[reg_def_id.0 as usize].generic_params = vec![
            GenericParamInfo {
                name: "A".to_owned(),
                kind: GenericParamKind::Type,
                // Sentinel NodeIds: prelude generic params have no surface
                // node. They are never looked up via `current_generic_params`
                // because reg's HirFn is synthesised directly.
                local: NodeId(u32::MAX),
                span: prelude_span(),
            },
            GenericParamInfo {
                name: "clk".to_owned(),
                kind: GenericParamKind::Domain,
                local: NodeId(u32::MAX - 1),
                span: prelude_span(),
            },
        ];

        // Declare posedge's generic parameter so typeck's general path
        // handles it. Signature: `fn posedge { dom clk: Clock }(self: Clock)
        // -> Event @clk`. The single `dom` arg lands the result's domain
        // via the receiver's identity once method dispatch unifies `self`.
        ctx.result.defs[posedge_def_id.0 as usize].generic_params = vec![GenericParamInfo {
            name: "clk".to_owned(),
            kind: GenericParamKind::Domain,
            local: NodeId(u32::MAX - 2),
            span: prelude_span(),
        }];
        ctx
    }

    /// Wire the prelude `reg` method into every value-shaped def that doesn't
    /// already have a user-defined `reg` entry. Run after impl-block
    /// resolution so user `impl T { fn reg }` definitions take precedence.
    fn backfill_prelude_reg(&mut self) {
        let Some(reg_def_id) = self.result.def_id("reg") else {
            return;
        };
        let value_def_ids: Vec<DefId> = self
            .result
            .defs
            .iter()
            .enumerate()
            .filter_map(|(i, info)| match info.kind {
                DefKind::Struct | DefKind::Port | DefKind::BuiltinType => Some(DefId(i as u32)),
                _ => None,
            })
            .collect();
        for def in value_def_ids {
            self.result
                .impl_methods
                .entry((def, "reg".to_owned()))
                .or_insert(reg_def_id);
        }
    }

    fn alloc_def(&mut self, kind: DefKind, ident: &Identifier) -> DefId {
        let id = DefId(self.result.defs.len() as u32);
        self.result.defs.push(DefInfo {
            kind,
            name: ident.text.clone(),
            span: ident.span.clone(),
            generic_params: Vec::new(),
        });
        id
    }

    fn alloc_local(&mut self, kind: LocalKind, name: &Identifier) {
        self.result.locals.insert(
            name.id,
            LocalInfo {
                kind,
                name: name.text.clone(),
                span: name.span.clone(),
            },
        );
    }

    fn collect_item(&mut self, item: &Item) {
        // `impl` blocks extend an existing type rather than introducing a new
        // top-level name. They are handled in pass 2 (`resolve_impl`), which
        // looks up the underlying struct/port DefId.
        let (kind, ident, constructor, requires_ctor) = match item {
            Item::Fn(f) => (DefKind::Fn, &f.name, None, false),
            Item::Struct(s) => (DefKind::Struct, &s.name, s.constructor.as_ref(), true),
            Item::Port(p) => (DefKind::Port, &p.name, p.constructor.as_ref(), true),
            Item::Impl(_) => return,
        };
        if self.global_defs.contains_key(&ident.text) {
            self.result.errors.push(ResolveError {
                kind: ResolveErrorKind::DuplicateDef(ident.text.clone()),
                span: ident.span.clone(),
            });
            return;
        }
        let id = self.alloc_def(kind, ident);
        self.global_defs.insert(ident.text.clone(), (kind, id));
        self.result.resolutions.insert(ident.id, Res::Def(kind, id));

        // Register the term-level constructor as a distinct `DefKind::Ctor`
        // pointing back at the owning type. Mirrors rustc's split between
        // `DefKind::Struct` (the type) and `DefKind::Ctor` (the constructor
        // value). Struct and port definitions always require an explicit
        // constructor name to keep the type/term distinction consistent.
        match constructor {
            Some(ctor) => {
                let ctor_kind = DefKind::Ctor { owner: id };
                if self.global_defs.contains_key(&ctor.text) && ctor.text != ident.text {
                    self.result.errors.push(ResolveError {
                        kind: ResolveErrorKind::DuplicateDef(ctor.text.clone()),
                        span: ctor.span.clone(),
                    });
                } else {
                    let ctor_id = self.alloc_def(ctor_kind, ctor);
                    self.global_defs
                        .insert(ctor.text.clone(), (ctor_kind, ctor_id));
                    self.result
                        .resolutions
                        .insert(ctor.id, Res::Def(ctor_kind, ctor_id));
                }
            }
            None if requires_ctor => {
                self.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::MissingConstructor(ident.text.clone()),
                    span: ident.span.clone(),
                });
            }
            None => {}
        }
    }

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => {
                // `self` is only valid inside an impl block.
                for p in &f.parameters {
                    if p.name.text == "self" {
                        self.result.errors.push(ResolveError {
                            kind: ResolveErrorKind::SelfOutsideImpl,
                            span: p.name.span.clone(),
                        });
                    }
                }
                let Some(&(_, def_id)) = self.global_defs.get(&f.name.text) else {
                    return;
                };
                self.resolve_function(f, def_id, &HashMap::new());
            }
            Item::Struct(s) => self.resolve_struct(s),
            Item::Port(p) => self.resolve_port(p),
            Item::Impl(i) => self.resolve_impl(i),
        }
    }

    fn resolve_struct(&mut self, s: &StructDefinition) {
        let Some(&(_, def_id)) = self.global_defs.get(&s.name.text) else {
            return;
        };
        let params = self.collect_params(def_id, &[], &s.parameters);
        self.populate_generic_params(def_id, &[], &s.parameters);
        for field in &s.fields {
            self.resolve_type_expr(&field.ty, &params);
        }
    }

    fn resolve_port(&mut self, p: &PortDefinition) {
        let Some(&(_, def_id)) = self.global_defs.get(&p.name.text) else {
            return;
        };
        let params = self.collect_params(def_id, &p.named_parameters, &p.parameters);
        self.populate_generic_params(def_id, &p.named_parameters, &p.parameters);
        for field in &p.fields {
            self.resolve_type_expr(&field.ty, &params);
        }
    }

    /// Classify each declared parameter on a struct or port and record it as
    /// a `GenericParamInfo` on the owning def. The classification is:
    ///
    /// - `kind == ParamKind::Dom`  →  `GenericParamKind::Domain`
    /// - `kind == ParamKind::Param` →  `GenericParamKind::Const`
    /// - `kind == ParamKind::Value` and type head is the `Type` builtin →
    ///   `GenericParamKind::Type`
    /// - anything else → not a generic param (runtime value); skipped
    ///
    /// Named parameters come first in the list, followed by positionals — this
    /// matches `HirFn::params` ordering and what later passes expect when
    /// looking up the index of `HirTypeKind::Param(i)`.
    fn populate_generic_params(
        &mut self,
        def_id: DefId,
        named: &[NamedParameter],
        positional: &[Parameter],
    ) {
        let mut out: Vec<GenericParamInfo> = Vec::new();
        for np in named {
            if let Some(info) = self.classify_named_param(np) {
                out.push(info);
            }
        }
        for p in positional {
            if let Some(info) = self.classify_positional_param(p) {
                out.push(info);
            }
        }
        self.result.defs[def_id.0 as usize].generic_params = out;
    }

    fn classify_named_param(&self, np: &NamedParameter) -> Option<GenericParamInfo> {
        let kind = match np.kind {
            crate::surface_ir::ParamKind::Dom => GenericParamKind::Domain,
            crate::surface_ir::ParamKind::Param => GenericParamKind::Const,
            crate::surface_ir::ParamKind::Value => {
                if is_type_kind_annotation(np.ty.as_ref()) {
                    GenericParamKind::Type
                } else {
                    return None;
                }
            }
        };
        Some(GenericParamInfo {
            name: np.name.text.clone(),
            kind,
            local: np.name.id,
            span: np.name.span.clone(),
        })
    }

    fn classify_positional_param(&self, p: &Parameter) -> Option<GenericParamInfo> {
        let kind = match p.kind {
            crate::surface_ir::ParamKind::Dom => GenericParamKind::Domain,
            crate::surface_ir::ParamKind::Param => GenericParamKind::Const,
            crate::surface_ir::ParamKind::Value => {
                if is_type_kind_annotation(Some(&p.ty)) {
                    GenericParamKind::Type
                } else {
                    return None;
                }
            }
        };
        Some(GenericParamInfo {
            name: p.name.text.clone(),
            kind,
            local: p.name.id,
            span: p.name.span.clone(),
        })
    }

    fn resolve_impl(&mut self, impl_block: &ImplBlock) {
        let Some(&(kind, def_id)) = self.global_defs.get(&impl_block.name.text) else {
            self.result.errors.push(ResolveError {
                kind: ResolveErrorKind::ImplOfUnknownType(impl_block.name.text.clone()),
                span: impl_block.name.span.clone(),
            });
            return;
        };
        // Record the impl-header name resolution against the existing type.
        self.result
            .resolutions
            .insert(impl_block.name.id, Res::Def(kind, def_id));
        let impl_params =
            self.collect_params(def_id, &impl_block.named_parameters, &impl_block.parameters);
        for func in &impl_block.functions {
            // Allocate a `DefId` for the method, scoped to its owner type
            // rather than the global namespace. Two impls may define the
            // same method name on different types; both get distinct ids.
            let method_def = self.alloc_def(DefKind::Method { owner: def_id }, &func.name);
            self.result.resolutions.insert(
                func.name.id,
                Res::Def(DefKind::Method { owner: def_id }, method_def),
            );
            let prior = self
                .result
                .impl_methods
                .insert((def_id, func.name.text.clone()), method_def);
            if prior.is_some() {
                self.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::DuplicateDef(format!(
                        "{}::{}",
                        impl_block.name.text, func.name.text
                    )),
                    span: func.name.span.clone(),
                });
            }
            self.resolve_function(func, method_def, &impl_params);
        }
    }

    fn resolve_function(
        &mut self,
        func: &FunctionDefinition,
        owner: DefId,
        outer_params: &HashMap<String, NodeId>,
    ) {
        let mut params = outer_params.clone();
        for np in &func.named_parameters {
            self.alloc_local(
                LocalKind::Param {
                    owner,
                    direction: np.direction,
                },
                &np.name,
            );
            params.insert(np.name.text.clone(), np.name.id);
            self.result
                .resolutions
                .insert(np.name.id, Res::Local(np.name.id));
            if let Some(ty) = &np.ty {
                self.resolve_type_expr(ty, &params);
            }
            if let Some(default) = &np.default {
                self.resolve_expr_in_params(default, &params);
            }
        }
        for p in &func.parameters {
            self.alloc_local(
                LocalKind::Param {
                    owner,
                    direction: p.direction,
                },
                &p.name,
            );
            params.insert(p.name.text.clone(), p.name.id);
            self.result
                .resolutions
                .insert(p.name.id, Res::Local(p.name.id));
            self.resolve_type_expr(&p.ty, &params);
            if let Some(default) = &p.default {
                self.resolve_expr_in_params(default, &params);
            }
        }
        if let Some(ty) = &func.return_type {
            self.resolve_type_expr(ty, &params);
        }
        self.resolve_block(&func.body, params);
    }

    fn collect_params(
        &mut self,
        owner: DefId,
        named: &[NamedParameter],
        positional: &[Parameter],
    ) -> HashMap<String, NodeId> {
        let mut scope = HashMap::new();
        for np in named {
            self.alloc_local(
                LocalKind::Param {
                    owner,
                    direction: np.direction,
                },
                &np.name,
            );
            scope.insert(np.name.text.clone(), np.name.id);
            self.result
                .resolutions
                .insert(np.name.id, Res::Local(np.name.id));
            if let Some(ty) = &np.ty {
                self.resolve_type_expr(ty, &scope);
            }
            if let Some(default) = &np.default {
                self.resolve_expr_in_params(default, &scope);
            }
        }
        for p in positional {
            self.alloc_local(
                LocalKind::Param {
                    owner,
                    direction: p.direction,
                },
                &p.name,
            );
            scope.insert(p.name.text.clone(), p.name.id);
            self.result
                .resolutions
                .insert(p.name.id, Res::Local(p.name.id));
            self.resolve_type_expr(&p.ty, &scope);
            if let Some(default) = &p.default {
                self.resolve_expr_in_params(default, &scope);
            }
        }
        scope
    }

    fn resolve_type_expr(&mut self, ty: &TypeExpression, params: &HashMap<String, NodeId>) {
        // Type head: check params first (for type-level parameters), then global defs.
        if let Some(&id) = params.get(&ty.name.text) {
            self.result.resolutions.insert(ty.name.id, Res::Local(id));
        } else if let Some(&(kind, id)) = self.global_defs.get(&ty.name.text) {
            self.result
                .resolutions
                .insert(ty.name.id, Res::Def(kind, id));
        }
        // else: built-in type (uint, bool, Reset, …) — not in the def table
        if let Some(domain) = &ty.domain {
            if let Some(&id) = params.get(&domain.text) {
                self.result.resolutions.insert(domain.id, Res::Local(id));
            }
            // else: builtin domain name — leave for later
        }
        for suffix in &ty.suffixes {
            match suffix {
                TypeSuffix::Index(idx) => {
                    for arg in &idx.args {
                        match arg {
                            TypeArgument::Type(inner) => self.resolve_type_expr(inner, params),
                            TypeArgument::Number(_) => {}
                        }
                    }
                }
            }
        }
    }

    fn resolve_expr_in_params(&mut self, expr: &Expression, params: &HashMap<String, NodeId>) {
        match expr {
            Expression::Identifier(ident) => {
                if let Some(&id) = params.get(&ident.text) {
                    self.result.resolutions.insert(ident.id, Res::Local(id));
                } else if let Some(&(kind, id)) = self.global_defs.get(&ident.text) {
                    self.result.resolutions.insert(ident.id, Res::Def(kind, id));
                }
            }
            Expression::Binary(b) => {
                self.resolve_expr_in_params(&b.left, params);
                self.resolve_expr_in_params(&b.right, params);
            }
            Expression::Number(_) => {}
            _ => {}
        }
    }

    fn resolve_block(&mut self, block: &Block, params: HashMap<String, NodeId>) {
        let mut bctx = BlockCtx {
            ctx: self,
            params,
            var_bindings: HashMap::new(),
            let_scope: Vec::new(),
        };
        bctx.resolve(block);
    }
}

struct BlockCtx<'a> {
    ctx: &'a mut Ctx,
    /// Parameters in scope: name → NodeId of the parameter's name identifier.
    params: HashMap<String, NodeId>,
    /// Block-wide var declarations, collected by the pre-scan.
    var_bindings: HashMap<String, NodeId>,
    /// Forward-only bindings (let and implicit var from `=>`), accumulated in source order.
    /// Searched from back to front so the most recent binding wins on shadowing.
    let_scope: Vec<(String, NodeId)>,
}

impl BlockCtx<'_> {
    fn resolve(&mut self, block: &Block) {
        self.prescan_vars(block);
        for stmt in &block.statements {
            self.resolve_statement(stmt);
        }
        // Resolve the trailing expression (implicit-return tail) in the
        // same scope as the last statement.
        if let Some(tail) = &block.tail {
            self.resolve_expr(tail);
        }
    }

    fn prescan_vars(&mut self, block: &Block) {
        for stmt in &block.statements {
            let Statement::Var(v) = stmt else { continue };
            for ident in &v.names {
                if self.var_bindings.contains_key(&ident.text) {
                    self.ctx.result.errors.push(ResolveError {
                        kind: ResolveErrorKind::DuplicateVar(ident.text.clone()),
                        span: ident.span.clone(),
                    });
                } else {
                    self.ctx.alloc_local(LocalKind::Var, ident);
                    self.var_bindings.insert(ident.text.clone(), ident.id);
                    self.ctx
                        .result
                        .resolutions
                        .insert(ident.id, Res::Local(ident.id));
                }
            }
        }
    }

    fn resolve_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Var(v) => self.resolve_var_stmt(v),
            Statement::Let(l) => self.resolve_let_stmt(l),
            Statement::Assignment(a) => {
                self.resolve_expr(&a.left);
                self.resolve_expr(&a.right);
            }
            Statement::Expression(e) => self.resolve_expr(&e.value),
            Statement::Return(r) => self.resolve_expr(&r.value),
        }
    }

    fn resolve_var_stmt(&mut self, v: &VarStatement) {
        // At this textual position: a var whose name is already bound by let is an error.
        for ident in &v.names {
            if self.has_let_binding(&ident.text) {
                self.ctx.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::VarAfterLet(ident.text.clone()),
                    span: ident.span.clone(),
                });
            }
        }
        if let Some(ty) = &v.ty {
            self.resolve_type(ty);
        }
        // The var name is already in scope from the prescan, so the init
        // expression may reference the var itself (register feedback pattern).
        if let Some(init) = &v.init {
            self.resolve_expr(init);
        }
    }

    fn resolve_let_stmt(&mut self, l: &LetStatement) {
        // Resolve RHS before introducing the new binding (so `let x = x + 1` sees the old x).
        self.resolve_expr(&l.value);
        self.ctx.alloc_local(LocalKind::Let, &l.name);
        self.ctx
            .result
            .resolutions
            .insert(l.name.id, Res::Local(l.name.id));
        self.let_scope.push((l.name.text.clone(), l.name.id));
    }

    fn resolve_expr(&mut self, expr: &Expression) {
        match expr {
            Expression::Identifier(ident) => self.resolve_name_use(ident),
            Expression::Number(_) => {}
            Expression::Path(p) => {
                // Resolve the type part; the member is a field name (deferred to type checking).
                if let Some(&(kind, id)) = self.ctx.global_defs.get(&p.ty.text) {
                    self.ctx
                        .result
                        .resolutions
                        .insert(p.ty.id, Res::Def(kind, id));
                }
            }
            Expression::Binary(b) => {
                self.resolve_expr(&b.left);
                self.resolve_expr(&b.right);
            }
            Expression::Postfix(p) => {
                self.resolve_expr(&p.receiver);
                for op in &p.operations {
                    self.resolve_postfix_op(op);
                }
            }
            Expression::RecordConstructor(r) => {
                if let Some(&(kind, id)) = self.ctx.global_defs.get(&r.constructor.text) {
                    self.ctx
                        .result
                        .resolutions
                        .insert(r.constructor.id, Res::Def(kind, id));
                }
                for field in &r.fields {
                    // field.name is a struct field name — deferred to type checking
                    self.resolve_expr(&field.value);
                }
            }
            Expression::Block(b) => {
                // A block-expression is a fresh let scope: inner `let`s
                // don't leak outward. The `let_scope` is restored after the
                // block ends. `var` declarations inside still belong to the
                // enclosing function's var namespace (Polar `var`s are
                // function-scoped today; revisit when block-scoped vars
                // come up).
                let scope_start = self.let_scope.len();
                self.prescan_vars(b);
                for stmt in &b.statements {
                    self.resolve_statement(stmt);
                }
                if let Some(tail) = &b.tail {
                    self.resolve_expr(tail);
                }
                self.let_scope.truncate(scope_start);
            }
            Expression::If(if_expr) => {
                self.resolve_expr(&if_expr.condition);
                // Each branch is its own fresh let scope, independent of the
                // other and of code after the if.
                for branch in [&if_expr.then_branch, &if_expr.else_branch] {
                    let scope_start = self.let_scope.len();
                    self.prescan_vars(branch);
                    for stmt in &branch.statements {
                        self.resolve_statement(stmt);
                    }
                    if let Some(tail) = &branch.tail {
                        self.resolve_expr(tail);
                    }
                    self.let_scope.truncate(scope_start);
                }
            }
            Expression::When(when_expr) => {
                self.resolve_expr(&when_expr.event);
                let scope_start = self.let_scope.len();
                self.prescan_vars(&when_expr.body);
                for stmt in &when_expr.body.statements {
                    self.resolve_statement(stmt);
                }
                if let Some(tail) = &when_expr.body.tail {
                    self.resolve_expr(tail);
                }
                self.let_scope.truncate(scope_start);
            }
        }
    }

    fn resolve_postfix_op(&mut self, op: &PostfixOperation) {
        match op {
            PostfixOperation::Field(_) => {
                // Field name — deferred to type checking.
            }
            PostfixOperation::NamedArguments(args) => {
                for arg in &args.arguments {
                    self.resolve_named_arg(arg);
                }
            }
            PostfixOperation::Arguments(args) => {
                for arg in &args.arguments {
                    match arg {
                        PositionalArgument::Value(expr) => self.resolve_expr(expr),
                        PositionalArgument::OutBind(out) => {
                            self.resolve_out_target(&out.target);
                        }
                    }
                }
            }
        }
    }

    fn resolve_named_arg(&mut self, arg: &NamedArgument) {
        match arg {
            // arg.name is a port/param field name — deferred to type checking
            NamedArgument::Sink(s) => self.resolve_expr(&s.value),
            NamedArgument::Source(s) => self.resolve_out_target(&s.target),
        }
    }

    /// Resolve the `target` identifier of an out-arg binding (named or
    /// positional source arrow). The target must be a `var`/`ImplicitVar`,
    /// or absent — in which case we introduce a fresh implicit var. `let`
    /// bindings and definitions are rejected.
    fn resolve_out_target(&mut self, target: &Identifier) {
        match self.lookup_name(&target.text) {
            Some(Res::Local(id)) => {
                let kind = self.ctx.result.locals[&id].kind;
                match kind {
                    LocalKind::Let => {
                        self.ctx.result.errors.push(ResolveError {
                            kind: ResolveErrorKind::SourceOnLetBinding(target.text.clone()),
                            span: target.span.clone(),
                        });
                    }
                    LocalKind::Var | LocalKind::ImplicitVar => {
                        self.ctx
                            .result
                            .resolutions
                            .insert(target.id, Res::Local(id));
                    }
                    LocalKind::Param { direction, .. } => {
                        // Only `out`-direction params are writable from
                        // inside the function body; everything else is a
                        // read-only input.
                        if matches!(direction, Some(crate::surface_ir::Direction::Out)) {
                            self.ctx
                                .result
                                .resolutions
                                .insert(target.id, Res::Local(id));
                        } else {
                            self.ctx.result.errors.push(ResolveError {
                                kind: ResolveErrorKind::InvalidSourceTarget(target.text.clone()),
                                span: target.span.clone(),
                            });
                        }
                    }
                }
            }
            Some(Res::Def(..)) => {
                self.ctx.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::InvalidSourceTarget(target.text.clone()),
                    span: target.span.clone(),
                });
            }
            None => {
                // Not in scope: introduce a forward-only implicit var binding.
                self.ctx.alloc_local(LocalKind::ImplicitVar, target);
                self.ctx
                    .result
                    .resolutions
                    .insert(target.id, Res::Local(target.id));
                self.let_scope.push((target.text.clone(), target.id));
            }
        }
    }

    fn resolve_name_use(&mut self, ident: &Identifier) {
        match self.lookup_name(&ident.text) {
            Some(res) => {
                self.ctx.result.resolutions.insert(ident.id, res);
            }
            None => {
                // Built-in literal identifiers (`true`, `false`, `high`, `low`)
                // are not in any scope; HIR lowering recognises them by name
                // and emits a `Const` node. Skip the undefined-name error.
                if is_builtin_literal(&ident.text) {
                    return;
                }
                self.ctx.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::UndefinedName(ident.text.clone()),
                    span: ident.span.clone(),
                });
            }
        }
    }

    fn resolve_type(&mut self, ty: &TypeExpression) {
        if let Some(&id) = self.params.get(&ty.name.text) {
            self.ctx
                .result
                .resolutions
                .insert(ty.name.id, Res::Local(id));
        } else if let Some(&(kind, id)) = self.ctx.global_defs.get(&ty.name.text) {
            self.ctx
                .result
                .resolutions
                .insert(ty.name.id, Res::Def(kind, id));
        }
        if let Some(domain) = &ty.domain {
            if let Some(&id) = self.params.get(&domain.text) {
                self.ctx
                    .result
                    .resolutions
                    .insert(domain.id, Res::Local(id));
            }
        }
        for suffix in &ty.suffixes {
            match suffix {
                TypeSuffix::Index(idx) => {
                    for arg in &idx.args {
                        match arg {
                            TypeArgument::Type(inner) => self.resolve_type(inner),
                            TypeArgument::Number(_) => {}
                        }
                    }
                }
            }
        }
    }

    fn lookup_name(&self, name: &str) -> Option<Res> {
        for (n, id) in self.let_scope.iter().rev() {
            if n == name {
                return Some(Res::Local(*id));
            }
        }
        if let Some(&id) = self.var_bindings.get(name) {
            return Some(Res::Local(id));
        }
        if let Some(&id) = self.params.get(name) {
            return Some(Res::Local(id));
        }
        if let Some(&(kind, id)) = self.ctx.global_defs.get(name) {
            return Some(Res::Def(kind, id));
        }
        None
    }

    fn has_let_binding(&self, name: &str) -> bool {
        self.let_scope.iter().any(|(n, _)| n == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface_ir::parse_surface_source;

    fn resolve(source: &str) -> ResolveResult {
        let file = parse_surface_source(source).expect("parse failed");
        resolve_file(&file)
    }

    #[test]
    fn collects_top_level_defs() {
        // Note: `fn f(...) -> Type { }` is ambiguous in the grammar when `Type` is followed
        // directly by `{` — the parser confuses the block with a named-argument type suffix.
        // Tests use no return type or @-domain annotations to avoid this.
        let r = resolve("fn add(a: uint(8), b: uint(8)) { let r = a; }");
        let add_id = r.def_id("add").expect("`add` should be registered");
        let add = r.def_info(add_id);
        assert_eq!(add.name, "add");
        assert!(matches!(add.kind, DefKind::Fn));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn prelude_registers_reg() {
        // Empty source: prelude entries are still present.
        let r = resolve("");
        let reg_id = r.def_id("reg").expect("`reg` should be in the prelude");
        let reg = r.def_info(reg_id);
        assert_eq!(reg.name, "reg");
        assert!(matches!(reg.kind, DefKind::Fn));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn user_defs_follow_prelude() {
        // User-defined `add` allocates a DefId after the prelude's `reg`.
        let r = resolve("fn add(a: uint(8), b: uint(8)) { let r = a; }");
        let reg_id = r.def_id("reg").expect("prelude `reg`");
        let add_id = r.def_id("add").expect("user `add`");
        assert!(
            add_id.0 > reg_id.0,
            "user defs should come after prelude in the def table; got reg={reg_id:?} add={add_id:?}"
        );
    }

    #[test]
    fn user_cannot_redefine_prelude_name() {
        // Defining a `fn reg` collides with the prelude entry and surfaces as
        // a duplicate-def error.
        let r = resolve("fn reg(a: uint(8)) { let r = a; }");
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::DuplicateDef(n) if n == "reg"));
    }

    #[test]
    fn reports_duplicate_top_level_def() {
        let r = resolve(
            "fn foo(a: uint(8)) { let r = a; }\n\
             fn foo(b: uint(8)) { let r = b; }",
        );
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::DuplicateDef(n) if n == "foo"));
    }

    #[test]
    fn resolves_parameter_use() {
        let r = resolve("fn add(a: uint(8), b: uint(8)) { let r = a; }");
        assert!(r.errors.is_empty());
        let param_res = r.resolutions.values().find(|res| match res {
            Res::Local(id) => matches!(r.locals[id].kind, LocalKind::Param { .. }),
            _ => false,
        });
        assert!(
            param_res.is_some(),
            "expected at least one param resolution"
        );
    }

    #[test]
    fn resolves_let_binding() {
        let r = resolve("fn f(x: uint(8)) { let y = x; }");
        assert!(r.errors.is_empty());
        assert!(
            r.locals
                .values()
                .any(|b| b.name == "y" && matches!(b.kind, LocalKind::Let))
        );
    }

    #[test]
    fn let_shadows_let() {
        let r = resolve("fn f(x: uint(8)) { let x = x; }");
        assert!(r.errors.is_empty());
        let let_xs: Vec<_> = r
            .locals
            .values()
            .filter(|b| b.name == "x" && matches!(b.kind, LocalKind::Let))
            .collect();
        assert_eq!(let_xs.len(), 1);
    }

    #[test]
    fn resolves_var_with_block_wide_scope() {
        // var is used in the assignment before the var declaration appears in source
        let r = resolve("fn f() { count = count; var count: uint(8); }");
        assert!(r.errors.is_empty());
        assert!(
            r.locals
                .values()
                .any(|b| b.name == "count" && matches!(b.kind, LocalKind::Var))
        );
    }

    #[test]
    fn reports_var_after_let() {
        let r = resolve("fn f(x: uint(8)) { let y = x; var y; }");
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::VarAfterLet(n) if n == "y"));
    }

    #[test]
    fn reports_duplicate_var() {
        let r = resolve("fn f() { var x: uint(8); var x: uint(8); }");
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::DuplicateVar(n) if n == "x"));
    }

    #[test]
    fn reports_undefined_name() {
        let r = resolve("fn f() { let x = y; }");
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::UndefinedName(n) if n == "y"));
    }

    #[test]
    fn introduces_implicit_var_from_source_arrow() {
        // `output => out_df` where `out_df` is not in scope should introduce an implicit var
        let r = resolve(
            "fn producer() { }\n\
             fn consumer(inp: uint(8)) { producer { output => out_df }(); let _ = out_df; }",
        );
        // out_df should be introduced as ImplicitVar and be resolvable by the subsequent let
        assert!(r.errors.is_empty());
        assert!(
            r.locals
                .values()
                .any(|b| b.name == "out_df" && matches!(b.kind, LocalKind::ImplicitVar))
        );
    }

    #[test]
    fn reports_source_on_let_binding() {
        let r = resolve(
            "fn producer() { }\n\
             fn consumer(inp: uint(8)) { let x = inp; producer { output => x }(); }",
        );
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::SourceOnLetBinding(n) if n == "x"));
    }

    #[test]
    fn resolves_var_as_source_target() {
        let r = resolve(
            "fn producer() { }\n\
             fn consumer() { var x: uint(8); producer { output => x }(); }",
        );
        assert!(r.errors.is_empty());
    }

    #[test]
    fn resolves_var_with_inline_initializer() {
        // var acc = acc + 1; — the init may reference the var itself (feedback)
        let r = resolve("fn f(x: uint(8)) { var acc = acc + x; }");
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
        assert!(
            r.locals
                .values()
                .any(|b| b.name == "acc" && matches!(b.kind, LocalKind::Var))
        );
    }

    #[test]
    fn var_initializer_resolves_names_in_scope() {
        // The init expression sees params and other vars.
        let r = resolve("fn f(x: uint(8)) { var a: uint(8); var b = a + x; }");
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn var_initializer_reports_undefined_name() {
        let r = resolve("fn f() { var acc = acc + missing; }");
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::UndefinedName(n) if n == "missing"));
    }

    // --- error message text ---

    #[test]
    fn error_message_undefined_name() {
        let r = resolve("fn f() { let x = y; }");
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].kind.to_string(), "undefined name `y`");
    }

    #[test]
    fn error_message_duplicate_def() {
        let r = resolve(
            "fn foo(a: uint(8)) { let r = a; }\n\
             fn foo(b: uint(8)) { let r = b; }",
        );
        assert_eq!(r.errors.len(), 1);
        assert_eq!(
            r.errors[0].kind.to_string(),
            "`foo` is defined more than once in this file"
        );
    }

    #[test]
    fn error_message_duplicate_var() {
        let r = resolve("fn f() { var x: uint(8); var x: uint(8); }");
        assert_eq!(r.errors.len(), 1);
        assert_eq!(
            r.errors[0].kind.to_string(),
            "`x` is declared more than once as `var` in this block"
        );
    }

    #[test]
    fn error_message_var_after_let() {
        let r = resolve("fn f(x: uint(8)) { let y = x; var y; }");
        assert_eq!(r.errors.len(), 1);
        assert_eq!(
            r.errors[0].kind.to_string(),
            "cannot declare `var y` after a `let y` binding in the same block"
        );
    }

    #[test]
    fn error_message_source_on_let_binding() {
        let r = resolve(
            "fn producer() { }\n\
             fn consumer(inp: uint(8)) { let x = inp; producer { output => x }(); }",
        );
        assert_eq!(r.errors.len(), 1);
        assert_eq!(
            r.errors[0].kind.to_string(),
            "`x` is a `let` binding; source arrows (`=>`) require a `var` signal"
        );
    }

    #[test]
    fn error_message_invalid_source_target() {
        let r = resolve(
            "fn producer() { }\n\
             fn consumer(inp: uint(8)) { producer { output => inp }(); }",
        );
        assert_eq!(r.errors.len(), 1);
        assert_eq!(
            r.errors[0].kind.to_string(),
            "`inp` is not a valid source arrow target; only `var` signals may be wired this way"
        );
    }

    // --- generic params / Ctor ---

    #[test]
    fn struct_constructor_is_separate_ctor_def() {
        let r = resolve("struct Packet = packet { valid: bool, payload: uint(8) }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        let packet_id = r.def_id("Packet").expect("type def");
        let ctor_id = r.def_id("packet").expect("ctor def");
        assert_ne!(packet_id, ctor_id, "ctor must be a separate DefId");
        assert!(matches!(r.def_info(packet_id).kind, DefKind::Struct));
        assert!(matches!(r.def_info(ctor_id).kind, DefKind::Ctor { owner } if owner == packet_id));
    }

    #[test]
    fn struct_without_constructor_is_error() {
        let r = resolve("struct Packet { valid: bool, payload: uint(8) }");
        assert_eq!(r.errors.len(), 1, "errors: {:?}", r.errors);
        assert!(matches!(
            &r.errors[0].kind,
            ResolveErrorKind::MissingConstructor(n) if n == "Packet"
        ));
    }

    #[test]
    fn struct_records_type_generic_param() {
        let r = resolve("struct Bus(A: Type) = bus { valid: bool, data: A }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        let bus = r.def_info(r.def_id("Bus").unwrap());
        assert_eq!(bus.generic_params.len(), 1);
        assert_eq!(bus.generic_params[0].name, "A");
        assert_eq!(bus.generic_params[0].kind, GenericParamKind::Type);
    }

    #[test]
    fn port_records_named_dom_and_positional_type_params() {
        let r = resolve(
            "port DF { dom clk: Clock } ( A: Type ) = df {\n\
                 in ready: bool @clk,\n\
                 out valid: bool @clk,\n\
                 out data: A @clk,\n\
             }",
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        let df = r.def_info(r.def_id("DF").unwrap());
        assert_eq!(df.generic_params.len(), 2);
        assert_eq!(df.generic_params[0].name, "clk");
        assert_eq!(df.generic_params[0].kind, GenericParamKind::Domain);
        assert_eq!(df.generic_params[1].name, "A");
        assert_eq!(df.generic_params[1].kind, GenericParamKind::Type);
    }

    // --- example file integration tests ---

    fn resolve_file_source(source: &str) -> ResolveResult {
        let file = parse_surface_source(source).expect("parse failed");
        resolve_file(&file)
    }

    #[test]
    fn resolves_example_file() {
        let source = include_str!("../../../examples/working/mult_add.plr");
        let r = resolve_file_source(source);
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn resolves_working_examples() {
        for (name, source) in crate::test_support::working_examples() {
            let r = resolve_file_source(&source);
            assert!(
                r.errors.is_empty(),
                "example `{name}` had unexpected resolve errors: {:?}",
                r.errors
            );
        }
    }

    #[test]
    fn name_resolution_fail_undefined_name() {
        let source = include_str!("../../../examples/fail-expected/undefined-name.plr");
        let r = resolve_file_source(source);
        assert_eq!(r.errors.len(), 1, "errors: {:?}", r.errors);
        assert!(
            matches!(&r.errors[0].kind, ResolveErrorKind::UndefinedName(n) if n == "offset"),
            "got: {}",
            r.errors[0].kind
        );
        assert_eq!(r.errors[0].kind.to_string(), "undefined name `offset`");
    }

    #[test]
    fn name_resolution_fail_duplicate_def() {
        let source = include_str!("../../../examples/fail-expected/duplicate-def.plr");
        let r = resolve_file_source(source);
        assert_eq!(r.errors.len(), 1, "errors: {:?}", r.errors);
        assert!(
            matches!(&r.errors[0].kind, ResolveErrorKind::DuplicateDef(n) if n == "process"),
            "got: {}",
            r.errors[0].kind
        );
        assert_eq!(
            r.errors[0].kind.to_string(),
            "`process` is defined more than once in this file"
        );
    }

    #[test]
    fn name_resolution_fail_duplicate_var() {
        let source = include_str!("../../../examples/fail-expected/duplicate-var.plr");
        let r = resolve_file_source(source);
        assert_eq!(r.errors.len(), 1, "errors: {:?}", r.errors);
        assert!(
            matches!(&r.errors[0].kind, ResolveErrorKind::DuplicateVar(n) if n == "count"),
            "got: {}",
            r.errors[0].kind
        );
        assert_eq!(
            r.errors[0].kind.to_string(),
            "`count` is declared more than once as `var` in this block"
        );
    }

    #[test]
    fn name_resolution_fail_var_after_let() {
        let source = include_str!("../../../examples/fail-expected/var-after-let.plr");
        let r = resolve_file_source(source);
        assert_eq!(r.errors.len(), 1, "errors: {:?}", r.errors);
        assert!(
            matches!(&r.errors[0].kind, ResolveErrorKind::VarAfterLet(n) if n == "acc"),
            "got: {}",
            r.errors[0].kind
        );
        assert_eq!(
            r.errors[0].kind.to_string(),
            "cannot declare `var acc` after a `let acc` binding in the same block"
        );
    }
}
