//! Type checking for the ETA language.
//!
//! The [`Typechecker`] walks the resolved AST and verifies that all operations
//! are well-typed. It populates a [`SymbolTable`] with type information that is
//! consumed by WTAC generation.
//!
//! Typechecking uses two passes over top-level declarations: first, all function
//! signatures and record definitions are registered (to support mutual recursion
//! and forward references), then each declaration body is checked.
//!
//! Statements produce a [`StatementType`] (`Unit` or `Void`) rather than a value
//! type. This tracks whether control flow can continue past a statement, which is
//! needed to verify that all function branches return a value.

use logos::Span;
use tracing::{debug, instrument};

use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

use crate::{
    ast::*,
    symbols::{Attr, Entry, MemberEntry, RecordEntry, SymbolTable},
    types::{self, Type},
};

//     my_func(b: bool): int {
//   if (b) {
//     return 1
//   } else {
//     // This branch falls through
//   }
// }
// This should fail since function body has void type!
/// The result type of statement typechecking.
///
/// This is not a data type — it describes control flow behavior.
/// A function body must have type `Void` to guarantee that every path returns.
#[derive(Debug, Clone, PartialEq)]
pub enum StatementType {
    /// The statement may complete normally (control passes to the next statement).
    Unit,
    /// The statement never passes control to the next one (e.g. it returns).
    Void,
}

impl StatementType {
    /// Computes the least upper bound of two statement types.
    ///
    /// If either branch might complete normally (`Unit`), the combination is `Unit`.
    /// Only if both branches return (`Void`) is the result `Void`.
    pub fn lub(s1: &StatementType, s2: &StatementType) -> StatementType {
        match (s1, s2) {
            (StatementType::Unit, _) => StatementType::Unit,
            (_, StatementType::Unit) => StatementType::Unit,
            _ => s1.clone(), // They are equal
        }
    }
}

/// The typechecker for the ETA language.
///
/// Maintains a [`SymbolTable`] that is populated during checking and later
/// consumed by the [`WtacGen`](crate::wtac::wtac_gen::WtacGen).
/// The built-in `length` function (takes any array, returns `int`) is
/// pre-registered.
pub struct Typechecker {
    pub symtab: SymbolTable,
    pub diagnostics: Vec<Diagnostic>,
    curr_fun_span: Option<Span>,
}

impl Default for Typechecker {
    fn default() -> Self {
        Self::new()
    }
}

impl Typechecker {
    /// Creates a new typechecker with a fresh symbol table.
    ///
    /// Pre-registers the built-in `length` function.
    pub fn new() -> Self {
        let mut symtab = SymbolTable::new();
        let length_f_type = Type::FunType {
            param_types: vec![Type::Array {
                elem_type: Box::new(Type::Unknown),
            }],
            ret_type: vec![Type::Int],
        };
        symtab.add_fun("length".to_string(), length_f_type, true, None);
        Typechecker {
            symtab,
            diagnostics: Vec::new(),
            curr_fun_span: None,
        }
    }

    /// Entry point: typechecks an entire program.
    ///
    /// First registers all top-level names, then checks each declaration body.
    pub fn typecheck(&mut self, prog: &Program) -> Result<(), Vec<Diagnostic>> {
        for decl in &prog.decls {
            self.add_top_level(decl);
        }
        debug!(sym_tab = %self.symtab, "Top-Level SymbolTable");
        for decl in &prog.decls {
            self.typecheck_declarations(decl);
        }
        debug!(sym_tab = %self.symtab, "Final SymbolTable");

        if self.diagnostics.is_empty() {
            Ok(())
        } else {
            Err(self.diagnostics.clone())
        }
    }

    /// Registers a top-level declaration (function signature or record type)
    /// without checking its body.
    fn add_top_level(&mut self, decl: &Declaration) {
        match decl {
            Declaration::FunDecl(Function {
                name, f_type, span, ..
            }) => {
                if self.symtab.get_opt(name).is_none() {
                    self.symtab
                        .add_fun(name.clone(), f_type.clone(), false, Some(span.clone()));
                }
            }
            Declaration::RecordDecl(rec) => self.typecheck_record(rec),
            _ => (),
        }
    }

    /// Typechecks a single top-level declaration.
    fn typecheck_declarations(&mut self, decl: &Declaration) {
        match decl {
            Declaration::Global(VarDeclaration {
                name,
                init,
                var_type,
                dims,
                span,
            }) if dims.is_empty() => {
                if let Type::Record(_) = var_type {
                    self.diagnostics.push(Diagnostic::error_with_label(
                        span.clone(),
                        format!("global record variable `{name}` is not allowed"),
                        "records cannot be declared as global variables",
                    ));
                    self.symtab
                        .add_global_var(name.clone(), Type::Error, Some(span.clone()));
                    return;
                }
                self.symtab
                    .add_global_var(name.clone(), var_type.clone(), Some(span.clone()));
                if let Some(e) = init {
                    let t = self.typecheck_expression(e);
                    if t != Type::Error && !Self::eq_mod_unknown(&t, var_type) {
                        let mut diag = Diagnostic::error_with_label(
                            e.span.clone(),
                            format!("mismatched types in global `{name}` initialization"),
                            format!("expected `{var_type}`, found `{t}`"),
                        )
                        .with_secondary(
                            span.clone(),
                            format!("`{name}` declared with type `{var_type}` here"),
                        );
                        if let Some((s, m)) = self.expr_decl_info(e) {
                            diag = diag.with_secondary(s, m);
                        }
                        self.diagnostics.push(diag);
                    }
                }
            }
            // Array globals
            Declaration::Global(vd) => {
                self.symtab.add_global_var(
                    vd.name.clone(),
                    vd.var_type.clone(),
                    Some(vd.span.clone()),
                );

                if vd.init.is_some() {
                    self.diagnostics.push(Diagnostic::error_with_label(
                        vd.span.clone(),
                        format!("global array `{}` cannot have an initializer", vd.name),
                        "global array initializers are not allowed",
                    ));
                    return;
                }
                self.typecheck_arr_decl(vd);
            }
            Declaration::FunDecl(function) => self.typecheck_fun(function),
            Declaration::RecordDecl(rec) => self.check_record_cycles(rec),
            Declaration::Error => (),
        }
    }

