use std::collections::HashMap;

use crate::surface_ir::{
    Block, ComponentDefinition, Expression, FunctionDefinition, ImplBlock, Item, LetStatement,
    NamedArgument, NamedParameter, NodeId, Parameter, PortDefinition, PostfixOperation,
    SourceArgument, SourceFile, Statement, StructDefinition, TypeExpression, TypeSuffix,
    VarStatement,
};
use crate::{Identifier, SourceSpan};

/// Unique ID for a top-level definition (component, struct, port, impl).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DefId(pub u32);

/// Unique ID for a parameter (named or positional) within a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamId(pub u32);

/// Unique ID for a local binding (let, var, or implicit var) within a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingId(pub u32);

/// What an identifier resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Res {
    Def(DefId),
    Param(ParamId),
    Local(BindingId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefKind {
    Component,
    Struct,
    Port,
    Impl,
}

/// How a local binding was introduced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingKind {
    Let,
    /// Explicit `var x` declaration — block-wide scope via pre-scan.
    Var,
    /// Introduced by `output => x` when `x` was not already in scope — forward-only scope.
    ImplicitVar,
}

#[derive(Debug, Clone)]
pub struct DefInfo {
    pub kind: DefKind,
    pub name: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: String,
    pub span: SourceSpan,
    pub owner: DefId,
}

#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub kind: BindingKind,
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
}

/// The output of name resolution.
#[derive(Debug, Default)]
pub struct ResolveResult {
    /// Maps each identifier use-site (by its `NodeId`) to what it resolves to.
    pub resolutions: HashMap<NodeId, Res>,
    pub errors: Vec<ResolveError>,
    /// Info for each top-level definition, indexed by DefId.0.
    pub defs: Vec<DefInfo>,
    /// Info for each parameter, indexed by ParamId.0.
    pub params: Vec<ParamInfo>,
    /// Info for each local binding, indexed by BindingId.0.
    pub bindings: Vec<BindingInfo>,
}

impl ResolveResult {
    pub fn def_info(&self, id: DefId) -> &DefInfo {
        &self.defs[id.0 as usize]
    }

    pub fn param_info(&self, id: ParamId) -> &ParamInfo {
        &self.params[id.0 as usize]
    }

    pub fn binding_info(&self, id: BindingId) -> &BindingInfo {
        &self.bindings[id.0 as usize]
    }
}

pub fn resolve_file(file: &SourceFile) -> ResolveResult {
    let mut ctx = Ctx::default();

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
    global_defs: HashMap<String, DefId>,
}

impl Ctx {
    fn alloc_def(&mut self, kind: DefKind, ident: &Identifier) -> DefId {
        let id = DefId(self.result.defs.len() as u32);
        self.result.defs.push(DefInfo {
            kind,
            name: ident.text.clone(),
            span: ident.span.clone(),
        });
        id
    }

    fn alloc_param(&mut self, name: &Identifier, owner: DefId) -> ParamId {
        let id = ParamId(self.result.params.len() as u32);
        self.result.params.push(ParamInfo {
            name: name.text.clone(),
            span: name.span.clone(),
            owner,
        });
        id
    }

    fn alloc_binding(&mut self, kind: BindingKind, name: &Identifier) -> BindingId {
        let id = BindingId(self.result.bindings.len() as u32);
        self.result.bindings.push(BindingInfo {
            kind,
            name: name.text.clone(),
            span: name.span.clone(),
        });
        id
    }

