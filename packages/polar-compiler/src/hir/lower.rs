//! Surface IR → HIR lowering.
//!
//! Runs after name resolution and direction checking. Does three things in a
//! single walk per `planning/hir.md`:
//!
//! - Bakes name resolutions into HIR nodes (no more side-table lookups by
//!   later passes).
//! - Desugars method-style calls (`x.f(args)`) into uniform `HirCall`s with
//!   the receiver slotted into the callee's `self` parameter.
//! - Slots named-and-positional arguments against the callee's signature,
//!   substituting declared defaults and marking inferable parameters.
//!
//! Things this pass does **not** do (deferred to later passes):
//!
//! - Allocate inference variables for untyped slots. `HirExpr::ty` and
//!   `HirVarDecl::ty` stay `None`.
//! - Resolve inferable arguments. `#clk` slots become `HirArg::Inferable`.
//! - Const-evaluate widths. `uint(N)` keeps `N` as an `HirExpr`.

use std::collections::HashMap;
use std::fmt;

use super::{
    ConstValue, Domain, HirArg, HirArgSource, HirBlock, HirCall, HirEquation, HirExpr, HirExprKind,
    HirFieldAccess, HirFn, HirId, HirItem, HirLet, HirLocalInfo, HirParam, HirPort, HirPortField,
    HirSourceFile, HirStmt, HirStruct, HirStructField, HirType, HirTypeKind, HirVarDecl, LocalId,
    ParamKind as HirParamKind, ParamSection, PortTypeRef, ValueKind, ValueType,
};
use crate::SourceSpan;
use crate::resolve::{DefId, DefKind, LocalKind, Res, ResolveResult};
use crate::surface_ir::{
    AssignmentStatement, BinaryOperator, Block, Expression, FunctionDefinition, Item, LetStatement,
    NamedArgument, NamedParameter, NodeId, ParamKind, Parameter, PortDefinition,
    PositionalArgument, PostfixExpression, PostfixOperation, RecordConstructorExpression,
    ReturnStatement, SourceFile, Statement, StructDefinition, TypeExpression, TypeSuffix,
    VarStatement,
};

#[derive(Debug, Clone)]
pub struct HirLowerError {
    pub kind: HirLowerErrorKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirLowerErrorKind {
    /// A required argument has no value and no default.
    MissingRequiredArgument { callee: String, param: String },
    /// More positional arguments were given than the callee accepts.
    TooManyPositionalArgs {
        callee: String,
        expected: usize,
        got: usize,
    },
    /// A method call references a method name that is not in the prelude.
    UnknownMethod { method: String },
    /// A bare definition was used in a value position (functions are not
    /// first-class values in the first pass).
    DefAsValue { name: String },
    /// A surface construct that is out of scope for the first-pass HIR.
    Unsupported { what: &'static str },
    /// An integer literal that didn't parse.
    InvalidNumber(String),
    /// An unrecognized type-head name.
    UnknownType(String),
    /// An `@domain` annotation appears on a type that does not carry a
    /// top-level domain (e.g. `Clock @x`).
    DomainOnNonValueType { ty: &'static str },
    /// An `@domain` annotation appears on a port type. Ports take their clock
    /// binding via type arguments (e.g. `Stream8(clk)`, syntax pending), not
    /// via a domain annotation. Reported when the name resolves to a port
    /// def — the annotation is fine on structs and primitives.
    DomainOnPortType { port: String },
    /// A record constructor names something other than a struct constructor.
    RecordConstructorNotStruct { name: String },
    /// A record constructor mentions a field the struct does not declare.
    UnknownStructField { struct_name: String, field: String },
    /// A record constructor omits a required field.
    MissingStructField { struct_name: String, field: String },
    /// A record constructor mentions the same field twice.
    DuplicateStructField { field: String },
    /// The assignment LHS resolves to something other than a `var` signal.
    AssignmentLhsNotVar,
    /// An expected resolver entry was missing. Should not happen on a clean
    /// resolution; treated as a defensive bug-report.
    InternalUnresolved(String),
}

impl fmt::Display for HirLowerErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequiredArgument { callee, param } => {
                write!(
                    f,
                    "missing required argument `{param}` in call to `{callee}`"
                )
            }
            Self::TooManyPositionalArgs {
                callee,
                expected,
                got,
            } => write!(
                f,
                "`{callee}` takes {expected} positional argument(s) but {got} were supplied"
            ),
            Self::UnknownMethod { method } => write!(f, "unknown method `.{method}`"),
            Self::DefAsValue { name } => {
                write!(f, "`{name}` is a function and cannot be used as a value")
            }
            Self::Unsupported { what } => write!(f, "{what} is not supported in the first pass"),
            Self::InvalidNumber(text) => write!(f, "invalid numeric literal `{text}`"),
            Self::UnknownType(name) => write!(f, "unknown type `{name}`"),
            Self::DomainOnNonValueType { ty } => {
                write!(f, "`{ty}` does not carry a domain annotation")
            }
            Self::DomainOnPortType { port } => write!(
                f,
                "port `{port}` does not take a domain annotation; ports take their clock as a type argument"
            ),
            Self::RecordConstructorNotStruct { name } => {
                write!(f, "`{name}` is not a struct constructor")
            }
            Self::UnknownStructField { struct_name, field } => {
                write!(f, "struct `{struct_name}` has no field `{field}`")
            }
            Self::MissingStructField { struct_name, field } => {
                write!(
                    f,
                    "missing field `{field}` in record constructor for `{struct_name}`"
                )
            }
            Self::DuplicateStructField { field } => {
                write!(f, "duplicate field `{field}` in record constructor")
            }
            Self::AssignmentLhsNotVar => {
                write!(f, "the left-hand side of `=` must be a `var` signal name")
            }
            Self::InternalUnresolved(what) => {
                write!(f, "internal: {what} was not resolved by name resolution")
            }
        }
    }
}

/// Lower a name-resolved Surface IR file into HIR.
pub fn lower_to_hir(
    file: &SourceFile,
    resolve: &ResolveResult,
) -> Result<HirSourceFile, Vec<HirLowerError>> {
    let mut ctx = Lowerer::new(file, resolve);
    let mut items = Vec::new();
    for item in &file.items {
        match item {
            Item::Fn(func) => {
                if let Some(hir_fn) = ctx.lower_fn(func) {
                    items.push(HirItem::Fn(hir_fn));
                }
            }
            Item::Struct(s) => {
                if let Some(hir_struct) = ctx.lower_struct(s) {
                    items.push(HirItem::Struct(hir_struct));
                }
            }
            Item::Port(p) => {
                if let Some(hir_port) = ctx.lower_port(p) {
                    items.push(HirItem::Port(hir_port));
                }
            }
            Item::Impl(impl_block) => {
                ctx.lower_impl(impl_block, &mut items);
            }
        }
    }
    if ctx.errors.is_empty() {
        Ok(HirSourceFile {
            items,
            span: file.span.clone(),
        })
    } else {
        Err(ctx.errors)
    }
}

struct Lowerer<'a> {
    resolve: &'a ResolveResult,
    /// User-defined top-level functions AND impl methods, keyed by `DefId`.
    /// Both kinds share the same call-site slotting logic; their `FunctionDefinition`
    /// shape is identical.
    user_fns: HashMap<DefId, &'a FunctionDefinition>,
    /// User-defined structs, keyed by `DefId`. Used to slot record-literal
    /// constructors against declared field order.
    user_structs: HashMap<DefId, &'a StructDefinition>,
    /// Per-file counter for `HirId`s.
    next_hir_id: u32,
    /// Per-function state. Reset by [`Lowerer::lower_fn`].
    fn_state: FnState,
    /// `DefId` of the type whose `impl T { … }` block we are lowering, if
    /// any. Set when entering an impl item; cleared when leaving. Used by
    /// `lower_type` to substitute `Self` in method signatures and bodies.
    current_impl_target: Option<DefId>,
    errors: Vec<HirLowerError>,
}