    /// Nothing to check while a record cannot contain a record: the field
    /// typecheck rejects that outright, so no cycle can be built. Adding nested
    /// records means implementing this — walk members recursively, keeping the
    /// records already seen, and report a cycle on revisiting one.
    fn check_record_cycles(&self, _cur_rec: &Record) {}

    /// Typechecks a record declaration: verifies that no two fields share a name
    /// and computes byte offsets for each field. Registers the record in the
    /// symbol table.
    fn typecheck_record(&mut self, rec: &Record) {
        let mut seen: HashMap<String, Span> = HashMap::new();
        let mut members = Vec::new();
        let mut offset = 0;
        // Natural alignment: a field starts at the next multiple of its own
        // alignment, and the record's alignment is the widest of its fields.
        let mut align = 1;

        for member in &rec.members {
            let MemberDecl { name, t, span } = match member {
                Member::Var(decl) => decl,
                Member::Error => continue,
            };
            if let Some(prev_span) = seen.get(name) {
                self.diagnostics.push(
                    Diagnostic::error_with_label(
                        span.clone(),
                        format!("duplicate field `{name}` in record `{}`", rec.tag),
                        format!("field `{name}` is already declared"),
                    )
                    .with_secondary(
                        prev_span.clone(),
                        format!("first declaration of `{name}` here"),
                    ),
                );
                continue;
            }
            seen.insert(name.clone(), span.clone());

            match t {
                Type::FunType { .. } => {
                    self.diagnostics.push(Diagnostic::error_with_label(
                        span.clone(),
                        format!("function types are not allowed inside record `{}`", rec.tag),
                        "field cannot have a function type",
                    ));
                    continue;
                }
                Type::Record(_) => {
                    self.diagnostics.push(Diagnostic::error_with_label(
                        span.clone(),
                        format!(
                            "nested record types are not allowed inside record `{}`",
                            rec.tag
                        ),
                        format!("field `{name}` cannot have a record type `{t}`"),
                    ));
                    continue;
                }
                Type::Tuple { .. } | Type::Unknown | Type::Error => continue,
                _ => (),
            }

            let member_align = types::get_align(t);
            align = align.max(member_align);
            offset = crate::util::round_to_n(offset as isize, member_align as isize) as usize;

            let mem_entry = MemberEntry {
                member_t: t.clone(),
                offset,
                decl_span: Some(span.clone()),
            };
            offset += types::get_size(t);
            members.push((name.clone(), mem_entry));
        }

        let rec_entry = RecordEntry { members, align };

        // This add fun is not used since it is overwritten by the add_record.
        // Record Construcotr is handled in function call seperatly. They are nto real functions here
        // let param_types = rec.members.iter().map(|md| md.t.clone()).collect();
        // Add constructor
        // let fn_type = Type::FunType { param_types, ret_type: vec![Type::Record(rec.tag.clone())] };
        // self.symtab.add_fun(rec.tag.clone(), fn_type, true);

        self.symtab
            .add_record(rec.tag.clone(), rec_entry, Some(rec.span.clone()));
    }

    /// Typechecks a function: adds parameters to the symbol table, checks the body,
    /// and verifies that the body has [`StatementType::Void`] (i.e. all paths return).
    #[instrument(name="Function", skip(self, fun), fields(n = %fun.name))]
    fn typecheck_fun(&mut self, fun: &Function) {
        let (params_t, ret_t) = match &fun.f_type {
            Type::FunType {
                param_types,
                ret_type,
            } => (param_types, ret_type),
            _ => panic!("Internal Error: Function has non-function type"),
        };

        if let Some(old_entry) = self.symtab.get_opt(&fun.name) {
            let old_t = old_entry.t.clone();
            let old_span = old_entry.decl_span.clone();
            let old_attr = old_entry.attr.clone();

            if old_t != fun.f_type {
                let mut diag = Diagnostic::error_with_label(
                    fun.span.clone(),
                    format!("incompatible declaration for function `{}`", fun.name),
                    format!("found signature `{}`", fun.f_type),
                );
                if let Some(s) = &old_span {
                    diag = diag
                        .with_secondary(s.clone(), format!("previously declared as `{old_t}` here"))
                }
                self.diagnostics.push(diag);
            }
            match old_attr {
                Attr::Fun { defined: true, .. } => {
                    let mut diag = Diagnostic::error_with_label(
                        fun.span.clone(),
                        format!("function `{}` is already defined", fun.name),
                        "duplicate function definition",
                    );
                    if let Some(s) = old_span {
                        diag = diag.with_secondary(s, "previously defined here")
                    }
                    self.diagnostics.push(diag);
                }
                Attr::Fun { defined: false, .. } => (),
                _ => {
                    let mut diag = Diagnostic::error_with_label(
                        fun.span.clone(),
                        format!("`{}` was already declared as a non-function", fun.name),
                        "conflicting function definition",
                    );
                    if let Some(s) = old_span {
                        diag = diag.with_secondary(s, "previously declared here");
                    }
                    self.diagnostics.push(diag);
                }
            }
        }

        self.symtab.add_fun(
            fun.name.clone(),
            fun.f_type.clone(),
            true,
            Some(fun.span.clone()),
        );
        for ((p, p_span), ty) in fun
            .params
            .iter()
            .zip(fun.param_spans.iter())
            .zip(params_t.iter())
        {
            if let Some(p) = p {
                self.symtab
                    .add_local_var(p.clone(), ty.clone(), p_span.clone());
            }
        }

        let tuple_ret_t = Type::Tuple {
            elems: ret_t.clone(),
        };

        self.curr_fun_span = Some(fun.span.clone());
        let block_t = self.typecheck_block(&fun.body, &tuple_ret_t);
        self.curr_fun_span = None;

        if !ret_t.is_empty() && block_t != StatementType::Void {
            self.diagnostics.push(
                Diagnostic::error_with_label(
                    fun.span.clone(),
                    format!(
                        "not all control-flow paths in `{}` return a value",
                        fun.name
                    ),
                    format!("expected function to return `{tuple_ret_t}` on all paths"),
                )
                .with_suggestion("ensure every branch ends with a `return` statement"),
            );
        }
    }

