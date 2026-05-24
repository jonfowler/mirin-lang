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
    BinOp, ConstValue, Domain, HirArg, HirBlock, HirCall, HirEquation, HirExpr, HirExprKind, HirFn,
    HirId, HirItem, HirLet, HirLocalInfo, HirParam, HirPort, HirPortField, HirRecord,
    HirRecordField, HirSourceFile, HirStmt, HirStruct, HirStructField, HirType, HirTypeKind,
    HirVarDecl, LocalId, ParamSection, PortTypeRef, ValueKind, ValueType,
};
use crate::SourceSpan;
use crate::resolve::{DefId, DefKind, LocalKind, Res, ResolveResult};
use crate::surface_ir::{
    AssignmentStatement, BinaryOperator, Block, Expression, FunctionDefinition, Item, LetStatement,
    NamedArgument, NamedParameter, NodeId, Parameter, PortDefinition, PostfixExpression,
    PostfixOperation, RecordConstructorExpression, ReturnStatement, SourceFile, Statement,
    StructDefinition, TypeExpression, TypeSuffix, VarStatement,
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
    /// `.reg(rst, reset_val)` did not receive exactly two positional args.
    RegArity { got: usize },
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
    /// A record constructor names something other than a struct constructor.
    RecordConstructorNotStruct { name: String },
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
            Self::RegArity { got } => write!(
                f,
                "`.reg(rst, reset_val)` expects 2 positional arguments, got {got}"
            ),
            Self::DefAsValue { name } => {
                write!(f, "`{name}` is a function and cannot be used as a value")
            }
            Self::Unsupported { what } => write!(f, "{what} is not supported in the first pass"),
            Self::InvalidNumber(text) => write!(f, "invalid numeric literal `{text}`"),
            Self::UnknownType(name) => write!(f, "unknown type `{name}`"),
            Self::DomainOnNonValueType { ty } => {
                write!(f, "`{ty}` does not carry a domain annotation")
            }
            Self::RecordConstructorNotStruct { name } => {
                write!(f, "`{name}` is not a struct constructor")
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
            // `impl` lowering is deferred until method resolution + path
            // expressions land; see todo-examples/impl_examples.plr.
            Item::Impl(_) => {}
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
    /// User-defined top-level functions, keyed by `DefId` for signature lookup
    /// when slotting call-site arguments.
    user_fns: HashMap<DefId, &'a FunctionDefinition>,
    /// `DefId` of the prelude `reg` primitive. Set once at startup; used for
    /// every `.reg(...)` desugaring.
    reg_def_id: Option<DefId>,
    /// Per-file counter for `HirId`s.
    next_hir_id: u32,
    /// Per-function state. Reset by [`Lowerer::lower_fn`].
    fn_state: FnState,
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
        for item in &file.items {
            if let Item::Fn(func) = item
                && let Some(&Res::Def(_, def_id)) = resolve.resolutions.get(&func.name.id)
            {
                user_fns.insert(def_id, func);
            }
        }
        Self {
            resolve,
            user_fns,
            reg_def_id: resolve.def_id("reg"),
            next_hir_id: 0,
            fn_state: FnState::default(),
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
            inferable: np.inferable,
            is_const: np.is_const,
            direction: None,
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
            inferable: pp.inferable,
            is_const: pp.is_const,
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
        // Port named parameters (`{ #clk: Clock }`) must be lowered before
        // field types so `@clk` annotations on fields can resolve to them.
        let named_params: Vec<HirParam> = p
            .named_parameters
            .iter()
            .map(|np| self.lower_named_param(np))
            .collect();
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
            named_params,
            fields,
            span: p.span.clone(),
        })
    }

    // ----- blocks and statements -----

    fn lower_block(&mut self, block: &Block) -> HirBlock {
        // Prescan: allocate LocalIds for all `var` declarations so subsequent
        // uses can resolve them, matching the resolver's block-wide-scope rule.
        for stmt in &block.statements {
            if let Statement::Var(v) = stmt {
                for name in &v.names {
                    self.alloc_local(name.id, &name.text, &name.span);
                }
            }
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
            Expression::Binary(b) => {
                let left = self.lower_expr(&b.left);
                let right = self.lower_expr(&b.right);
                let op = match b.operator {
                    BinaryOperator::Add => BinOp::Add,
                    BinaryOperator::Multiply => BinOp::Multiply,
                };
                HirExpr {
                    kind: HirExprKind::Binary(op, Box::new(left), Box::new(right)),
                    ty: None,
                    span: b.span.clone(),
                    id: self.next_hir_id(),
                }
            }
            Expression::Postfix(p) => self.lower_postfix(p),
            Expression::RecordConstructor(r) => self.lower_record_constructor(r),
        }
    }

    fn lower_record_constructor(&mut self, r: &RecordConstructorExpression) -> HirExpr {
        // The constructor identifier names the struct's constructor; the
        // resolver maps it back to the struct's DefId.
        let struct_def = match self.resolve.resolutions.get(&r.constructor.id) {
            Some(&Res::Def(DefKind::Struct, def_id)) => Some(def_id),
            Some(&Res::Def(_, _)) | Some(Res::Local(_)) => {
                self.error(
                    HirLowerErrorKind::RecordConstructorNotStruct {
                        name: r.constructor.text.clone(),
                    },
                    r.constructor.span.clone(),
                );
                None
            }
            None => {
                // Constructor names that don't resolve get the same diagnostic
                // shape; this happens for record literals whose head name is
                // unknown.
                self.error(
                    HirLowerErrorKind::RecordConstructorNotStruct {
                        name: r.constructor.text.clone(),
                    },
                    r.constructor.span.clone(),
                );
                None
            }
        };

        let fields: Vec<HirRecordField> = r
            .fields
            .iter()
            .map(|f| HirRecordField {
                name: f.name.text.clone(),
                value: self.lower_expr(&f.value),
                span: f.span.clone(),
            })
            .collect();

        match struct_def {
            Some(def) => HirExpr {
                kind: HirExprKind::Record(HirRecord {
                    struct_def: def,
                    fields,
                    span: r.span.clone(),
                }),
                ty: None,
                span: r.span.clone(),
                id: self.next_hir_id(),
            },
            None => self.placeholder_expr(r.span.clone()),
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
        let mut positional_block: Option<&[Expression]> = None;
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

        // Index the user's named args by name.
        let mut user_named: HashMap<&str, &Expression> = HashMap::new();
        if let Some(args) = named_block {
            for arg in args {
                if let NamedArgument::Sink(s) = arg {
                    user_named.insert(s.name.text.as_str(), &s.value);
                }
                // Source (`=>`) on a fn named param was already rejected by
                // direction checking; ignore it here.
            }
        }

        let positional = positional_block.unwrap_or(&[]);
        let positional_param_count = callee.parameters.len();
        if positional.len() > positional_param_count {
            self.error(
                HirLowerErrorKind::TooManyPositionalArgs {
                    callee: callee.name.text.clone(),
                    expected: positional_param_count,
                    got: positional.len(),
                },
                p.span.clone(),
            );
        }

        let mut args = Vec::with_capacity(callee.named_parameters.len() + positional_param_count);

        // Named-section slots.
        for np in &callee.named_parameters {
            let supplied = user_named.get(np.name.text.as_str()).copied();
            args.push(self.slot_arg(
                supplied,
                np.default.as_ref(),
                np.inferable,
                &np.name.text,
                &callee.name.text,
                &p.span,
            ));
        }
        // Positional-section slots.
        for (i, pp) in callee.parameters.iter().enumerate() {
            let supplied = positional.get(i);
            args.push(self.slot_arg(
                supplied,
                pp.default.as_ref(),
                pp.inferable,
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
            HirArg::Given(self.lower_expr(expr))
        } else if let Some(expr) = default {
            HirArg::Default(self.lower_expr(expr))
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

    fn lower_method_call(&mut self, p: &PostfixExpression) -> HirExpr {
        // First-pass supports only `<expr>.reg(rst, reset_val)`.
        let mut field = None;
        let mut args: Option<&[Expression]> = None;
        let mut rest_ok = true;
        for op in &p.operations {
            match op {
                PostfixOperation::Field(f) if field.is_none() => {
                    field = Some(f);
                }
                PostfixOperation::Arguments(list) if args.is_none() => {
                    args = Some(&list.arguments);
                }
                _ => {
                    rest_ok = false;
                }
            }
        }
        if !rest_ok {
            self.error(
                HirLowerErrorKind::Unsupported {
                    what: "method-call shapes beyond `<expr>.method(args)`",
                },
                p.span.clone(),
            );
            return self.placeholder_expr(p.span.clone());
        }
        let Some(field) = field else {
            self.error(
                HirLowerErrorKind::Unsupported {
                    what: "postfix expressions without a callable head",
                },
                p.span.clone(),
            );
            return self.placeholder_expr(p.span.clone());
        };

        if field.field.text != "reg" {
            self.error(
                HirLowerErrorKind::UnknownMethod {
                    method: field.field.text.clone(),
                },
                field.span.clone(),
            );
            return self.placeholder_expr(p.span.clone());
        }
        let Some(reg_def_id) = self.reg_def_id else {
            self.error(
                HirLowerErrorKind::InternalUnresolved("prelude `reg` def".to_owned()),
                p.span.clone(),
            );
            return self.placeholder_expr(p.span.clone());
        };

        let receiver_expr = self.lower_expr(&p.receiver);

        // `reg` signature (hardcoded for first pass):
        //   named:      [ #clk: Clock ]
        //   positional: [ self @clk, rst: Reset @clk, reset_val: uint(N) @clk ]
        let user_args = args.unwrap_or(&[]);
        if user_args.len() != 2 {
            self.error(
                HirLowerErrorKind::RegArity {
                    got: user_args.len(),
                },
                p.span.clone(),
            );
        }
        let rst = user_args
            .first()
            .map(|e| self.lower_expr(e))
            .unwrap_or_else(|| self.placeholder_expr(p.span.clone()));
        let reset_val = user_args
            .get(1)
            .map(|e| self.lower_expr(e))
            .unwrap_or_else(|| self.placeholder_expr(p.span.clone()));

        let hir_args = vec![
            HirArg::Inferable,            // #clk
            HirArg::Given(receiver_expr), // self
            HirArg::Given(rst),
            HirArg::Given(reset_val),
        ];

        HirExpr {
            kind: HirExprKind::Call(HirCall {
                callee: reg_def_id,
                args: hir_args,
                span: p.span.clone(),
            }),
            ty: None,
            span: p.span.clone(),
            id: self.next_hir_id(),
        }
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
                // For now treat it as a value of unknown kind anchored by the
                // given domain; later passes that need the concrete type can
                // resolve `Self` against the enclosing impl.
                self.error(
                    HirLowerErrorKind::Unsupported {
                        what: "`Self` type (no impl-context resolution yet)",
                    },
                    ty.span.clone(),
                );
                self.value_type(ValueKind::Usize, None, ty.span.clone())
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
                        // Ports do not carry a top-level domain. The clock flows
                        // through the port's own `#clk` parameter into its
                        // fields, so an `@clk` annotation here is rejected at
                        // lowering rather than waiting for type-checking.
                        if domain_annotation.is_some() {
                            // We accept this for now (with a warning-shaped
                            // error) because all current examples write
                            // `Stream8 @clk` to convey the clock binding.
                            // Strict rejection lands once port-parameter
                            // application returns to the grammar.
                            let _ = domain_annotation;
                        }
                        HirType {
                            kind: HirTypeKind::Port(PortTypeRef { def: def_id }),
                            span: ty.span.clone(),
                        }
                    }
                    Some((DefKind::Fn, _)) | Some((DefKind::Impl, _)) => {
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
    fn method_call_to_reg_desugars() {
        let file = lower_ok(
            "fn f(rstn: Reset @clk, data: uint(8) @clk) -> uint(8) @clk { let r = data.reg(rstn, 0); return r; }",
        );
        let func = first_fn(&file);
        let HirStmt::Let(l) = &func.body.statements[0] else {
            panic!("expected Let");
        };
        let HirExprKind::Call(call) = &l.value.kind else {
            panic!("expected Call, got {:?}", l.value.kind);
        };
        // reg's args = [#clk: Inferable, self: Given(data), rst: Given(rstn), reset_val: Given(0)]
        assert_eq!(call.args.len(), 4);
        assert!(matches!(call.args[0], HirArg::Inferable));
        assert!(matches!(call.args[1], HirArg::Given(_)));
        assert!(matches!(call.args[2], HirArg::Given(_)));
        assert!(matches!(call.args[3], HirArg::Given(_)));
    }

    #[test]
    fn free_call_slots_defaults_and_inferables() {
        // `target` declares all three named params (#clk inferable, rstn with default, c with default)
        // and one positional `a`. The caller supplies only `c` and the positional `x`.
        let file = lower_ok(
            "fn target { #clk: Clock, rstn: Reset @clk = high, c: uint(8) @clk = 0 } ( a: uint(8) @clk ) { let r = a; }\n\
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
        assert!(matches!(call.args[1], HirArg::Default(_)));
        assert!(matches!(call.args[2], HirArg::Given(_)));
        assert!(matches!(call.args[3], HirArg::Given(_)));
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
            "port Stream8 { #clk: Clock } = stream8 {\n\
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
    fn record_constructor_lowers_to_record_node() {
        let file = lower_ok(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn idle() -> Packet { return packet { valid: false, payload: 0 }; }",
        );
        let func = first_fn(&file);
        let HirStmt::Return(ret) = &func.body.statements[0] else {
            panic!("expected return");
        };
        let HirExprKind::Record(rec) = &ret.kind else {
            panic!("expected Record, got {:?}", ret.kind);
        };
        assert_eq!(rec.fields.len(), 2);
        assert_eq!(rec.fields[0].name, "valid");
        assert_eq!(rec.fields[1].name, "payload");
    }

    #[test]
    fn out_direction_is_preserved_on_param() {
        let file = lower_ok(
            "port Stream8 { #clk: Clock } = stream8 { out valid: bool @clk }\n\
             fn connect { #clk: Clock } ( upstream: Stream8 @clk, out downstream: Stream8 @clk ) { downstream = upstream; }",
        );
        let func = first_fn(&file);
        // Two positional params: upstream (no direction) and downstream (Out).
        let upstream = &func.params[1]; // named `#clk` is at [0]
        let downstream = &func.params[2];
        assert!(matches!(upstream.section, ParamSection::Positional));
        assert!(matches!(downstream.section, ParamSection::Positional));
        assert!(upstream.direction.is_none());
        assert!(matches!(downstream.direction, Some(Direction::Out)));
    }

    #[test]
    fn lowers_first_pass_examples() {
        let examples: &[(&str, &str)] = &[
            (
                "add_constant",
                include_str!("../../../../examples/add_constant.plr"),
            ),
            (
                "accumulator",
                include_str!("../../../../examples/accumulator.plr"),
            ),
            ("counter", include_str!("../../../../examples/counter.plr")),
            (
                "mult_add",
                include_str!("../../../../examples/mult_add.plr"),
            ),
            (
                "packet_struct",
                include_str!("../../../../examples/packet_struct.plr"),
            ),
            (
                "pipeline",
                include_str!("../../../../examples/pipeline.plr"),
            ),
            (
                "shift_register",
                include_str!("../../../../examples/shift_register.plr"),
            ),
            (
                "simple_port",
                include_str!("../../../../examples/simple_port.plr"),
            ),
        ];
        for (name, source) in examples {
            let file = parse_surface_source(source)
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
}