#[derive(Default)]
struct FnState {
    next_local_id: u32,
    locals: Vec<HirLocalInfo>,
    /// Map from a surface-binding identifier's `NodeId` to its HIR `LocalId`.
    node_to_local: HashMap<NodeId, LocalId>,
}

impl<'a> Lowerer<'a> {
    fn new(file: &'a SourceFile, resolve: &'a ResolveResult) -> Self {
        let mut user_fns = HashMap::new();
        let mut user_structs = HashMap::new();
        for item in &file.items {
            match item {
                Item::Fn(func) => {
                    if let Some(&Res::Def(_, def_id)) = resolve.resolutions.get(&func.name.id) {
                        user_fns.insert(def_id, func);
                    }
                }
                Item::Struct(s) => {
                    if let Some(&Res::Def(_, def_id)) = resolve.resolutions.get(&s.name.id) {
                        user_structs.insert(def_id, s);
                    }
                }
                Item::Impl(impl_block) => {
                    // Methods defined inside the impl get their own `DefId`s
                    // from `resolve_impl`; record each one so call-site
                    // lookups (e.g. typeck's `lookup_fn`) find them.
                    for func in &impl_block.functions {
                        if let Some(&Res::Def(_, def_id)) = resolve.resolutions.get(&func.name.id) {
                            user_fns.insert(def_id, func);
                        }
                    }
                }
                _ => {}
            }
        }
        Self {
            resolve,
            user_fns,
            user_structs,
            next_hir_id: 0,
            fn_state: FnState::default(),
            current_impl_target: None,
            errors: Vec::new(),
        }
    }

    fn next_hir_id(&mut self) -> HirId {
        let id = HirId(self.next_hir_id);
        self.next_hir_id += 1;
        id
    }

    fn alloc_local(&mut self, name_node: NodeId, name: &str, span: &SourceSpan) -> LocalId {
        let id = LocalId(self.fn_state.next_local_id);
        self.fn_state.next_local_id += 1;
        let kind = self
            .resolve
            .locals
            .get(&name_node)
            .map(|info| info.kind)
            .unwrap_or(LocalKind::Let); // defensive — resolver should always have it
        self.fn_state.locals.push(HirLocalInfo {
            kind,
            name: name.to_owned(),
            span: span.clone(),
            surface_node: name_node,
        });
        self.fn_state.node_to_local.insert(name_node, id);
        id
    }

    fn local_for(&self, decl_node: NodeId) -> Option<LocalId> {
        self.fn_state.node_to_local.get(&decl_node).copied()
    }

    fn error(&mut self, kind: HirLowerErrorKind, span: SourceSpan) {
        self.errors.push(HirLowerError { kind, span });
    }

    // ----- items -----

    fn lower_fn(&mut self, func: &FunctionDefinition) -> Option<HirFn> {
        let def_id = match self.resolve.resolutions.get(&func.name.id) {
            Some(&Res::Def(_, id)) => id,
            _ => {
                self.error(
                    HirLowerErrorKind::InternalUnresolved(format!(
                        "fn `{}` def id",
                        func.name.text
                    )),
                    func.name.span.clone(),
                );
                return None;
            }
        };

        self.fn_state = FnState::default();

        let mut params = Vec::new();
        for np in &func.named_parameters {
            params.push(self.lower_named_param(np));
        }
        for pp in &func.parameters {
            params.push(self.lower_positional_param(pp));
        }

        let return_type = func.return_type.as_ref().map(|ty| self.lower_type(ty));
        let body = self.lower_block(&func.body);
        let locals = std::mem::take(&mut self.fn_state.locals);

        Some(HirFn {
            def_id,
            name: func.name.text.clone(),
            params,
            return_type,
            locals,
            body,
            span: func.span.clone(),
        })
    }

    fn lower_named_param(&mut self, np: &NamedParameter) -> HirParam {
        let local = self.alloc_local(np.name.id, &np.name.text, &np.name.span);
        let ty = np
            .ty
            .as_ref()
            .map(|t| self.lower_type(t))
            .unwrap_or_else(|| {
                // Named params always have a declared type in the current
                // grammar; this fallback keeps lowering total if that ever
                // slips.
                HirType {
                    kind: HirTypeKind::Value(ValueType {
                        kind: ValueKind::Usize,
                        domain: Domain::Unspecified,
                    }),
                    span: np.span.clone(),
                }
            });
        let default = np.default.as_ref().map(|e| self.lower_expr(e));
        HirParam {
            local,
            section: ParamSection::Named,
            kind: lower_param_kind(np.kind),
            direction: np.direction,
            ty,
            default,
            span: np.span.clone(),
        }
    }

    fn lower_positional_param(&mut self, pp: &Parameter) -> HirParam {
        let local = self.alloc_local(pp.name.id, &pp.name.text, &pp.name.span);
        let ty = self.lower_type(&pp.ty);
        let default = pp.default.as_ref().map(|e| self.lower_expr(e));
        HirParam {
            local,
            section: ParamSection::Positional,
            kind: lower_param_kind(pp.kind),
            direction: pp.direction,
            ty,
            default,
            span: pp.span.clone(),
        }
    }

    fn lower_struct(&mut self, s: &StructDefinition) -> Option<HirStruct> {
        let def_id = match self.resolve.resolutions.get(&s.name.id) {
            Some(&Res::Def(_, id)) => id,
            _ => return None,
        };
        // Reset per-fn state — struct fields can reference parameters declared
        // on the struct itself (parametric structs are out of basic-first-pass
        // scope, but the locals table needs to exist for `lower_type` to use
        // the same machinery).
        self.fn_state = FnState::default();
        let fields = s
            .fields
            .iter()
            .map(|f| HirStructField {
                name: f.name.text.clone(),
                ty: self.lower_type(&f.ty),
                span: f.span.clone(),
            })
            .collect();
        Some(HirStruct {
            def_id,
            name: s.name.text.clone(),
            fields,
            span: s.span.clone(),
        })
    }

    fn lower_port(&mut self, p: &PortDefinition) -> Option<HirPort> {
        let def_id = match self.resolve.resolutions.get(&p.name.id) {
            Some(&Res::Def(_, id)) => id,
            _ => return None,
        };
        self.fn_state = FnState::default();
        // Parameters must be lowered before field types so `@clk` annotations
        // on fields can resolve to the port's named parameter. The shape
        // mirrors `HirFn::params` — named section first, then positional.
        let mut params = Vec::new();
        for np in &p.named_parameters {
            params.push(self.lower_named_param(np));
        }
        for pp in &p.parameters {
            params.push(self.lower_positional_param(pp));
        }
        let fields = p
            .fields
            .iter()
            .map(|f| HirPortField {
                direction: f.direction,
                name: f.name.text.clone(),
                ty: self.lower_type(&f.ty),
                span: f.span.clone(),
            })
            .collect();
        Some(HirPort {
            def_id,
            name: p.name.text.clone(),
            params,
            fields,
            span: p.span.clone(),
        })
    }

    /// Lower each method inside an `impl T { ... }` block as a `HirItem::Fn`.
    /// The method's `HirFn.def_id` is the method's own `DefId` (allocated by
    /// `resolve_impl`), not the target type's. `current_impl_target` is set
    /// during method lowering so `lower_type` can substitute `Self`.
    ///
    /// Impl-level parameters (e.g. `impl T { #clk: Clock } { … }`) are not
    /// yet supported; if present, lowering errors and the impl is skipped.
    fn lower_impl(&mut self, impl_block: &crate::surface_ir::ImplBlock, items: &mut Vec<HirItem>) {
        let target_def = match self.resolve.resolutions.get(&impl_block.name.id) {
            Some(&Res::Def(_, id)) => id,
            _ => return,
        };
        if !impl_block.named_parameters.is_empty() || !impl_block.parameters.is_empty() {
            self.error(
                HirLowerErrorKind::Unsupported {
                    what: "impl-level parameters",
                },
                impl_block.span.clone(),
            );
            return;
        }
        self.current_impl_target = Some(target_def);
        for func in &impl_block.functions {
            if let Some(hir_fn) = self.lower_fn(func) {
                items.push(HirItem::Fn(hir_fn));
            }
        }
        self.current_impl_target = None;
    }