    // In some cases we need to have the types without tuples. For example
    // x, y = add2()
    // add2() returns a tuple type. However, for assignment check we need the elements of the tuple
    // Second return Point(x,y)
    // Here Point returns a tuple which we need to comparew with the return_type. SO we need to wrap the type in tuple
    // which is wrong for function calls. So we first unfold the tuple type in those cases
    /// Flattens nested tuples into a single flat list of types.
    ///
    /// Multi-return functions produce `Tuple` types, and when multiple such
    /// calls appear on the RHS of an assignment, their types need to be
    /// flattened before comparing against the LHS.
    pub fn unfold_tuple_types(tys: Vec<Type>) -> Vec<Type> {
        let mut unfolded_tys = Vec::new();

        for ty in tys {
            match ty {
                // I do not think we need to do that recusrively here
                Type::Tuple { elems } => unfolded_tys.extend(elems),
                Type::FunType { .. } => panic!("Internal Error: Cant unfold Function Type"),
                _ => unfolded_tys.push(ty),
            }
        }
        unfolded_tys
    }

    /// Compares two types for equality, treating [`Type::Unknown`] and [`Type::Error`] as matching
    /// any type.
    ///
    /// This is needed for empty array literals (`{}`) whose element type
    /// is unknown until the context provides it.
    fn eq_mod_unknown(t1: &Type, t2: &Type) -> bool {
        match (t1, t2) {
            (Type::Unknown, _) => true,
            (_, Type::Unknown) => true,
            (Type::Error, _) | (_, Type::Error) => true,
            // pretty hacky I would say. Just used for length function. Does not cover all cases. Also used for Binary add on arrays
            (Type::Array { elem_type: et1 }, Type::Array { elem_type: et2 }) => {
                Self::eq_mod_unknown(et1, et2)
            }
            (t1_, t2_) => *t1_ == *t2_,
        }
    }

    /// Unifies two types, preferring the more specific one.
    ///
    /// When one side is [`Type::Error`], [`Type::Error`] is returned.
    /// When one side is [`Type::Unknown`] (e.g. from an empty array literal `{}`),
    /// the other side's type is returned. For arrays, unification recurses into
    /// the element type. Used after [`eq_mod_unknown`](Self::eq_mod_unknown)
    /// confirms the types are compatible.
    fn unify(t1: &Type, t2: &Type) -> Type {
        match (t1, t2) {
            (Type::Error, _) | (_, Type::Error) => Type::Error,
            (Type::Unknown, _) => t2.clone(),
            (_, Type::Unknown) => t1.clone(),
            // pretty hacky I would say. Just used for length function. Does not cover all cases. Also used for Binary add on arrays
            (Type::Array { elem_type: et1 }, Type::Array { elem_type: et2 }) => Type::Array {
                elem_type: Box::new(Self::unify(et1, et2)),
            },
            (t1_, t2_) if t1_ == t2_ => t1_.clone(),
            _ => panic!(
                "Internal Error: Unification only allowed for arrays and simple types right now!"
            ),
        }
    }

    fn typecheck_block(&mut self, block: &Block, ret_t: &Type) -> StatementType {
        if block.stmts.is_empty() {
            return StatementType::Unit;
        }
        let (last_stmt, preceding_stmts) = block.stmts.split_last().unwrap();
        let mut always_returns = false;
        // All except last have to have type unit
        for s in preceding_stmts {
            let stmt_t = self.typecheck_statement(s, ret_t);
            if stmt_t != StatementType::Unit {
                always_returns = true;
                self.diagnostics.push(Diagnostic::error_with_label(
                    s.span.clone(),
                    "unreachable code after returning statement",
                    "this statement always returns, so following statements are unreachable",
                ));
            }
        }
        // Result of Block is result of last stmt
        let last_t = self.typecheck_statement(last_stmt, ret_t);
        if always_returns {
            StatementType::Void
        } else {
            last_t
        }
    }

    /// Typechecks a statement, returning its [`StatementType`].
    ///
    /// `ret_t` is the expected return type of the enclosing function, passed
    /// down so that `return` statements can be checked against it.
    #[instrument(skip(self, ret_t), level = "debug")]
    pub fn typecheck_statement(&mut self, stmt: &Statement, ret_t: &Type) -> StatementType {
        match &stmt.kind {
            StatementKind::Return(es) => self.typecheck_return(es, &stmt.span, ret_t),
            StatementKind::LocalDecl(vd) => {
                self.symtab.add_local_var(
                    vd.name.clone(),
                    vd.var_type.clone(),
                    Some(vd.span.clone()),
                );
                if vd.dims.is_empty() {
                    self.typecheck_scalar_decl(vd)
                } else {
                    // Arrays
                    self.typecheck_arr_decl(vd);
                    StatementType::Unit
                }
            }
            StatementKind::Assign { lhs, rhs } => self.typecheck_assign(lhs, rhs, &stmt.span),
            StatementKind::Compound(block) => self.typecheck_block(block, ret_t),
            StatementKind::If {
                guard,
                then_br,
                else_br,
            } => {
                let e_t = self.typecheck_expression(guard);
                if e_t != Type::Bool && e_t != Type::Error {
                    let mut diag = Diagnostic::error_with_label(
                        guard.span.clone(),
                        "condition of `if` statement must be `bool`",
                        format!("expected `bool`, found `{e_t}`"),
                    );
                    if let Some((s, m)) = self.expr_decl_info(guard) {
                        diag = diag.with_secondary(s, m);
                    }
                    if e_t == Type::Int {
                        diag = diag.with_suggestion(
                            "compare the integer to produce a `bool`, e.g. `x != 0`",
                        );
                    }
                    self.diagnostics.push(diag);
                }

                let then_t = self.typecheck_statement(then_br, ret_t);
                if let Some(s) = else_br.as_ref() {
                    let else_t = self.typecheck_statement(s, ret_t);
                    StatementType::lub(&then_t, &else_t)
                }
                // No else branch then if has to be unit since it can be skipped
                else {
                    StatementType::Unit
                }
            }
            StatementKind::While { guard, body } => {
                let e_t = self.typecheck_expression(guard);
                if e_t != Type::Bool && e_t != Type::Error {
                    let mut diag = Diagnostic::error_with_label(
                        guard.span.clone(),
                        "condition of while loop must be `bool`",
                        format!("expected `bool`, found `{e_t}`"),
                    );
                    if let Some((s, m)) = self.expr_decl_info(guard) {
                        diag = diag.with_secondary(s, m);
                    }
                    if e_t == Type::Int {
                        diag = diag.with_suggestion(
                            "compare the integer to produce a `bool`, e.g. `x != 0`",
                        );
                    }
                    self.diagnostics.push(diag);
                }
                self.typecheck_statement(body, ret_t);
                StatementType::Unit
            }
            StatementKind::Procedure { name, args } => {
                match self.check_fun_args(name, args, &stmt.span) {
                    Type::Error => StatementType::Unit,
                    Type::Tuple { elems } if elems.is_empty() => StatementType::Unit,
                    ret_ty => {
                        let mut diag = Diagnostic::error_with_label(
                            stmt.span.clone(),
                            "procedure call needs empty return type",
                            format!("returns `{ret_ty}`, expected `void`"),
                        )
                        .with_suggestion("assign the return value or discard it with `_ = ...`");

                        if let Some(d_span) =
                            self.symtab.get_opt(name).and_then(|e| e.decl_span.clone())
                        {
                            diag = diag
                                .with_secondary(d_span, format!("function `{name}` defined here"));
                        }
                        self.diagnostics.push(diag);
                        StatementType::Unit
                    }
                }
            }
            StatementKind::Error => StatementType::Unit,
        }
    }