    fn collect_item(&mut self, item: &Item) {
        let (kind, ident) = match item {
            Item::Component(c) => (DefKind::Component, &c.name),
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
            self.global_defs.insert(ident.text.clone(), id);
            self.result.resolutions.insert(ident.id, Res::Def(id));
        }
    }

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Component(c) => self.resolve_component(c),
            Item::Struct(s) => self.resolve_struct(s),
            Item::Port(p) => self.resolve_port(p),
            Item::Impl(i) => self.resolve_impl(i),
        }
    }

    fn resolve_component(&mut self, comp: &ComponentDefinition) {
        let Some(&def_id) = self.global_defs.get(&comp.name.text) else {
            return;
        };
        let params = self.collect_params(def_id, &comp.named_parameters, &comp.parameters);
        if let Some(ty) = &comp.return_type {
            self.resolve_type_expr(ty, &params);
        }
        self.resolve_block(&comp.body, params);
    }

    fn resolve_struct(&mut self, s: &StructDefinition) {
        let Some(&def_id) = self.global_defs.get(&s.name.text) else {
            return;
        };
        let params = self.collect_params(def_id, &[], &s.parameters);
        for field in &s.fields {
            self.resolve_type_expr(&field.ty, &params);
        }
    }

    fn resolve_port(&mut self, p: &PortDefinition) {
        let Some(&def_id) = self.global_defs.get(&p.name.text) else {
            return;
        };
        let params = self.collect_params(def_id, &p.named_parameters, &p.parameters);
        for field in &p.fields {
            self.resolve_type_expr(&field.ty, &params);
        }
    }

    fn resolve_impl(&mut self, impl_block: &ImplBlock) {
        let Some(&def_id) = self.global_defs.get(&impl_block.name.text) else {
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
        outer_params: &HashMap<String, ParamId>,
    ) {
        let mut params = outer_params.clone();
        for np in &func.named_parameters {
            let id = self.alloc_param(&np.name, owner);
            params.insert(np.name.text.clone(), id);
            self.result.resolutions.insert(np.name.id, Res::Param(id));
        }
        for p in &func.parameters {
            let id = self.alloc_param(&p.name, owner);
            params.insert(p.name.text.clone(), id);
            self.result.resolutions.insert(p.name.id, Res::Param(id));
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
    ) -> HashMap<String, ParamId> {
        let mut scope = HashMap::new();
        for np in named {
            let id = self.alloc_param(&np.name, owner);
            scope.insert(np.name.text.clone(), id);
            self.result.resolutions.insert(np.name.id, Res::Param(id));
            if let Some(ty) = &np.ty {
                self.resolve_type_expr(ty, &scope);
            }
            if let Some(default) = &np.default {
                self.resolve_expr_in_params(default, &scope);
            }
        }
        for p in positional {
            let id = self.alloc_param(&p.name, owner);
            scope.insert(p.name.text.clone(), id);
            self.result.resolutions.insert(p.name.id, Res::Param(id));
            self.resolve_type_expr(&p.ty, &scope);
            if let Some(default) = &p.default {
                self.resolve_expr_in_params(default, &scope);
            }
        }
        scope
    }

    fn resolve_type_expr(&mut self, ty: &TypeExpression, params: &HashMap<String, ParamId>) {
        // Type head: check params first (for type-level parameters), then global defs.
        if let Some(&id) = params.get(&ty.name.text) {
            self.result.resolutions.insert(ty.name.id, Res::Param(id));
        } else if let Some(&id) = self.global_defs.get(&ty.name.text) {
            self.result.resolutions.insert(ty.name.id, Res::Def(id));
        }
        // else: built-in type (uint, bool, Reset, …) — not in the def table
        if let Some(domain) = &ty.domain {
            if let Some(&id) = params.get(&domain.text) {
                self.result.resolutions.insert(domain.id, Res::Param(id));
            }
            // else: builtin domain name — leave for later
        }
        for suffix in &ty.suffixes {
            match suffix {
                TypeSuffix::Index(idx) => self.resolve_expr_in_params(&idx.index, params),
                TypeSuffix::Arguments(args) => {
                    for arg in &args.arguments {
                        self.resolve_type_expr(arg, params);
                    }
                }
                TypeSuffix::NamedArguments(_) => {}
            }
        }
    }

    fn resolve_expr_in_params(&mut self, expr: &Expression, params: &HashMap<String, ParamId>) {
        match expr {
            Expression::Identifier(ident) => {
                if let Some(&id) = params.get(&ident.text) {
                    self.result.resolutions.insert(ident.id, Res::Param(id));
                } else if let Some(&id) = self.global_defs.get(&ident.text) {
                    self.result.resolutions.insert(ident.id, Res::Def(id));
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

    fn resolve_block(&mut self, block: &Block, params: HashMap<String, ParamId>) {
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
    params: HashMap<String, ParamId>,
    /// Block-wide var declarations, collected by the pre-scan.
    var_bindings: HashMap<String, BindingId>,
    /// Forward-only bindings (let and implicit var from `=>`), accumulated in source order.
    /// Searched from back to front so the most recent binding wins on shadowing.
    let_scope: Vec<(String, BindingId)>,
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
                    let id = self.ctx.alloc_binding(BindingKind::Var, ident);
                    self.var_bindings.insert(ident.text.clone(), id);
                    self.ctx.result.resolutions.insert(ident.id, Res::Local(id));
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
    }

    fn resolve_let_stmt(&mut self, l: &LetStatement) {
        // Resolve RHS before introducing the new binding (so `let x = x + 1` sees the old x).
        self.resolve_expr(&l.value);
        let id = self.ctx.alloc_binding(BindingKind::Let, &l.name);
        self.ctx.result.resolutions.insert(l.name.id, Res::Local(id));
        self.let_scope.push((l.name.text.clone(), id));
    }

    fn resolve_expr(&mut self, expr: &Expression) {
        match expr {
            Expression::Identifier(ident) => self.resolve_name_use(ident),
            Expression::Number(_) => {}
            Expression::Path(p) => {
                // Resolve the type part; the member is a field name (deferred to type checking).
                if let Some(&id) = self.ctx.global_defs.get(&p.ty.text) {
                    self.ctx.result.resolutions.insert(p.ty.id, Res::Def(id));
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
                if let Some(&id) = self.ctx.global_defs.get(&r.constructor.text) {
                    self.ctx
                        .result
                        .resolutions
                        .insert(r.constructor.id, Res::Def(id));
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
            PostfixOperation::Slice(s) => {
                self.resolve_expr(&s.start);
                self.resolve_expr(&s.end);
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
                let kind = self.ctx.result.bindings[id.0 as usize].kind.clone();
                match kind {
                    BindingKind::Let => {
                        self.ctx.result.errors.push(ResolveError {
                            kind: ResolveErrorKind::SourceOnLetBinding(s.target.text.clone()),
                            span: s.target.span.clone(),
                        });
                    }
                    BindingKind::Var | BindingKind::ImplicitVar => {
                        self.ctx
                            .result
                            .resolutions
                            .insert(s.target.id, Res::Local(id));
                    }
                }
            }
            Some(Res::Param(_) | Res::Def(_)) => {
                self.ctx.result.errors.push(ResolveError {
                    kind: ResolveErrorKind::InvalidSourceTarget(s.target.text.clone()),
                    span: s.target.span.clone(),
                });
            }
            None => {
                // Not in scope: introduce a forward-only implicit var binding.
                let id = self.ctx.alloc_binding(BindingKind::ImplicitVar, &s.target);
                self.ctx
                    .result
                    .resolutions
                    .insert(s.target.id, Res::Local(id));
                self.let_scope.push((s.target.text.clone(), id));
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
            self.ctx.result.resolutions.insert(ty.name.id, Res::Param(id));
        } else if let Some(&id) = self.ctx.global_defs.get(&ty.name.text) {
            self.ctx.result.resolutions.insert(ty.name.id, Res::Def(id));
        }
        if let Some(domain) = &ty.domain {
            if let Some(&id) = self.params.get(&domain.text) {
                self.ctx
                    .result
                    .resolutions
                    .insert(domain.id, Res::Param(id));
            }
        }
        for suffix in &ty.suffixes {
            match suffix {
                TypeSuffix::Index(idx) => self.resolve_expr(&idx.index),
                TypeSuffix::Arguments(args) => {
                    for arg in &args.arguments {
                        self.resolve_type(arg);
                    }
                }
                TypeSuffix::NamedArguments(_) => {}
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
            return Some(Res::Param(id));
        }
        if let Some(&id) = self.ctx.global_defs.get(name) {
            return Some(Res::Def(id));
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
        let r = resolve("fn add(a: uint[8], b: uint[8]) { let r = a; }");
        assert_eq!(r.defs.len(), 1);
        assert_eq!(r.defs[0].name, "add");
        assert!(matches!(r.defs[0].kind, DefKind::Component));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn reports_duplicate_top_level_def() {
        let r = resolve(
            "fn foo(a: uint[8]) { let r = a; }\n\
             fn foo(b: uint[8]) { let r = b; }",
        );
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::DuplicateDef(n) if n == "foo"));
    }

    #[test]
    fn resolves_parameter_use() {
        let r = resolve("fn add(a: uint[8], b: uint[8]) { let r = a; }");
        assert!(r.errors.is_empty());
        let param_res = r.resolutions.values().find(|res| matches!(res, Res::Param(_)));
        assert!(param_res.is_some(), "expected at least one param resolution");
    }

    #[test]
    fn resolves_let_binding() {
        let r = resolve("fn f(x: uint[8]) { let y = x; }");
        assert!(r.errors.is_empty());
        assert!(r.bindings.iter().any(|b| b.name == "y" && matches!(b.kind, BindingKind::Let)));
    }

    #[test]
    fn let_shadows_let() {
        let r = resolve("fn f(x: uint[8]) { let x = x; }");
        assert!(r.errors.is_empty());
        let let_xs: Vec<_> = r
            .bindings
            .iter()
            .filter(|b| b.name == "x" && matches!(b.kind, BindingKind::Let))
            .collect();
        assert_eq!(let_xs.len(), 1);
    }

    #[test]
    fn resolves_var_with_block_wide_scope() {
        // var is used in the assignment before the var declaration appears in source
        let r = resolve(
            "fn f() { count = count; var count: uint[8]; }",
        );
        assert!(r.errors.is_empty());
        assert!(r.bindings.iter().any(|b| b.name == "count" && matches!(b.kind, BindingKind::Var)));
    }

    #[test]
    fn reports_var_after_let() {
        let r = resolve(
            "fn f(x: uint[8]) { let y = x; var y; }",
        );
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(&r.errors[0].kind, ResolveErrorKind::VarAfterLet(n) if n == "y"));
    }

    #[test]
    fn reports_duplicate_var() {
        let r = resolve(
            "fn f() { var x: uint[8]; var x: uint[8]; }",
        );
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
             fn consumer(inp: uint[8]) { producer { output => out_df }(); let _ = out_df; }",
        );
        // out_df should be introduced as ImplicitVar and be resolvable by the subsequent let
        assert!(r.errors.is_empty());
        assert!(
            r.bindings
                .iter()
                .any(|b| b.name == "out_df" && matches!(b.kind, BindingKind::ImplicitVar))
        );
    }

    #[test]
    fn reports_source_on_let_binding() {
        let r = resolve(
            "fn producer() { }\n\
             fn consumer(inp: uint[8]) { let x = inp; producer { output => x }(); }",
        );
        assert_eq!(r.errors.len(), 1);
        assert!(
            matches!(&r.errors[0].kind, ResolveErrorKind::SourceOnLetBinding(n) if n == "x")
        );
    }

    #[test]
    fn resolves_var_as_source_target() {
        let r = resolve(
            "fn producer() { }\n\
             fn consumer() { var x: uint[8]; producer { output => x }(); }",
        );
        assert!(r.errors.is_empty());
    }

    #[test]
    fn resolves_example_file() {
        let source = include_str!("../../../examples/mult_add.plr");
        let file = parse_surface_source(source).unwrap();
        let r = resolve_file(&file);
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }
}