    // ----- blocks and statements -----

    /// Lower a fn body block. Same as `lower_block_no_tail`, but a tail
    /// expression becomes an implicit `HirStmt::Return` so it propagates as
    /// the function's return value. Block-expressions used in expression
    /// position go through `lower_block_no_tail` instead — their tail is
    /// the expression's value, not a return.
    fn lower_block(&mut self, block: &Block) -> HirBlock {
        let mut hir_block = self.lower_block_no_tail(block);
        if let Some(tail) = &block.tail {
            let value = self.lower_expr(tail);
            hir_block.statements.push(HirStmt::Return(value));
        }
        hir_block
    }

    /// Lower only the statement list of a `Block` (no implicit-return on
    /// the tail). Used as the shared lowering body; callers decide what to
    /// do with the tail.
    fn lower_block_no_tail(&mut self, block: &Block) -> HirBlock {
        // Prescan: allocate LocalIds for `var` declarations (block-wide
        // scope) AND for implicit vars introduced by source-arrow out-arg
        // targets (`f { name => x }(…)` where `x` is new). The resolver
        // tagged these as `LocalKind::ImplicitVar`; we mirror its alloc
        // here so subsequent references resolve.
        for stmt in &block.statements {
            self.prescan_stmt_for_locals(stmt);
        }

        let mut statements = Vec::new();
        for stmt in &block.statements {
            self.lower_stmt_into(stmt, &mut statements);
        }
        HirBlock {
            statements,
            span: block.span.clone(),
        }
    }