    /// Typechecks a `return` statement.
    ///
    /// Evaluates each return expression, flattens tuple types (from multi-return
    /// function calls), and verifies that the resulting types match the enclosing
    /// function's declared return type `ret_t` in both number and kind.
    fn typecheck_return(&mut self, es: &[Expression], span: &Span, ret_t: &Type) -> StatementType {
        let es_ty: Vec<Type> = es.iter().map(|e| self.typecheck_expression(e)).collect();
        // Function calls return tuples
        let unfold_es_ty = Self::unfold_tuple_types(es_ty);

        let Type::Tuple { elems: ret_elems } = ret_t else {
            panic!("Internal Error: Provied Return Type was not a Tuple");
        };

        // If any returned expression is of type error, stay silent and return
        if unfold_es_ty.iter().any(|t| *t == Type::Error) {
            return StatementType::Void;
        }

        if unfold_es_ty.len() != ret_elems.len() {
            let mut diag = Diagnostic::error_with_label(
                span.clone(),
                "wrong number of return values",
                format!(
                    "expected {} return value(s), found {}",
                    ret_elems.len(),
                    unfold_es_ty.len()
                ),
            );
            if let Some(fun_span) = self.curr_fun_span.clone() {
                diag = diag.with_secondary(
                    fun_span,
                    format!("function declared to return `{ret_t}` here"),
                );
            }
            self.diagnostics.push(diag);
            return StatementType::Void;
        }

        let one_to_one = es.len() == unfold_es_ty.len();
        for (i, (actual, expected)) in unfold_es_ty.iter().zip(ret_elems.iter()).enumerate() {
            if !Self::eq_mod_unknown(actual, expected) {
                let prim_span = if one_to_one {
                    es[i].span.clone()
                } else {
                    span.clone()
                };
                let mut diag = Diagnostic::error_with_label(
                    prim_span,
                    "mismatched return type",
                    format!("expected `{expected}`, found `{actual}`"),
                );
                if one_to_one && let Some((s, m)) = self.expr_decl_info(&es[i]) {
                    diag = diag.with_secondary(s, m);
                }
                if let Some(fun_span) = self.curr_fun_span.clone() {
                    diag = diag.with_secondary(
                        fun_span,
                        format!("function declared to return `{ret_t}` here"),
                    );
                }
                self.diagnostics.push(diag);
            }
        }
        StatementType::Void
    }

    /// Typechecks a scalar variable declaration (no array dimensions).
    ///
    /// Checks the initializer (if present) against the declared type.
    /// Single-element tuples returned by function calls are unwrapped before comparison.
    fn typecheck_scalar_decl(&mut self, vd: &VarDeclaration) -> StatementType {
        if let Some(e) = &vd.init {
            let e_ty = self.typecheck_expression(e);
            if e_ty == Type::Error {
                return StatementType::Unit;
            }
            let e_ty_unfold = if let Type::Tuple { elems } = &e_ty {
                let es_unfold = Self::unfold_tuple_types(elems.clone());
                if es_unfold.len() != 1 {
                    self.diagnostics.push(
                        Diagnostic::error_with_label(
                            e.span.clone(),
                            "mismatched number of values in variable initialization",
                            format!(
                                "expected a single value of type `{}`, found `{e_ty}`",
                                vd.var_type
                            ),
                        )
                        .with_secondary(
                            vd.span.clone(),
                            format!("expected `{}` due to this declaration", vd.var_type),
                        ),
                    );
                    return StatementType::Unit;
                }
                es_unfold[0].clone()
            } else {
                e_ty.clone()
            };

            if e_ty_unfold != Type::Error && !Self::eq_mod_unknown(&e_ty_unfold, &vd.var_type) {
                let mut diag = Diagnostic::error_with_label(
                    e.span.clone(),
                    "mismatched types in variable initialization",
                    format!("expected `{}`, found `{e_ty_unfold}`", vd.var_type),
                )
                .with_secondary(
                    vd.span.clone(),
                    format!("expected `{}` due to this declaration", vd.var_type),
                );
                if let Some((s, m)) = self.expr_decl_info(e) {
                    diag = diag.with_secondary(s, m);
                }
                if vd.var_type == Type::Bool && e_ty_unfold == Type::Int {
                    diag = diag.with_suggestion(
                        "compare the integer expression to produce a bool, e.g. `... != 0`",
                    );
                }
                self.diagnostics.push(diag);
            }
        }
        StatementType::Unit
    }

