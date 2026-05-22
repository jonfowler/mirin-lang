use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::surface_ir::{
    Block, Expression, FunctionDefinition, ImplBlock, Item, LetStatement, NamedArgument,
    NamedParameter, NodeId, Parameter, PortDefinition, PostfixOperation, SourceArgument,
    SourceFile, Statement, StructDefinition, TypeExpression, TypeSuffix, VarStatement,
};
use crate::{Identifier, SourceExcerpt, SourcePosition, SourceSpan};

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
}

/// How a local binding was introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// Parameter (named or positional) of the owning def.
    Param { owner: DefId },
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
/// as user-defined functions. See `planning/hir.md` ("Prelude").
const PRELUDE_FN_NAMES: &[&str] = &["reg"];

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
            });
            ctx.global_defs.insert(name.to_owned(), (DefKind::Fn, id));
        }
        ctx
    }

    fn alloc_def(&mut self, kind: DefKind, ident: &Identifier) -> DefId {
        let id = DefId(self.result.defs.len() as u32);
        self.result.defs.push(DefInfo {
            kind,
            name: ident.text.clone(),
            span: ident.span.clone(),
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
        let (kind, ident) = match item {
            Item::Fn(f) => (DefKind::Fn, &f.name),
            Item::Struct(s) => (DefKind::Struct, &s.name),
            Item::Port(p) => (DefKind::Port, &p.name),
            Item::Impl(i) => (DefKind::Impl, &i.name),
        };
        if self.global_defs.contains_key(&ident.text) {
            self.result.errors.push(ResolveError {
                kind: ResolveErrorKind::DuplicateDef(ident.text.clone()),
                span: ident.span.clone(),
            });
        } else {
            let id = self.alloc_def(kind, ident);
            self.global_defs.insert(ident.text.clone(), (kind, id));
            self.result.resolutions.insert(ident.id, Res::Def(kind, id));
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
        for field in &s.fields {
            self.resolve_type_expr(&field.ty, &params);
        }
    }

    fn resolve_port(&mut self, p: &PortDefinition) {
        let Some(&(_, def_id)) = self.global_defs.get(&p.name.text) else {
            return;
        };
        let params = self.collect_params(def_id, &p.named_parameters, &p.parameters);
        for field in &p.fields {
            self.resolve_type_expr(&field.ty, &params);
        }
    }

    fn resolve_impl(&mut self, impl_block: &ImplBlock) {
        let Some(&(_, def_id)) = self.global_defs.get(&impl_block.name.text) else {
            return;
        };
        let impl_params =
            self.collect_params(def_id, &impl_block.named_parameters, &impl_block.parameters);
        for func in &impl_block.functions {
            self.resolve_function(func, def_id, &impl_params);
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
            self.alloc_local(LocalKind::Param { owner }, &np.name);
            params.insert(np.name.text.clone(), np.name.id);
            self.result
                .resolutions
                .insert(np.name.id, Res::Local(np.name.id));
        }
        for p in &func.parameters {
            self.alloc_local(LocalKind::Param { owner }, &p.name);
            params.insert(p.name.text.clone(), p.name.id);
            self.result
                .resolutions
                .insert(p.name.id, Res::Local(p.name.id));
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
            self.alloc_local(LocalKind::Param { owner }, &np.name);
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
            self.alloc_local(LocalKind::Param { owner }, &p.name);
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
                TypeSuffix::Index(idx) => self.resolve_expr_in_params(&idx.index, params),
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
                for expr in &args.arguments {
                    self.resolve_expr(expr);
                }
            }
        }
    }

    fn resolve_named_arg(&mut self, arg: &NamedArgument) {
        match arg {
            // arg.name is a port/param field name — deferred to type checking
            NamedArgument::Sink(s) => self.resolve_expr(&s.value),
            NamedArgument::Source(s) => self.resolve_source_target(s),
        }
    }

    fn resolve_source_target(&mut self, s: &SourceArgument) {
        match self.lookup_name(&s.target.text) {
            Some(Res::Local(id)) => {
                let kind = self.ctx.result.locals[&id].kind;
                match kind {
                    LocalKind::Let => {
                        self.ctx.result.errors.push(ResolveError {
                            kind: ResolveErrorKind::SourceOnLetBinding(s.target.text.clone()),
                            span: s.target.span.clone(),
                        });
                    }
                    LocalKind::Var | LocalKind::ImplicitVar => {
                        self.ctx
                            .result
                            .resolutions
                            .insert(s.target.id, Res::Local(id));
                    }
                    LocalKind::Param { .. } => {
                        self.ctx.result.errors.push(ResolveError {
                            kind: ResolveErrorKind::InvalidSourceTarget(s.target.text.clone()),
                            span: s.target.span.clone(),
                        });
                    }
                }
            }
            Some(Res::Def(..)) => {
                self.ctx.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::InvalidSourceTarget(s.target.text.clone()),
                    span: s.target.span.clone(),
                });
            }
            None => {
                // Not in scope: introduce a forward-only implicit var binding.
                self.ctx.alloc_local(LocalKind::ImplicitVar, &s.target);
                self.ctx
                    .result
                    .resolutions
                    .insert(s.target.id, Res::Local(s.target.id));
                self.let_scope.push((s.target.text.clone(), s.target.id));
            }
        }
    }

    fn resolve_name_use(&mut self, ident: &Identifier) {
        match self.lookup_name(&ident.text) {
            Some(res) => {
                self.ctx.result.resolutions.insert(ident.id, res);
            }
            None => {
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
                TypeSuffix::Index(idx) => self.resolve_expr(&idx.index),
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

    // --- example file integration tests ---

    fn resolve_file_source(source: &str) -> ResolveResult {
        let file = parse_surface_source(source).expect("parse failed");
        resolve_file(&file)
    }

    #[test]
    fn resolves_example_file() {
        let source = include_str!("../../../examples/mult_add.plr");
        let r = resolve_file_source(source);
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn resolves_success_examples() {
        let examples: &[(&str, &str)] = &[
            (
                "add_constant",
                include_str!("../../../examples/add_constant.plr"),
            ),
            (
                "accumulator",
                include_str!("../../../examples/accumulator.plr"),
            ),
            ("counter", include_str!("../../../examples/counter.plr")),
            ("mult_add", include_str!("../../../examples/mult_add.plr")),
            ("pipeline", include_str!("../../../examples/pipeline.plr")),
            (
                "shift_register",
                include_str!("../../../examples/shift_register.plr"),
            ),
        ];
        for (name, source) in examples {
            let r = resolve_file_source(source);
            assert!(
                r.errors.is_empty(),
                "example `{name}` had unexpected resolve errors: {:?}",
                r.errors
            );
        }
    }

    #[test]
    fn name_resolution_fail_undefined_name() {
        let source = include_str!("../../../fail-examples/undefined-name.plr");
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
        let source = include_str!("../../../fail-examples/duplicate-def.plr");
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
        let source = include_str!("../../../fail-examples/duplicate-var.plr");
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
        let source = include_str!("../../../fail-examples/var-after-let.plr");
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