    /// Walk a statement and allocate `LocalId`s for any binding that needs
    /// to be in scope before the statement is lowered: `var` declarations
    /// and implicit vars introduced by source-arrow out-arg targets.
    fn prescan_stmt_for_locals(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Var(v) => {
                for name in &v.names {
                    self.alloc_local(name.id, &name.text, &name.span);
                }
            }
            Statement::Let(l) => self.prescan_expr_for_implicits(&l.value),
            Statement::Return(r) => self.prescan_expr_for_implicits(&r.value),
            Statement::Assignment(a) => {
                self.prescan_expr_for_implicits(&a.left);
                self.prescan_expr_for_implicits(&a.right);
            }
            Statement::Expression(e) => self.prescan_expr_for_implicits(&e.value),
        }
    }

    /// Walk an expression, allocating `LocalId`s for any implicit-var target
    /// the resolver introduced. Each call shape (postfix free-call, postfix
    /// method-call) can host out-arg bindings in its named or positional
    /// section; we visit all of them.
    fn prescan_expr_for_implicits(&mut self, expr: &Expression) {
        match expr {
            Expression::Postfix(p) => {
                self.prescan_expr_for_implicits(&p.receiver);
                for op in &p.operations {
                    match op {
                        PostfixOperation::NamedArguments(list) => {
                            for arg in &list.arguments {
                                if let NamedArgument::Source(s) = arg {
                                    self.maybe_alloc_implicit_var(&s.target);
                                }
                            }
                        }
                        PostfixOperation::Arguments(list) => {
                            for arg in &list.arguments {
                                match arg {
                                    PositionalArgument::Value(e) => {
                                        self.prescan_expr_for_implicits(e);
                                    }
                                    PositionalArgument::OutBind(out) => {
                                        self.maybe_alloc_implicit_var(&out.target);
                                    }
                                }
                            }
                        }
                        PostfixOperation::Field(_) => {}
                    }
                }
            }
            Expression::Binary(b) => {
                self.prescan_expr_for_implicits(&b.left);
                self.prescan_expr_for_implicits(&b.right);
            }
            Expression::RecordConstructor(r) => {
                for field in &r.fields {
                    self.prescan_expr_for_implicits(&field.value);
                }
            }
            Expression::Block(b) => {
                for stmt in &b.statements {
                    self.prescan_stmt_for_locals(stmt);
                }
                if let Some(tail) = &b.tail {
                    self.prescan_expr_for_implicits(tail);
                }
            }
            Expression::If(if_expr) => {
                self.prescan_expr_for_implicits(&if_expr.condition);
                for branch in [&if_expr.then_branch, &if_expr.else_branch] {
                    for stmt in &branch.statements {
                        self.prescan_stmt_for_locals(stmt);
                    }
                    if let Some(tail) = &branch.tail {
                        self.prescan_expr_for_implicits(tail);
                    }
                }
            }
            Expression::Identifier(_) | Expression::Number(_) | Expression::Path(_) => {}
        }
    }

    /// Allocate a `LocalId` for an implicit-var target if (a) the resolver
    /// classified the target's surface node as `LocalKind::ImplicitVar` (the
    /// introduction site), and (b) we haven't already allocated for it.
    fn maybe_alloc_implicit_var(&mut self, target: &crate::surface_ir::Identifier) {
        if self.fn_state.node_to_local.contains_key(&target.id) {
            return;
        }
        let info = self.resolve.locals.get(&target.id);
        if matches!(info.map(|i| i.kind), Some(LocalKind::ImplicitVar)) {
            self.alloc_local(target.id, &target.text, &target.span);
        }
    }

    fn lower_stmt_into(&mut self, stmt: &Statement, out: &mut Vec<HirStmt>) {
        match stmt {
            Statement::Let(l) => out.push(self.lower_let(l)),
            Statement::Var(v) => self.lower_var(v, out),
            Statement::Assignment(a) => {
                if let Some(stmt) = self.lower_assignment(a) {
                    out.push(stmt);
                }
            }
            Statement::Return(r) => out.push(self.lower_return(r)),
            Statement::Expression(e) => out.push(HirStmt::Expr(self.lower_expr(&e.value))),
        }
    }

    fn lower_let(&mut self, l: &LetStatement) -> HirStmt {
        // RHS is lowered before the binding is introduced, so `let x = x + 1`
        // sees the outer `x`. (Resolver already arranged this in `resolutions`.)
        let value = self.lower_expr(&l.value);
        let local = self.alloc_local(l.name.id, &l.name.text, &l.name.span);
        HirStmt::Let(HirLet {
            local,
            value,
            span: l.span.clone(),
        })
    }

    fn lower_var(&mut self, v: &VarStatement, out: &mut Vec<HirStmt>) {
        let ty = v.ty.as_ref().map(|t| self.lower_type(t));
        // Locals already allocated by the prescan; just look them up.
        for name in &v.names {
            let Some(local) = self.local_for(name.id) else {
                self.error(
                    HirLowerErrorKind::InternalUnresolved(format!("var `{}`", name.text)),
                    name.span.clone(),
                );
                continue;
            };
            out.push(HirStmt::VarDecl(HirVarDecl {
                local,
                ty: ty.clone(),
                span: v.span.clone(),
            }));
        }

        // `var x: T = init;` splits into a `VarDecl` plus an `Equation` whose
        // `lhs` is the (sole) var name.
        if let Some(init) = &v.init {
            if v.names.len() != 1 {
                self.error(
                    HirLowerErrorKind::Unsupported {
                        what: "multi-name `var` with initializer",
                    },
                    v.span.clone(),
                );
            }
            let rhs = self.lower_expr(init);
            if let Some(name) = v.names.first()
                && let Some(local) = self.local_for(name.id)
            {
                out.push(HirStmt::Equation(HirEquation {
                    lhs: local,
                    rhs,
                    span: v.span.clone(),
                }));
            }
        }
    }

    fn lower_assignment(&mut self, a: &AssignmentStatement) -> Option<HirStmt> {
        let lhs = match &a.left {
            Expression::Identifier(ident) => match self.resolve.resolutions.get(&ident.id) {
                Some(Res::Local(decl_node)) => self.local_for(*decl_node),
                _ => {
                    self.error(HirLowerErrorKind::AssignmentLhsNotVar, ident.span.clone());
                    None
                }
            },
            other => {
                self.error(HirLowerErrorKind::AssignmentLhsNotVar, other.span().clone());
                None
            }
        }?;
        let rhs = self.lower_expr(&a.right);
        Some(HirStmt::Equation(HirEquation {
            lhs,
            rhs,
            span: a.span.clone(),
        }))
    }

    fn lower_return(&mut self, r: &ReturnStatement) -> HirStmt {
        HirStmt::Return(self.lower_expr(&r.value))
    }

    // ----- expressions -----

    fn lower_expr(&mut self, expr: &Expression) -> HirExpr {
        match expr {
            Expression::Identifier(ident) => {
                let kind = match self.resolve.resolutions.get(&ident.id) {
                    Some(Res::Local(decl_node)) => match self.local_for(*decl_node) {
                        Some(local) => HirExprKind::Local(local),
                        None => {
                            self.error(
                                HirLowerErrorKind::InternalUnresolved(format!(
                                    "local `{}`",
                                    ident.text
                                )),
                                ident.span.clone(),
                            );
                            HirExprKind::Const(ConstValue::Integer(0))
                        }
                    },
                    Some(Res::Def(_, _)) => {
                        self.error(
                            HirLowerErrorKind::DefAsValue {
                                name: ident.text.clone(),
                            },
                            ident.span.clone(),
                        );
                        HirExprKind::Const(ConstValue::Integer(0))
                    }
                    None => {
                        // Built-in reset-polarity literals. Resolved here rather
                        // than in name resolution because they have no `DefId`
                        // representation yet.
                        if let Some(value) = builtin_literal(&ident.text) {
                            HirExprKind::Const(value)
                        } else {
                            self.error(
                                HirLowerErrorKind::InternalUnresolved(format!(
                                    "identifier `{}`",
                                    ident.text
                                )),
                                ident.span.clone(),
                            );
                            HirExprKind::Const(ConstValue::Integer(0))
                        }
                    }
                };
                HirExpr {
                    kind,
                    ty: None,
                    span: ident.span.clone(),
                    id: self.next_hir_id(),
                }
            }
            Expression::Number(n) => {
                let value = match n.text.parse::<u64>() {
                    Ok(v) => v,
                    Err(_) => {
                        self.error(
                            HirLowerErrorKind::InvalidNumber(n.text.clone()),
                            n.span.clone(),
                        );
                        0
                    }
                };
                HirExpr {
                    kind: HirExprKind::Const(ConstValue::Integer(value)),
                    ty: None,
                    span: n.span.clone(),
                    id: self.next_hir_id(),
                }
            }
            Expression::Path(p) => {
                self.error(
                    HirLowerErrorKind::Unsupported {
                        what: "path expressions",
                    },
                    p.span.clone(),
                );
                HirExpr {
                    kind: HirExprKind::Const(ConstValue::Integer(0)),
                    ty: None,
                    span: p.span.clone(),
                    id: self.next_hir_id(),
                }
            }
            Expression::Binary(b) => self.lower_binary(b),
            Expression::Postfix(p) => self.lower_postfix(p),
            Expression::RecordConstructor(r) => self.lower_record_constructor(r),
            Expression::Block(b) => self.lower_block_expression(b),
            Expression::If(if_expr) => self.lower_if_expression(if_expr),
        }
    }

    /// Lower a surface `{ stmts; tail }` block-expression to a HIR
    /// `Block` expression. The block stays tree-shaped through typeck; a
    /// later pass flattens it into a result-local plus inlined statements.
    fn lower_block_expression(&mut self, block: &Block) -> HirExpr {
        let span = block.span.clone();
        let inner = self.lower_block_no_tail(block);
        let tail = block.tail.as_ref().map(|t| self.lower_expr(t));
        HirExpr {
            kind: HirExprKind::Block(Box::new(super::HirBlockExpr { block: inner, tail })),
            ty: None,
            span,
            id: self.next_hir_id(),
        }
    }

    /// Lower `if cond { … } else { … }` to a HIR `If` expression. Each
    /// branch is lowered as its own block-expression so the tail value is
    /// available to the late flattening pass.
    fn lower_if_expression(&mut self, if_expr: &crate::surface_ir::IfExpression) -> HirExpr {
        let condition = self.lower_expr(&if_expr.condition);
        let then_branch = self.lower_branch_block(&if_expr.then_branch);
        let else_branch = self.lower_branch_block(&if_expr.else_branch);
        HirExpr {
            kind: HirExprKind::If(Box::new(super::HirIfExpr {
                condition,
                then_branch,
                else_branch,
            })),
            ty: None,
            span: if_expr.span.clone(),
            id: self.next_hir_id(),
        }
    }

    fn lower_branch_block(&mut self, block: &Block) -> super::HirBlockExpr {
        let inner = self.lower_block_no_tail(block);
        let tail = block.tail.as_ref().map(|t| self.lower_expr(t));
        super::HirBlockExpr { block: inner, tail }
    }

    /// Desugar `a + b` / `a * b` into a `HirCall` against the prelude
    /// operator's `DefId`. After lowering, HIR has no dedicated binary-op
    /// shape — every "operation" is a call. Type checking handles the
    /// polymorphic signature `(+){N, D}(uint(N) @D, uint(N) @D) -> uint(N) @D`
    /// the same way it handles `reg`'s implicit width parameter today.
    fn lower_binary(&mut self, b: &crate::surface_ir::BinaryExpression) -> HirExpr {
        let op_name = match b.operator {
            BinaryOperator::Add => "+",
            BinaryOperator::Multiply => "*",
        };
        let left = self.lower_expr(&b.left);
        let right = self.lower_expr(&b.right);
        let callee = match self.resolve.def_id(op_name) {
            Some(id) => id,
            None => {
                self.error(
                    HirLowerErrorKind::InternalUnresolved(format!("prelude operator `{op_name}`")),
                    b.span.clone(),
                );
                return self.placeholder_expr(b.span.clone());
            }
        };
        HirExpr {
            kind: HirExprKind::Call(HirCall {
                callee,
                args: vec![hir_given(left), hir_given(right)],
                span: b.span.clone(),
            }),
            ty: None,
            span: b.span.clone(),
            id: self.next_hir_id(),
        }
    }

    /// Desugar a record literal into a `HirCall` against the struct's `DefId`.
    /// The struct's declared fields act as a positional parameter list; the
    /// user's named fields are slotted into declared order. Missing, unknown,
    /// and duplicate fields are caught here so type-checking sees a
    /// well-formed call (with one `Given` arg per declared field) and only
    /// needs to verify value types.
    fn lower_record_constructor(&mut self, r: &RecordConstructorExpression) -> HirExpr {
        let struct_def_id = match self.resolve.resolutions.get(&r.constructor.id) {
            Some(&Res::Def(DefKind::Struct, def_id)) => def_id,
            _ => {
                self.error(
                    HirLowerErrorKind::RecordConstructorNotStruct {
                        name: r.constructor.text.clone(),
                    },
                    r.constructor.span.clone(),
                );
                // Still lower the field expressions so any inner errors surface.
                for f in &r.fields {
                    let _ = self.lower_expr(&f.value);
                }
                return self.placeholder_expr(r.span.clone());
            }
        };

        let Some(decl) = self.user_structs.get(&struct_def_id).copied() else {
            self.error(
                HirLowerErrorKind::RecordConstructorNotStruct {
                    name: r.constructor.text.clone(),
                },
                r.constructor.span.clone(),
            );
            return self.placeholder_expr(r.span.clone());
        };

        // Lower each provided field's value, indexed by name. Reject
        // duplicates and unknown names along the way.
        let mut provided: HashMap<String, HirExpr> = HashMap::new();
        for field in &r.fields {
            let value = self.lower_expr(&field.value);
            if provided.contains_key(&field.name.text) {
                self.error(
                    HirLowerErrorKind::DuplicateStructField {
                        field: field.name.text.clone(),
                    },
                    field.span.clone(),
                );
                continue;
            }
            if !decl.fields.iter().any(|d| d.name.text == field.name.text) {
                self.error(
                    HirLowerErrorKind::UnknownStructField {
                        struct_name: decl.name.text.clone(),
                        field: field.name.text.clone(),
                    },
                    field.span.clone(),
                );
                continue;
            }
            provided.insert(field.name.text.clone(), value);
        }

        // Slot in declared field order. Missing fields are reported and slot
        // gets a placeholder so the call's arg count still matches.
        let mut args = Vec::with_capacity(decl.fields.len());
        for decl_field in &decl.fields {
            match provided.remove(&decl_field.name.text) {
                Some(expr) => args.push(hir_given(expr)),
                None => {
                    self.error(
                        HirLowerErrorKind::MissingStructField {
                            struct_name: decl.name.text.clone(),
                            field: decl_field.name.text.clone(),
                        },
                        r.span.clone(),
                    );
                    args.push(hir_given(self.placeholder_expr(r.span.clone())));
                }
            }
        }

        HirExpr {
            kind: HirExprKind::Call(HirCall {
                callee: struct_def_id,
                args,
                span: r.span.clone(),
            }),
            ty: None,
            span: r.span.clone(),
            id: self.next_hir_id(),
        }
    }

    fn lower_postfix(&mut self, p: &PostfixExpression) -> HirExpr {
        // Two shapes we recognise:
        //   1. Free call: receiver is an identifier resolving to a fn def,
        //      followed by an optional `{ ... }` and a `( ... )`.
        //   2. Method call: any receiver, followed by `.method(...)`. Only
        //      `.reg(...)` is supported in the first pass.

        let direct_def = match p.receiver.as_ref() {
            Expression::Identifier(ident) => match self.resolve.resolutions.get(&ident.id) {
                Some(&Res::Def(DefKind::Fn, def_id)) => Some(def_id),
                _ => None,
            },
            _ => None,
        };

        if let Some(def_id) = direct_def {
            return self.lower_free_call(p, def_id);
        }

        self.lower_method_call(p)
    }

    fn lower_free_call(&mut self, p: &PostfixExpression, def_id: DefId) -> HirExpr {
        let mut named_block: Option<&[NamedArgument]> = None;
        let mut positional_block: Option<&[PositionalArgument]> = None;
        for op in &p.operations {
            match op {
                PostfixOperation::Field(field) => {
                    self.error(
                        HirLowerErrorKind::Unsupported {
                            what: "field access on a function definition",
                        },
                        field.span.clone(),
                    );
                }
                PostfixOperation::NamedArguments(list) => {
                    if named_block.is_some() {
                        self.error(
                            HirLowerErrorKind::Unsupported {
                                what: "multiple named-argument blocks per call",
                            },
                            list.span.clone(),
                        );
                    }
                    named_block = Some(&list.arguments);
                }
                PostfixOperation::Arguments(list) => {
                    if positional_block.is_some() {
                        self.error(
                            HirLowerErrorKind::Unsupported {
                                what: "chained function calls",
                            },
                            list.span.clone(),
                        );
                    }
                    positional_block = Some(&list.arguments);
                }
            }
        }

        // Drop callee fields out so we can mutate self while iterating signature.
        let Some(callee) = self.user_fns.get(&def_id).copied() else {
            self.error(
                HirLowerErrorKind::Unsupported {
                    what: "calls to non-user-defined functions (no prelude free-call shape known)",
                },
                p.span.clone(),
            );
            return self.placeholder_expr(p.span.clone());
        };

        // Index the user's named args by name. Both sink (`name = value`)
        // and source (`name => target`) forms supply an expression; the
        // source form materialises an identifier from the target local.
        let mut user_named: HashMap<&str, Expression> = HashMap::new();
        if let Some(args) = named_block {
            for arg in args {
                match arg {
                    NamedArgument::Sink(s) => {
                        user_named.insert(s.name.text.as_str(), s.value.clone());
                    }
                    NamedArgument::Source(s) => {
                        user_named.insert(
                            s.name.text.as_str(),
                            Expression::Identifier(s.target.clone()),
                        );
                    }
                }
            }
        }

        // Materialise positional arguments to a uniform `Expression` shape.
        // Out-bind forms (`out => target`) become an identifier expression
        // referencing the target local; the callee's positional slot's
        // direction (Out) is what actually gives them their out-arg semantics.
        let positional_exprs: Vec<Expression> = match positional_block {
            Some(args) => args
                .iter()
                .map(|arg| match arg {
                    PositionalArgument::Value(e) => e.clone(),
                    PositionalArgument::OutBind(out) => Expression::Identifier(out.target.clone()),
                })
                .collect(),
            None => Vec::new(),
        };
        let positional_param_count = callee.parameters.len();
        if positional_exprs.len() > positional_param_count {
            self.error(
                HirLowerErrorKind::TooManyPositionalArgs {
                    callee: callee.name.text.clone(),
                    expected: positional_param_count,
                    got: positional_exprs.len(),
                },
                p.span.clone(),
            );
        }

        let mut args = Vec::with_capacity(callee.named_parameters.len() + positional_param_count);

        // Named-section slots. A named `param`/`dom` with no default is
        // inferable from call-site usage.
        for np in &callee.named_parameters {
            let supplied = user_named.get(np.name.text.as_str());
            let inferable = matches!(np.kind, ParamKind::Param | ParamKind::Dom);
            args.push(self.slot_arg(
                supplied,
                np.default.as_ref(),
                inferable,
                &np.name.text,
                &callee.name.text,
                &p.span,
            ));
        }
        // Positional-section slots. Positional `param`/`dom` are never
        // inferable — they must be supplied at the call site.
        for (i, pp) in callee.parameters.iter().enumerate() {
            let supplied = positional_exprs.get(i);
            args.push(self.slot_arg(
                supplied,
                pp.default.as_ref(),
                false,
                &pp.name.text,
                &callee.name.text,
                &p.span,
            ));
        }

        HirExpr {
            kind: HirExprKind::Call(HirCall {
                callee: def_id,
                args,
                span: p.span.clone(),
            }),
            ty: None,
            span: p.span.clone(),
            id: self.next_hir_id(),
        }
    }

    fn slot_arg(
        &mut self,
        supplied: Option<&Expression>,
        default: Option<&Expression>,
        inferable: bool,
        param_name: &str,
        callee_name: &str,
        call_span: &SourceSpan,
    ) -> HirArg {
        if let Some(expr) = supplied {
            hir_given(self.lower_expr(expr))
        } else if let Some(expr) = default {
            hir_default(self.lower_expr(expr))
        } else if inferable {
            HirArg::Inferable
        } else {
            self.error(
                HirLowerErrorKind::MissingRequiredArgument {
                    callee: callee_name.to_owned(),
                    param: param_name.to_owned(),
                },
                call_span.clone(),
            );
            HirArg::Inferable
        }
    }

    /// Walk a postfix expression's operations left-to-right, building up the
    /// HIR expression as we go. Two shapes are recognised within the walk:
    ///
    /// - `Field` immediately followed by `Arguments` → method call. Only the
    ///   prelude `.reg(rst, reset_val)` is dispatched today; everything else
    ///   surfaces as an `UnknownMethod` error pending the deferred
    ///   method-dispatch pass (typeck-time, modelled on rustc).
    /// - Bare `Field` → field access (`HirExprKind::Field`); the receiver's
    ///   type is resolved by type-check, which dispatches to the struct or
    ///   port definition.
    ///
    /// `NamedArguments` and standalone `Arguments` ops only make sense
    /// against a direct fn-def receiver, handled by `lower_free_call`. Any
    /// other shape (e.g. `f(x).y`) is rejected as unsupported.
    fn lower_method_call(&mut self, p: &PostfixExpression) -> HirExpr {
        let mut current = self.lower_expr(&p.receiver);
        let ops = &p.operations;
        let mut i = 0;
        while i < ops.len() {
            match &ops[i] {
                PostfixOperation::Field(field) => {
                    let method_call =
                        matches!(ops.get(i + 1), Some(PostfixOperation::Arguments(_)));
                    if method_call {
                        let PostfixOperation::Arguments(args_list) = &ops[i + 1] else {
                            unreachable!()
                        };
                        current = self.lower_method_step(current, field, &args_list.arguments);
                        i += 2;
                    } else {
                        let span = combine_spans(&current.span, &field.span);
                        current = HirExpr {
                            kind: HirExprKind::Field(HirFieldAccess {
                                receiver: Box::new(current),
                                name: field.field.text.clone(),
                                name_span: field.field.span.clone(),
                            }),
                            ty: None,
                            span,
                            id: self.next_hir_id(),
                        };
                        i += 1;
                    }
                }
                PostfixOperation::Arguments(list) => {
                    self.error(
                        HirLowerErrorKind::Unsupported {
                            what: "calling a non-function expression",
                        },
                        list.span.clone(),
                    );
                    return self.placeholder_expr(p.span.clone());
                }
                PostfixOperation::NamedArguments(list) => {
                    self.error(
                        HirLowerErrorKind::Unsupported {
                            what: "named arguments outside a direct function call",
                        },
                        list.span.clone(),
                    );
                    return self.placeholder_expr(p.span.clone());
                }
            }
        }
        current
    }

    /// Lower a single `.<name>(<args>)` step against an already-lowered
    /// receiver.
    ///
    /// - `.reg(rst, reset_val)` is special-cased and lowered directly to a
    ///   `HirCall` against the prelude `reg` def. The prelude method is
    ///   not associated with a specific receiver type.
    /// Every `.method(...)` becomes a `HirExprKind::MethodCall`. `typeck`
    /// resolves the callee via `ResolveResult::impl_methods` keyed on the
    /// receiver's type — including the prelude `uint::reg` entry, so the
    /// receiver's type drives dispatch rather than the method's name.
    fn lower_method_step(
        &mut self,
        receiver: HirExpr,
        field: &crate::surface_ir::FieldAccess,
        args: &[PositionalArgument],
    ) -> HirExpr {
        let call_span = combine_spans(&receiver.span, &field.span);
        let exprs = self.materialise_positional_args(args);

        let lowered_args: Vec<HirArg> = exprs
            .iter()
            .map(|e| hir_given(self.lower_expr(e)))
            .collect();
        HirExpr {
            kind: HirExprKind::MethodCall(super::HirMethodCall {
                receiver: Box::new(receiver),
                name: field.field.text.clone(),
                name_span: field.field.span.clone(),
                args: lowered_args,
            }),
            ty: None,
            span: call_span,
            id: self.next_hir_id(),
        }
    }

    /// Convert each `PositionalArgument` to an `Expression`: values pass
    /// through verbatim; out-binds materialise to an identifier expression
    /// referencing the target local. Used by every postfix-call lowering
    /// path so the rest of the code sees a uniform shape.
    fn materialise_positional_args(&mut self, args: &[PositionalArgument]) -> Vec<Expression> {
        args.iter()
            .map(|a| match a {
                PositionalArgument::Value(e) => e.clone(),
                PositionalArgument::OutBind(out) => Expression::Identifier(out.target.clone()),
            })
            .collect()
    }

    fn placeholder_expr(&mut self, span: SourceSpan) -> HirExpr {
        HirExpr {
            kind: HirExprKind::Const(ConstValue::Integer(0)),
            ty: None,
            span,
            id: self.next_hir_id(),
        }
    }

    // ----- types -----

    fn lower_type(&mut self, ty: &TypeExpression) -> HirType {
        // A type-head can be one of: a primitive (`uint`/`bool`/`Reset`/`Clock`/
        // `usize`), `Self` (synthesised by the parameter lowerer for `self @clk`
        // shorthand), or a user-defined struct/port name. The latter is resolved
        // by looking up the head in the def table.
        let domain_annotation = ty.domain.as_ref().and_then(|d| self.lower_domain(d));

        match ty.name.text.as_str() {
            "uint" => {
                let width = match ty.suffixes.first() {
                    Some(TypeSuffix::Index(idx)) => self.lower_expr(&idx.index),
                    None => {
                        self.error(
                            HirLowerErrorKind::Unsupported {
                                what: "`uint` without an explicit width",
                            },
                            ty.span.clone(),
                        );
                        self.placeholder_expr(ty.span.clone())
                    }
                };
                self.value_type(
                    ValueKind::UInt {
                        width: Box::new(width),
                    },
                    domain_annotation,
                    ty.span.clone(),
                )
            }
            "bool" => self.value_type(ValueKind::Bool, domain_annotation, ty.span.clone()),
            "Reset" => self.value_type(ValueKind::Reset, domain_annotation, ty.span.clone()),
            "Clock" => {
                if domain_annotation.is_some() {
                    self.error(
                        HirLowerErrorKind::DomainOnNonValueType { ty: "Clock" },
                        ty.span.clone(),
                    );
                }
                HirType {
                    kind: HirTypeKind::Clock,
                    span: ty.span.clone(),
                }
            }
            "usize" => self.value_type(ValueKind::Usize, domain_annotation, ty.span.clone()),
            "Self" => {
                // `self @clk` shorthand: the parameter lowerer in surface_ir
                // synthesises this type. Outside an `impl` it's nonsensical.
                // Inside an impl, substitute the impl's target type.
                let Some(target) = self.current_impl_target else {
                    self.error(
                        HirLowerErrorKind::Unsupported {
                            what: "`Self` type outside an `impl` block",
                        },
                        ty.span.clone(),
                    );
                    return self.value_type(ValueKind::Usize, None, ty.span.clone());
                };
                let target_kind = self
                    .resolve
                    .defs
                    .get(target.0 as usize)
                    .map(|info| info.kind);
                match target_kind {
                    Some(DefKind::Struct) => self.value_type(
                        ValueKind::Struct { def: target },
                        domain_annotation,
                        ty.span.clone(),
                    ),
                    Some(DefKind::Port) => {
                        if domain_annotation.is_some() {
                            self.error(
                                HirLowerErrorKind::DomainOnPortType {
                                    port: "Self".to_owned(),
                                },
                                ty.span.clone(),
                            );
                        }
                        HirType {
                            kind: HirTypeKind::Port(PortTypeRef { def: target }),
                            span: ty.span.clone(),
                        }
                    }
                    _ => {
                        self.error(
                            HirLowerErrorKind::UnknownType("Self".to_owned()),
                            ty.span.clone(),
                        );
                        self.value_type(ValueKind::Usize, None, ty.span.clone())
                    }
                }
            }
            other => {
                // User-defined struct or port.
                match self.resolve.def_id(other).and_then(|id| {
                    self.resolve
                        .defs
                        .get(id.0 as usize)
                        .map(|info| (info.kind, id))
                }) {
                    Some((DefKind::Struct, def_id)) => self.value_type(
                        ValueKind::Struct { def: def_id },
                        domain_annotation,
                        ty.span.clone(),
                    ),
                    Some((DefKind::Port, def_id)) => {
                        // Ports don't carry a top-level domain. Clock bindings
                        // are supplied as type arguments (`Stream8(clk)` —
                        // syntax pending). An `@clk` annotation here is a
                        // category error and is rejected.
                        if domain_annotation.is_some() {
                            self.error(
                                HirLowerErrorKind::DomainOnPortType {
                                    port: other.to_owned(),
                                },
                                ty.span.clone(),
                            );
                        }
                        HirType {
                            kind: HirTypeKind::Port(PortTypeRef { def: def_id }),
                            span: ty.span.clone(),
                        }
                    }
                    Some((DefKind::Fn, _))
                    | Some((DefKind::Impl, _))
                    | Some((DefKind::Method { .. }, _))
                    | Some((DefKind::BuiltinType, _)) => {
                        // `uint`/`bool` reach this arm when written verbatim
                        // (e.g. `let x: uint`) — primitive types must be
                        // written with their required suffix (`uint(N)`) and
                        // are handled by the literal `"uint"` arm above.
                        self.error(
                            HirLowerErrorKind::UnknownType(other.to_owned()),
                            ty.name.span.clone(),
                        );
                        self.value_type(ValueKind::Usize, None, ty.span.clone())
                    }
                    None => {
                        self.error(
                            HirLowerErrorKind::UnknownType(other.to_owned()),
                            ty.name.span.clone(),
                        );
                        self.value_type(ValueKind::Usize, None, ty.span.clone())
                    }
                }
            }
        }
    }

    fn value_type(
        &mut self,
        kind: ValueKind,
        domain_annotation: Option<Domain>,
        span: SourceSpan,
    ) -> HirType {
        HirType {
            kind: HirTypeKind::Value(ValueType {
                kind,
                domain: domain_annotation.unwrap_or(Domain::Unspecified),
            }),
            span,
        }
    }

    fn lower_domain(&mut self, ident: &crate::Identifier) -> Option<Domain> {
        match self.resolve.resolutions.get(&ident.id) {
            Some(Res::Local(decl_node)) => {
                let local = self.local_for(*decl_node)?;
                Some(Domain::Clock(local))
            }
            Some(Res::Def(_, _)) => None,
            None => None,
        }
    }
}