    /// Typechecks an assignment statement.
    ///
    /// Verifies that all LHS entries are valid l-values, typechecks both sides,
    /// flattens tuple types on the RHS (to handle multi-return function calls),
    /// and checks that the LHS and RHS match in length and types.
    /// Uses [`eq_mod_unknown`](Self::eq_mod_unknown) for comparison so that
    /// the wildcard `_` (typed as `Unknown`) is accepted on the LHS.
    fn typecheck_assign(&mut self, lhs: &[LVal], rhs: &[Expression], span: &Span) -> StatementType {
        let mut bad_lval = false;
        for l in lhs {
            if !is_lval(l) {
                self.diagnostics.push(Diagnostic::error_with_label(
                    l.span(),
                    "invalid assignment target",
                    "cannot assign to this expression",
                ));
                bad_lval = true;
            }
        }

        let lhs_t: Vec<Type> = lhs.iter().map(|s| self.typecheck_lval(s)).collect();

        let rhs_t: Vec<Type> = rhs.iter().map(|e| self.typecheck_expression(e)).collect();

        // If any lval is not an lval, return after typechecking all lval and all right expressions
        if bad_lval {
            return StatementType::Unit;
        }

        let rhs_t_unfolded = Self::unfold_tuple_types(rhs_t);

        if rhs_t_unfolded.iter().any(|t| *t == Type::Error) {
            return StatementType::Unit;
        }

        if lhs_t.len() != rhs_t_unfolded.len() {
            self.diagnostics.push(Diagnostic::error_with_label(
                span.clone(),
                "mismatched number of targets and values in assignment",
                format!(
                    "expected {} value(s) on the right-hand side, found {}",
                    lhs_t.len(),
                    rhs_t_unfolded.len()
                ),
            ));
            return StatementType::Unit;
        }

        let one_to_one = rhs.len() == rhs_t_unfolded.len();

        for (i, (l_ty, r_ty)) in lhs_t.iter().zip(rhs_t_unfolded.iter()).enumerate() {
            if !Self::eq_mod_unknown(l_ty, r_ty) {
                let prim_span = if one_to_one {
                    rhs[i].span.clone()
                } else {
                    span.clone()
                };
                let mut diag = Diagnostic::error_with_label(
                    prim_span,
                    "mismatched types in assignment",
                    format!("expected `{l_ty}`, found `{r_ty}`"),
                );
                match &lhs[i] {
                    LVal::V(vd) => {
                        diag = diag.with_secondary(
                            vd.span.clone(),
                            format!("expected `{l_ty}` due to this declaration"),
                        );
                    }
                    LVal::E(e) => {
                        if let Some((s, m)) = self.expr_decl_info(e) {
                            diag = diag.with_secondary(s, m);
                        }
                    }
                    LVal::Error(_) => (),
                }
                if one_to_one && let Some((s, m)) = self.expr_decl_info(&rhs[i]) {
                    diag = diag.with_secondary(s, m);
                }
                self.diagnostics.push(diag);
            }
        }
        StatementType::Unit
    }

    /// Validates that array dimensions are well-formed.
    ///
    /// A specified dimension (e.g. `[3]`) cannot follow an unspecified one (`[]`).
    /// For example, `int[3][]` is valid but `int[][3]` is not, because the
    /// inner dimension must be known if the outer one is.
    fn dim_check(dims_b: &[bool]) -> bool {
        let mut unspec_bool = false;
        for d_b in dims_b {
            if !*d_b {
                unspec_bool = true;
            }

            if *d_b && unspec_bool {
                return false;
            }
        }
        true
    }

    /// Typechecks an array declaration, verifying that dimension expressions
    /// are integers and that the initializer (if present) matches the declared type.
    fn typecheck_arr_decl(&mut self, vd: &VarDeclaration) {
        //Check out dimensions Convert dims with value to true empty ones to false
        // [1][][2] -> true, false, true
        debug!("Inside array initialzer");
        let dims_bool: Vec<bool> = vd.dims.iter().map(|d| d.is_some()).collect();
        let clean = display_name(&vd.name);

        if !Self::dim_check(&dims_bool) {
            self.diagnostics.push(Diagnostic::error_with_label(
                vd.span.clone(),
                format!("invalid array dimensions for `{clean}`"),
                "a sized dimension cannot follow an unsized `[]` dimension",
            ));
        }

        // Only Int indices allowed!
        for e in vd.dims.iter().flatten() {
            let e_t = self.typecheck_expression(e);
            if e_t != Type::Int && e_t != Type::Error {
                let mut diag = Diagnostic::error_with_label(
                    e.span.clone(),
                    format!("array dimensions for `{clean}` must have type `int`"),
                    format!("expected `int`, found `{e_t}`"),
                );
                if let Some((s, m)) = self.expr_decl_info(e) {
                    diag = diag.with_secondary(s, m);
                }
                self.diagnostics.push(diag);
            }
        }
        // If dimension is specified an initializer is not allowed!
        let has_spec_dim = dims_bool.iter().any(|b| *b);
        match &vd.init {
            Some(_) if has_spec_dim => {
                self.diagnostics.push(
                    Diagnostic::error_with_label(
                        vd.span.clone(),
                        format!("cannot specify dimensions and initialize array `{clean}`"),
                        "sized array declaration cannot have an initializer",
                    )
                    .with_suggestion("remove the explicit size or remove the initializer"),
                );
            }
            Some(e) => {
                let e_t = self.typecheck_expression(e);
                match e_t {
                    Type::Error => {}
                    t if Self::eq_mod_unknown(&t, &vd.var_type) => {}
                    // Happens when array is returned by function call
                    Type::Tuple { elems }
                        if elems.len() == 1 && Self::eq_mod_unknown(&elems[0], &vd.var_type) => {}
                    t => {
                        let mut diag = Diagnostic::error_with_label(
                            e.span.clone(),
                            format!("mismatched initializer type for array `{clean}`"),
                            format!("expected `{}`, found `{t}`", vd.var_type),
                        )
                        .with_secondary(
                            vd.span.clone(),
                            format!("expected `{}` due to this declaration", vd.var_type),
                        );
                        if let Some((s, m)) = self.expr_decl_info(e) {
                            diag = diag.with_secondary(s, m);
                        }
                        self.diagnostics.push(diag);
                    }
                }
            }
            None => (),
        }
    }

    /// Typechecks the left-hand side of an assignment.
    ///
    /// For [`LVal::V`], adds the variable to the symbol table.
    /// For [`LVal::E`], checks that the expression is a valid l-value.
    pub fn typecheck_lval(&mut self, l: &LVal) -> Type {
        match l {
            LVal::E(Expression {
                kind: ExpressionKind::Var(n),
                ..
            }) if n == "_" => Type::Unknown,
            LVal::E(e) => self.typecheck_expression(e),
            LVal::V(vd) => {
                // add to symbol table
                self.symtab.add_local_var(
                    vd.name.clone(),
                    vd.var_type.clone(),
                    Some(vd.span.clone()),
                );
                vd.var_type.clone()
            }
            LVal::Error(_) => Type::Error,
        }
    }

    /// Typechecks an expression, returning its [`Type`].
    //#[instrument(skip(self), level = "debug")]
    pub fn typecheck_expression(&mut self, expr: &Expression) -> Type {
        match &expr.kind {
            ExpressionKind::Error => Type::Error,
            ExpressionKind::Var(name) => match self.symtab.get_opt(name) {
                Some(e) => e.t.clone(),
                None => Type::Error,
            },
            ExpressionKind::Int(_) => Type::Int,
            ExpressionKind::Bool(_) => Type::Bool,
            ExpressionKind::ArrayLit(es) => {
                let es_t: Vec<Type> = es.iter().map(|e| self.typecheck_expression(e)).collect();
                if es_t.is_empty() {
                    return Type::Array {
                        elem_type: Box::new(Type::Unknown),
                    };
                }

                let Some((first_idx, first_t)) =
                    es_t.iter().enumerate().find(|(_, t)| **t != Type::Error)
                else {
                    return Type::Error;
                };

                let mut elem_t = first_t.clone();
                let mut had_error = false;

                for (i, t) in es_t.iter().enumerate() {
                    if *t == Type::Error {
                        continue;
                    }
                    if !Self::eq_mod_unknown(&elem_t, t) {
                        let diag = Diagnostic::error_with_label(
                            es[i].span.clone(),
                            "mismatched element types in array literal",
                            format!("expected `{elem_t}`, found `{t}`"),
                        )
                        .with_secondary(
                            es[first_idx].span.clone(),
                            format!("first element has type `{elem_t}` here"),
                        );
                        self.diagnostics.push(diag);
                        had_error = true;
                    } else {
                        elem_t = Self::unify(&elem_t, t);
                    }
                }

                if had_error {
                    Type::Error
                } else {
                    Type::Array {
                        elem_type: Box::new(elem_t),
                    }
                }
            }
            ExpressionKind::Unary(op, e) => {
                let e_ty = self.typecheck_expression(e);

                if e_ty == Type::Error {
                    return Type::Error;
                }

                let (expected, op_str) = match op {
                    UnaryOp::Neg => (Type::Int, "-"),
                    UnaryOp::Not => (Type::Bool, "!"),
                };
                if e_ty != expected {
                    let mut diag = Diagnostic::error_with_label(
                        e.span.clone(),
                        format!("invalid operand type for unary `{op_str}`"),
                        format!("expected `{expected}`, found `{e_ty}`"),
                    );
                    if let Some((sec_span, sec_msg)) = self.expr_decl_info(e) {
                        diag = diag.with_secondary(sec_span, sec_msg);
                    }
                    self.diagnostics.push(diag);
                    return Type::Error;
                }
                e_ty
            }
            ExpressionKind::Binary(bin_op, e1, e2) => {
                self.typecheck_binary(bin_op, e1, e2, &expr.span)
            }
            ExpressionKind::Call { name, args } => self.typecheck_fun_call(name, args, &expr.span),
            ExpressionKind::Subscript(e1, e2) => {
                let arr_t = self.typecheck_expression(e1);
                let idx_t = self.typecheck_expression(e2);

                if idx_t != Type::Int && idx_t != Type::Error {
                    let mut diag = Diagnostic::error_with_label(
                        e2.span.clone(),
                        "array index must have type `int`",
                        format!("expected `int`, found `{idx_t}`"),
                    );
                    if let Some((sec_span, sec_msg)) = self.expr_decl_info(e2) {
                        diag = diag.with_secondary(sec_span, sec_msg);
                    }
                    self.diagnostics.push(diag);
                }

                match arr_t {
                    Type::Error => Type::Error,
                    Type::Array { elem_type } => *elem_type,
                    other => {
                        let mut diag = Diagnostic::error_with_label(
                            e1.span.clone(),
                            format!("cannot index into non-array type `{other}`"),
                            format!("expected an array, found `{other}`"),
                        );
                        if let Some((sec_span, sec_msg)) = self.expr_decl_info(e1) {
                            diag = diag.with_secondary(sec_span, sec_msg);
                        }
                        self.diagnostics.push(diag);
                        Type::Error
                    }
                }
            }
            ExpressionKind::Dot(e, field) => {
                let e_ty = self.typecheck_expression(e);
                if e_ty == Type::Error {
                    return Type::Error;
                }

                if let Type::Record(name) = e_ty {
                    let Some(entry) = self.symtab.get_opt(&name) else {
                        let mut diag = Diagnostic::error_with_label(
                            expr.span.clone(),
                            format!("unknown record type `{name}`"),
                            format!("record type `{name}` is not defined"),
                        );
                        if let Some((sec_span, sec_msg)) = self.expr_decl_info(e) {
                            diag = diag.with_secondary(sec_span, sec_msg);
                        }
                        self.diagnostics.push(diag);
                        return Type::Error;
                    };
                    let decl_span = entry.decl_span.clone();
                    match &entry.attr {
                        Attr::Record(rec_entry) => match rec_entry.find(field) {
                            Some(md) => md.member_t.clone(),
                            None => {
                                let available: Vec<String> = rec_entry
                                    .members
                                    .iter()
                                    .map(|(m, _)| format!("`{m}`"))
                                    .collect();
                                let mut diag = Diagnostic::error_with_label(
                                    expr.span.clone(),
                                    format!("unknown field `{field}` on record `{name}`"),
                                    format!("record `{name}` has no field named `{field}`"),
                                );
                                if let Some(span) = decl_span {
                                    diag = diag.with_secondary(
                                        span,
                                        format!("record `{name}` defined here"),
                                    );
                                }
                                if !available.is_empty() {
                                    diag = diag.with_suggestion(format!(
                                        "available fields are: {}",
                                        available.join(", ")
                                    ));
                                }
                                self.diagnostics.push(diag);
                                Type::Error
                            }
                        },
                        _ => {
                            let mut diag = Diagnostic::error_with_label(
                                expr.span.clone(),
                                format!("`{name}` is not a record type"),
                                format!("cannot access field `{field}` on `{name}`"),
                            );
                            if let Some(span) = decl_span {
                                diag = diag.with_secondary(span, format!("`{name}` defined here"));
                            }
                            self.diagnostics.push(diag);
                            Type::Error
                        }
                    }
                } else {
                    let mut diag = Diagnostic::error_with_label(
                        e.span.clone(),
                        format!("cannot access field `{field}` on non-record type `{e_ty}`"),
                        format!("expected a record type, found `{e_ty}`"),
                    );
                    if let Some((sec_span, sec_msg)) = self.expr_decl_info(e) {
                        diag = diag.with_secondary(sec_span, sec_msg);
                    }
                    self.diagnostics.push(diag);
                    Type::Error
                }
            }
        }
    }