/// Surface and HIR `ParamKind` carry the same three variants; just remap.
fn lower_param_kind(kind: ParamKind) -> HirParamKind {
    match kind {
        ParamKind::Value => HirParamKind::Value,
        ParamKind::Param => HirParamKind::Param,
        ParamKind::Dom => HirParamKind::Dom,
    }
}

fn hir_given(expr: HirExpr) -> HirArg {
    HirArg::Provided {
        expr,
        source: HirArgSource::Given,
    }
}

fn hir_default(expr: HirExpr) -> HirArg {
    HirArg::Provided {
        expr,
        source: HirArgSource::Default,
    }
}

/// Build a span covering `from`'s start through `to`'s end. Used when stitching
/// a postfix operation onto a receiver expression to produce a span for the
/// combined HIR node.
fn combine_spans(from: &SourceSpan, to: &SourceSpan) -> SourceSpan {
    SourceSpan {
        start_byte: from.start_byte,
        end_byte: to.end_byte,
        start: from.start.clone(),
        end: to.end.clone(),
    }
}

fn builtin_literal(text: &str) -> Option<ConstValue> {
    // `true`/`false` are bool literals; `high`/`low` are reset-polarity
    // literals (the default for `rstn: Reset @clk = high`). First-pass
    // `Reset` carries no separate enum, so we represent the active level as
    // a `bool` const and let later passes interpret it in `Reset` context.
    match text {
        "true" | "high" => Some(ConstValue::Bool(true)),
        "false" | "low" => Some(ConstValue::Bool(false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve_file;
    use crate::surface_ir::{Direction, parse_surface_source};

    fn lower(source: &str) -> Result<HirSourceFile, Vec<HirLowerError>> {
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(
            resolve.errors.is_empty(),
            "unexpected resolve errors: {:?}",
            resolve.errors
        );
        lower_to_hir(&file, &resolve)
    }

    fn lower_ok(source: &str) -> HirSourceFile {
        match lower(source) {
            Ok(file) => file,
            Err(errors) => panic!("unexpected lowering errors: {errors:?}"),
        }
    }

    fn first_fn(file: &HirSourceFile) -> &HirFn {
        file.items
            .iter()
            .find_map(|item| match item {
                HirItem::Fn(f) => Some(f),
                _ => None,
            })
            .expect("at least one fn item")
    }

    fn nth_fn(file: &HirSourceFile, n: usize) -> &HirFn {
        file.items
            .iter()
            .filter_map(|item| match item {
                HirItem::Fn(f) => Some(f),
                _ => None,
            })
            .nth(n)
            .expect("not enough fn items")
    }

    #[test]
    fn lowers_simple_function() {
        let file = lower_ok("fn f(a: uint(8) @clk) -> uint(8) @clk { let r = a; return r; }");
        let func = first_fn(&file);
        assert_eq!(func.name, "f");
        assert_eq!(func.params.len(), 1);
        // Two locals: param `a` and `let r`.
        assert_eq!(func.locals.len(), 2);
        assert!(matches!(func.body.statements[0], HirStmt::Let(_)));
        assert!(matches!(func.body.statements[1], HirStmt::Return(_)));
    }

    #[test]
    fn tail_expression_lowers_to_implicit_return() {
        // A fn body's trailing expression (no `;`) becomes an implicit
        // `return tail;` in HIR. The lowered shape is identical to writing
        // the explicit return.
        let file = lower_ok("fn f(a: uint(8) @clk) -> uint(8) @clk { let r = a; r }");
        let func = first_fn(&file);
        assert!(matches!(func.body.statements[0], HirStmt::Let(_)));
        let HirStmt::Return(ret) = &func.body.statements[1] else {
            panic!(
                "expected implicit Return, got {:?}",
                func.body.statements[1]
            );
        };
        // The return expression is the tail (`r`), a Local reference.
        assert!(matches!(ret.kind, HirExprKind::Local(_)));
    }

    #[test]
    fn splits_var_with_initializer() {
        let file = lower_ok(
            "fn f(rstn: Reset @clk, data: uint(8) @clk) { var acc: uint(8) @clk = (acc + data).reg(rstn, 0); }",
        );
        let func = first_fn(&file);
        // After `data: ... @clk` param, body should be VarDecl + Equation.
        let stmts: Vec<&HirStmt> = func.body.statements.iter().collect();
        assert_eq!(stmts.len(), 2, "got {stmts:?}");
        assert!(matches!(stmts[0], HirStmt::VarDecl(_)));
        let eq = match stmts[1] {
            HirStmt::Equation(e) => e,
            other => panic!("expected Equation, got {other:?}"),
        };
        // The equation's LHS is the var `acc`, which is the same local as the
        // VarDecl's local.
        let decl = match stmts[0] {
            HirStmt::VarDecl(d) => d,
            _ => unreachable!(),
        };
        assert_eq!(eq.lhs, decl.local);
    }

    #[test]
    fn assignment_lowers_to_equation() {
        let file = lower_ok(
            "fn f(rstn: Reset @clk) { var count: uint(8) @clk; count = (count + 1).reg(rstn, 0); }",
        );
        let func = first_fn(&file);
        // VarDecl, then Equation from the assignment.
        let stmts = &func.body.statements;
        assert!(matches!(stmts[0], HirStmt::VarDecl(_)));
        assert!(matches!(stmts[1], HirStmt::Equation(_)));
    }

    #[test]
    fn method_call_to_reg_lowers_to_method_call() {
        // HIR lowering emits `MethodCall` for every `.method(...)`, including
        // `.reg(...)`. The method-lower pass (driven by typeck's resolution
        // table) rewrites it into a `HirCall` against the prelude `reg`.
        let file = lower_ok(
            "fn f(rstn: Reset @clk, data: uint(8) @clk) -> uint(8) @clk { let r = data.reg(rstn, 0); return r; }",
        );
        let func = first_fn(&file);
        let HirStmt::Let(l) = &func.body.statements[0] else {
            panic!("expected Let");
        };
        let HirExprKind::MethodCall(mc) = &l.value.kind else {
            panic!("expected MethodCall, got {:?}", l.value.kind);
        };
        assert_eq!(mc.name, "reg");
        // Two user-supplied args: rst and reset_val. The receiver is in `mc.receiver`,
        // and the `#clk` inferable slot is prepended by method_lower (not here).
        assert_eq!(mc.args.len(), 2);
    }

    #[test]
    fn free_call_slots_defaults_and_inferables() {
        // `target` declares all three named params (#clk inferable, rstn with default, c with default)
        // and one positional `a`. The caller supplies only `c` and the positional `x`.
        let file = lower_ok(
            "fn target { dom clk: Clock, rstn: Reset @clk = high, c: uint(8) @clk = 0 } ( a: uint(8) @clk ) { let r = a; }\n\
             fn caller ( x: uint(8) ) { let r = target { c = 5 }(x); }",
        );
        let caller = nth_fn(&file, 1);
        let HirStmt::Let(l) = &caller.body.statements[0] else {
            panic!("expected Let");
        };
        let HirExprKind::Call(call) = &l.value.kind else {
            panic!("expected Call");
        };
        // [#clk: Inferable, rstn: Default(high), c: Given(5), a: Given(x)]
        assert_eq!(call.args.len(), 4);
        assert!(matches!(call.args[0], HirArg::Inferable));
        assert!(matches!(
            call.args[1],
            HirArg::Provided {
                source: HirArgSource::Default,
                ..
            }
        ));
        assert!(matches!(
            call.args[2],
            HirArg::Provided {
                source: HirArgSource::Given,
                ..
            }
        ));
        assert!(matches!(
            call.args[3],
            HirArg::Provided {
                source: HirArgSource::Given,
                ..
            }
        ));
    }

    #[test]
    fn missing_required_arg_is_reported() {
        // `target` has a required positional `a` (no default). The caller omits it.
        let errors = lower(
            "fn target ( a: uint(8) ) { let r = a; }\n\
             fn caller ( ) { let r = target(); }",
        )
        .expect_err("expected lowering errors");
        assert!(
            errors.iter().any(|e| matches!(
                &e.kind,
                HirLowerErrorKind::MissingRequiredArgument { callee, param }
                    if callee == "target" && param == "a"
            )),
            "got: {errors:?}"
        );
    }

    #[test]
    fn lowers_struct_definition() {
        let file = lower_ok("struct Packet = packet { valid: bool, payload: uint(8) }");
        let hir_struct = file
            .items
            .iter()
            .find_map(|i| match i {
                HirItem::Struct(s) => Some(s),
                _ => None,
            })
            .expect("struct item");
        assert_eq!(hir_struct.name, "Packet");
        assert_eq!(hir_struct.fields.len(), 2);
        assert_eq!(hir_struct.fields[0].name, "valid");
        assert!(matches!(
            hir_struct.fields[0].ty.kind,
            HirTypeKind::Value(ValueType {
                kind: ValueKind::Bool,
                ..
            })
        ));
        assert_eq!(hir_struct.fields[1].name, "payload");
        assert!(matches!(
            hir_struct.fields[1].ty.kind,
            HirTypeKind::Value(ValueType {
                kind: ValueKind::UInt { .. },
                ..
            })
        ));
    }

    #[test]
    fn lowers_port_definition_with_field_directions() {
        let file = lower_ok(
            "port Stream8 { dom clk: Clock } = stream8 {\n\
                 out valid: bool @clk,\n\
                 out data: uint(8) @clk,\n\
                 in ready: bool @clk,\n\
             }",
        );
        let port = file
            .items
            .iter()
            .find_map(|i| match i {
                HirItem::Port(p) => Some(p),
                _ => None,
            })
            .expect("port item");
        assert_eq!(port.name, "Stream8");
        assert_eq!(port.fields.len(), 3);
        assert_eq!(port.fields[0].name, "valid");
        assert!(matches!(port.fields[0].direction, Direction::Out));
        assert_eq!(port.fields[2].name, "ready");
        assert!(matches!(port.fields[2].direction, Direction::In));
    }

    #[test]
    fn record_constructor_lowers_to_call_in_declared_order() {
        let file = lower_ok(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn idle() -> Packet { return packet { payload: 0, valid: false }; }",
        );
        let func = first_fn(&file);
        let HirStmt::Return(ret) = &func.body.statements[0] else {
            panic!("expected return");
        };
        let HirExprKind::Call(call) = &ret.kind else {
            panic!("expected Call, got {:?}", ret.kind);
        };
        // Slotted in declared field order: valid (bool), then payload (uint).
        assert_eq!(call.args.len(), 2);
        let HirArg::Provided { expr: valid, .. } = &call.args[0] else {
            panic!("expected Provided");
        };
        assert!(matches!(
            valid.kind,
            HirExprKind::Const(ConstValue::Bool(false))
        ));
        let HirArg::Provided { expr: payload, .. } = &call.args[1] else {
            panic!("expected Provided");
        };
        assert!(matches!(
            payload.kind,
            HirExprKind::Const(ConstValue::Integer(0))
        ));
    }

    #[test]
    fn missing_struct_field_is_reported() {
        let errors = lower(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn f() -> Packet { return packet { valid: false }; }",
        )
        .expect_err("expected lowering errors");
        assert!(
            errors.iter().any(|e| matches!(
                &e.kind,
                HirLowerErrorKind::MissingStructField { struct_name, field }
                    if struct_name == "Packet" && field == "payload"
            )),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn unknown_struct_field_is_reported() {
        let errors = lower(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn f() -> Packet { return packet { valid: false, payload: 0, extra: 1 }; }",
        )
        .expect_err("expected lowering errors");
        assert!(
            errors.iter().any(|e| matches!(
                &e.kind,
                HirLowerErrorKind::UnknownStructField { struct_name, field }
                    if struct_name == "Packet" && field == "extra"
            )),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn duplicate_struct_field_is_reported() {
        let errors = lower(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn f() -> Packet { return packet { valid: false, valid: true, payload: 0 }; }",
        )
        .expect_err("expected lowering errors");
        assert!(
            errors.iter().any(|e| matches!(
                &e.kind,
                HirLowerErrorKind::DuplicateStructField { field } if field == "valid"
            )),
            "errors: {errors:?}"
        );
    }

    // `out_direction_is_preserved_on_param` exercised `Stream8 @clk` —
    // removed pending `Stream8(clk)` syntax. Restore once port type
    // arguments are wired up.

    #[test]
    fn lowers_working_examples() {
        for (name, source) in crate::test_support::working_examples() {
            let file = parse_surface_source(&source)
                .unwrap_or_else(|e| panic!("example `{name}` failed to parse: {e}"));
            let resolve = resolve_file(&file);
            assert!(
                resolve.errors.is_empty(),
                "example `{name}` failed to resolve: {:?}",
                resolve.errors
            );
            match lower_to_hir(&file, &resolve) {
                Ok(_) => {}
                Err(errors) => panic!("example `{name}` failed to lower: {errors:?}"),
            }
        }
    }

    #[test]
    fn bare_field_access_lowers_to_field_expr() {
        let file = lower_ok(
            "struct Pair = pair { a: bool, b: uint(8) }\n\
             fn f(p: Pair) -> bool { return p.a; }",
        );
        let func = first_fn(&file);
        let HirStmt::Return(ret) = &func.body.statements[0] else {
            panic!("expected return statement");
        };
        let HirExprKind::Field(field) = &ret.kind else {
            panic!("expected Field expression, got {:?}", ret.kind);
        };
        assert_eq!(field.name, "a");
        assert!(matches!(field.receiver.kind, HirExprKind::Local(_)));
    }

    #[test]
    fn chained_field_then_reg_lowers_to_method_call_on_field_access() {
        let file = lower_ok(
            "struct Pair = pair { a: bool, b: uint(8) }\n\
             fn f(rstn: Reset @clk, p: Pair @clk) -> uint(8) @clk { return p.b.reg(rstn, 0); }",
        );
        let func = first_fn(&file);
        let HirStmt::Return(ret) = &func.body.statements[0] else {
            panic!("expected return statement");
        };
        // `p.b.reg(rstn, 0)` lowers to a MethodCall whose receiver is
        // `Field(Local(p), "b")`. method_lower (driven by typeck) rewrites
        // the MethodCall into a HirCall against the prelude `reg`.
        let HirExprKind::MethodCall(mc) = &ret.kind else {
            panic!("expected MethodCall, got {:?}", ret.kind);
        };
        assert_eq!(mc.name, "reg");
        let HirExprKind::Field(field) = &mc.receiver.kind else {
            panic!("expected Field receiver, got {:?}", mc.receiver.kind);
        };
        assert_eq!(field.name, "b");
        assert_eq!(mc.args.len(), 2);
    }

    #[test]
    fn non_reg_method_lowers_to_method_call() {
        // Any `.method(args)` that isn't the prelude `.reg` becomes a
        // `HirExprKind::MethodCall`; resolution happens at typeck via the
        // resolver's `impl_methods` table.
        let file = lower_ok("fn f(a: uint(8)) -> uint(8) { return a.frobnicate(); }");
        let func = first_fn(&file);
        let HirStmt::Return(ret) = &func.body.statements[0] else {
            panic!("expected return");
        };
        let HirExprKind::MethodCall(mc) = &ret.kind else {
            panic!("expected MethodCall, got {:?}", ret.kind);
        };
        assert_eq!(mc.name, "frobnicate");
        assert!(matches!(mc.receiver.kind, HirExprKind::Local(_)));
        assert!(mc.args.is_empty());
    }
}