    /// Typechecks a function call expression.
    ///
    /// Rejects calls to functions with no return value (those are procedure calls
    /// and must appear as statements).
    fn typecheck_fun_call(&mut self, name: &str, args: &[Expression], span: &Span) -> Type {
        match self.check_fun_args(name, args, span) {
            Type::Error => Type::Error,
            Type::Tuple { elems } if elems.is_empty() => {
                let mut diag = Diagnostic::error_with_label(
                    span.clone(),
                    format!("procedure `{name}` does not return a value"),
                    "cannot use a procedure call as an expression",
                )
                .with_suggestion("call it as a standalone statement instead of using its value");
                if let Some(d_span) = self.symtab.get_opt(name).and_then(|e| e.decl_span.clone()) {
                    diag = diag.with_secondary(d_span, format!("procedure `{name}` defined here"));
                }
                self.diagnostics.push(diag);
                Type::Error
            }
            t => t,
        }
    }

    /// Shared helper for function and procedure calls. Verifies that argument
    /// types match the parameter types in the function signature.
    fn check_fun_args(&mut self, name: &str, args: &[Expression], span: &Span) -> Type {
        let args_t: Vec<Type> = args
            .iter()
            .map(|arg| self.typecheck_expression(arg))
            .collect();
        let Some(entry) = self.symtab.get_opt(name) else {
            // Undefined function was already reported by the Resolver, so just propagate the error
            return Type::Error;
        };

        let decl_span = entry.decl_span.clone();
        let (param_t, ret_t, kind) = match entry {
            Entry {
                t:
                    Type::FunType {
                        param_types,
                        ret_type,
                    },
                attr: Attr::Fun { .. },
                ..
            } => (param_types.clone(), ret_type.clone(), "function"),
            Entry {
                t,
                attr: Attr::Record(rec_entry),
                ..
            } => {
                let param_types = rec_entry
                    .members
                    .iter()
                    .map(|(_, mem)| mem.member_t.clone())
                    .collect();
                (param_types, vec![t.clone()], "record")
            }
            other => {
                let clean = display_name(name);
                let mut diag = Diagnostic::error_with_label(
                    span.clone(),
                    format!("`{clean}` is not a function or record"),
                    format!("cannot call value of type `{}`", other.t),
                );
                if let Some(d_span) = decl_span {
                    diag = diag.with_secondary(d_span, format!("`{clean}` was declared here"));
                }
                self.diagnostics.push(diag);
                return Type::Error;
            }
        };

        if param_t.len() != args.len() {
            let mut diag = Diagnostic::error_with_label(
                span.clone(),
                format!("wrong number of arguments to {kind} `{name}`"),
                format!(
                    "expected {} argument(s), found {}",
                    param_t.len(),
                    args.len()
                ),
            );
            if let Some(d_span) = decl_span {
                diag = diag.with_secondary(d_span, format!("{kind} `{name}` defined here"));
            }
            self.diagnostics.push(diag);
            return Type::Error;
        }

        let mut had_error = false;
        for ((expected, actual), arg) in param_t.iter().zip(args_t.iter()).zip(args.iter()) {
            if *actual == Type::Error {
                continue;
            }
            if !Self::eq_mod_unknown(expected, actual) {
                let mut diag = Diagnostic::error_with_label(
                    arg.span.clone(),
                    format!("mismatched argument type in call to `{name}`"),
                    format!("expected `{expected}`, found `{actual}`"),
                );
                if let Some((sec_span, sec_msg)) = self.expr_decl_info(arg) {
                    diag = diag.with_secondary(sec_span, sec_msg);
                }
                if let Some(d_span) = &decl_span {
                    diag = diag
                        .with_secondary(d_span.clone(), format!("{kind} `{name}` defined here"));
                }
                self.diagnostics.push(diag);
                had_error = true;
            }
        }

        if had_error {
            return Type::Error;
        }

        match ret_t.len() {
            0 => Type::Tuple { elems: vec![] },
            1 => ret_t[0].clone(),
            _ => Type::Tuple { elems: ret_t },
        }
    }

    /// Typechecks a binary operation. Arithmetic operators require `int` operands,
    /// comparison operators require matching types and return `bool`, and
    /// logical operators require `bool` operands.
    fn typecheck_binary(
        &mut self,
        op: &BinOp,
        e1: &Expression,
        e2: &Expression,
        span: &Span,
    ) -> Type {
        let e1_ty = self.typecheck_expression(e1);
        let e2_ty = self.typecheck_expression(e2);

        // If either side already is error, stay silent
        if e1_ty == Type::Error || e2_ty == Type::Error {
            return Type::Error;
        }

        match op {
            BinOp::Add if is_arr(&e1_ty) && is_arr(&e2_ty) => {
                if !Self::eq_mod_unknown(&e1_ty, &e2_ty) {
                    let mut diag = Diagnostic::error_with_label(
                        span.clone(),
                        "cannot concatenate arrays of different types",
                        format!("cannot apply `+` to `{e1_ty}` and `{e2_ty}`"),
                    );
                    if let Some((s, m)) = self.expr_decl_info(e1) {
                        diag = diag.with_secondary(s, m);
                    }
                    if let Some((s, m)) = self.expr_decl_info(e2) {
                        diag = diag.with_secondary(s, m);
                    }
                    self.diagnostics.push(diag);
                    return Type::Error;
                }
                Self::unify(&e1_ty, &e2_ty)
            }
            BinOp::Add | BinOp::Sub | BinOp::Mult | BinOp::Div | BinOp::Mod
                if e1_ty == Type::Int && e2_ty == Type::Int =>
            {
                Type::Int
            }
            BinOp::Lt | BinOp::Leq | BinOp::Gt | BinOp::Geq
                if e1_ty == Type::Int && e2_ty == Type::Int =>
            {
                Type::Bool
            }
            BinOp::And | BinOp::Or if e1_ty == Type::Bool && e2_ty == Type::Bool => Type::Bool,
            BinOp::Eq | BinOp::Neq if Self::eq_mod_unknown(&e1_ty, &e2_ty) => Type::Bool,
            _ => {
                let op_s = binop_str(op);
                let mut diag = Diagnostic::error_with_label(
                    span.clone(),
                    format!("invalid operand types for binary `{op_s}`"),
                    format!("cannot apply `{op_s}` to `{e1_ty}` and `{e2_ty}`"),
                );
                if let Some((s, m)) = self.expr_decl_info(e1) {
                    diag = diag.with_secondary(s, m);
                }
                if let Some((s, m)) = self.expr_decl_info(e2) {
                    diag = diag.with_secondary(s, m);
                }
                self.diagnostics.push(diag);
                Type::Error
            }
        }
    }

    /// Returns the declaration span and context label
    /// for a variable or record field expression.
    fn expr_decl_info(&self, expr: &Expression) -> Option<(Span, String)> {
        match &expr.kind {
            ExpressionKind::Var(name) => {
                let entry = self.symtab.get_opt(name)?;
                let decl_span = entry.decl_span.clone()?;
                let clean = display_name(name);
                Some((
                    decl_span,
                    format!("`{clean}` was declared with type `{}` here", entry.t),
                ))
            }
            ExpressionKind::Dot(base, field) => {
                if let ExpressionKind::Var(var_name) = &base.kind
                    && let Some(var_entry) = self.symtab.get_opt(var_name)
                    && let Type::Record(rec_name) = &var_entry.t
                    && let Some(rec_entry) = self.symtab.get_opt(rec_name)
                    && let Attr::Record(rec) = &rec_entry.attr
                    && let Some(mem) = rec.find(field)
                {
                    let decl_span = mem.decl_span.clone()?;
                    return Some((
                        decl_span,
                        format!(
                            "field `{field}` was declared with type `{}` here",
                            mem.member_t
                        ),
                    ));
                }
                None
            }
            _ => None,
        }
    }
}

/// Returns `true` if an [`LVal`] is a valid assignment target, or if it is of kind [`ExpressionKind::Error`].
///
/// Variables, subscripts, and field accesses are valid l-values.
/// Literals, binary operations, and function calls are not.
fn is_lval(l: &LVal) -> bool {
    match l {
        LVal::V(_) => true,
        LVal::E(e) => matches!(
            e.kind,
            ExpressionKind::Var(..)
                | ExpressionKind::Dot(..)
                | ExpressionKind::Subscript(..)
                | ExpressionKind::Error
        ),
        LVal::Error(_) => true,
    }
}

/// Strips the resolver's numeric suffix (e.g. `x.0` -> `x`)
/// for user-facing error messages.
fn display_name(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// Returns `true` if the type is an array type.
pub fn is_arr(t: &Type) -> bool {
    matches!(t, Type::Array { .. })
}

/// Returns the corresponding string of a `BinOp`
fn binop_str(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mult => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::And => "&",
        BinOp::Or => "|",
        BinOp::Lt => "<",
        BinOp::Leq => "<=",
        BinOp::Gt => ">",
        BinOp::Geq => ">=",
        BinOp::Eq => "==",
        BinOp::Neq => "!=",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer::Lexer, parser::Parser, resolver::Resolver};

    fn run_typecheck(src: &str) -> Vec<Diagnostic> {
        let mut parser = Parser::new(Lexer::new(src));
        let (mut prog, _parse_errors) = parser.parse();
        let _resolve_errors = Resolver::new().resolve(&mut prog);
        let mut tc = Typechecker::new();
        let _ = tc.typecheck(&prog);
        tc.diagnostics
    }

    #[test]
    fn poisoned_parse_and_resolve_nodes_emit_no_typecheck_errors() {
        let diags = run_typecheck(
            "record BrokenRec {
                ok_field: int
                : int
            }
            f(a: int, a: int): int {
                x: int = 1 + * 2
                if (* 3) {}
                y: int = undefined_var + 10
                if undefined_bool {}
                arr: int[] = {1, 2, 3}
                z: int = arr[undefined_idx]
                x: int = 99
                x: int, w: int = 1, 2
                missing_proc(undefined_arg)
                return missing_fn(1)
            }",
        );
        assert!(
            diags.is_empty(),
            "expected no cascaded typecheck diagnostics, got: {diags:?}"
        );
    }

    #[test]
    fn type_error_poisoning_suppresses_cascades_and_accumulates_real_errors() {
        let diags = run_typecheck(
            "f(): int {
                x: int = (1 + true) + 5
                if (1 + false) {}
                y: bool = 42
                return true
            }",
        );
        // 4 type errors: `1 + true`, `1 + false`, `y: bool = 42`, and `return true`.
        // `+ 5`, `x: int = ...`, and `if (...)` should not emit a cascaded type error.
        assert_eq!(diags.len(), 4, "got diagnostics: {diags:?}");
    }

    #[test]
    fn secondary_spans_attached_for_vars_params_records_and_functions() {
        let src = "
            record Point {
                x: int
            }
            add(a: int, b: int): int {
                if a {}
                p: Point = Point(1, 2)
                p.x = true
                return true
            }
        ";
        let diags = run_typecheck(src);
        assert_eq!(diags.len(), 4, "got diagnostics: {diags:?}");

        // 1. `if a {}` -> secondary span points to parameter `a: int`
        assert_eq!(diags[0].secondary.len(), 1);
        assert_eq!(&src[diags[0].secondary[0].span.clone()], "a: int");

        // 2. `Point(1,2)` -> wrong arg count -> secondary span points to `record Point`
        assert_eq!(diags[1].secondary.len(), 1);
        assert_eq!(&src[diags[1].secondary[0].span.clone()], "record Point");

        // 3. `p.x = true` -> secondary span points to record field `x: int`
        assert_eq!(diags[2].secondary.len(), 1);
        assert_eq!(&src[diags[2].secondary[0].span.clone()], "x: int");

        // 4. `return true` -> secondary span points to function `add`
        assert_eq!(diags[3].secondary.len(), 1);
        assert_eq!(&src[diags[3].secondary[0].span.clone()], "add");
    }
}
