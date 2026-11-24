//! Variable resolution and duplicate detection.
//!
//! The [`Resolver`] walks the AST and renames all variables to globally unique names,
//! eliminating the need for later phases to reason about scoping. It also rejects
//! duplicate declarations within the same scope.
//!
//! After resolution, a program like:
//! ```text
//! x: int = 5
//! { x: int = 7 }
//! ```
//! becomes:
//! ```text
//! x.0: int = 5
//! { x.1: int = 7 }
//! ```
//!
//! This is a precondition for the [`Typechecker`](crate::typecheck::Typechecker) and
//! the [`SymbolTable`](crate::symbols::SymbolTable), which use a flat namespace.

use crate::diagnostic::Diagnostic;
use std::collections::HashMap;
use std::fmt;

use logos::Span;
use tracing::{debug, trace};

use crate::ast::*;

/// An entry in the [`StackMap`], recording the unique name assigned to an identifier.
#[derive(Debug)]
#[allow(dead_code)] // global is not read right now
pub struct IdEntry {
    unique_name: String,
    global: bool,
}

/// A stack of hash maps that mirrors the lexical scope structure of the program.
///
/// Each map represents one scope level. Lookups search from the innermost
/// scope outward, so inner declarations shadow outer ones.
/// Pushing creates a new scope (e.g. entering a block), popping discards it.
#[derive(Debug)]
pub struct StackMap {
    stack: Vec<HashMap<String, IdEntry>>,
}

impl Default for StackMap {
    fn default() -> Self {
        Self::new()
    }
}

impl StackMap {
    /// Creates a new stack map with a single empty scope (the global scope).
    pub fn new() -> Self {
        StackMap {
            stack: vec![HashMap::new()],
        }
    }

    /// Pushes a new empty scope onto the stack (e.g. entering a block or function body).
    pub fn push(&mut self) {
        self.stack.push(HashMap::new());
        trace!("push              {self}");
    }

    /// Pops the innermost scope (e.g. leaving a block).
    ///
    /// # Panics
    ///
    /// Panics if the stack is empty.
    pub fn pop(&mut self) {
        match self.stack.pop() {
            Some(_) => trace!("pop               {self}"),
            None => panic!("Internal Error: Tried to Pop empty Stack Map"),
        }
    }

    /// Adds a binding to the current (innermost) scope.
    pub fn add(&mut self, k: String, v: IdEntry) {
        match self.stack.last_mut() {
            Some(map) => {
                map.insert(k, v);
            }
            None => panic!("Internal Error: Stack Map was empty"),
        }
        trace!("add               {self}");
    }

    /// Looks up a name starting from the innermost scope and searching outward.
    ///
    /// Returns `None` if the name is not found in any scope.
    pub fn find(&self, k: &str) -> Option<&IdEntry> {
        // In reverse order. Since on top is innermost Scope
        let found = self.stack.iter().rev().find_map(|map| map.get(k));
        let op = format!("lookup({k})");
        trace!("{op:<18}-> {}", found.map_or("undefined", |e| &e.unique_name));
        found
    }

    /// Looks up a name in the current scope only, without searching outer scopes.
    ///
    /// Used to detect duplicate declarations within the same scope.
    pub fn find_curr(&self, k: &str) -> Option<&IdEntry> {
        let found = self.stack.last().and_then(|map| map.get(k));
        let op = format!("lookup_curr({k})");
        trace!("{op:<18}-> {}", found.map_or("free", |e| &e.unique_name));
        found
    }
}

/// Prints the stack in the lecture's notation, innermost scope last:
/// `[glob, fun]::[x ↦ x.0]::[y ↦ y.1, x ↦ x.2]`. Renamed locals show their
/// unique name; globals and functions keep their own and are shown bare.
impl fmt::Display for StackMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scopes: Vec<String> = self
            .stack
            .iter()
            .map(|map| {
                let mut entries: Vec<_> = map.iter().collect();
                // A local's suffix is its declaration order; bare names first.
                entries.sort_by_key(|(name, e)| {
                    let n = e.unique_name.rsplit_once('.').map(|(_, n)| n.parse::<usize>().unwrap_or(0));
                    (n, (*name).clone())
                });
                let shown: Vec<String> = entries
                    .iter()
                    .map(|(name, e)| {
                        if **name == e.unique_name {
                            name.to_string()
                        } else {
                            format!("{name} ↦ {}", e.unique_name)
                        }
                    })
                    .collect();
                format!("[{}]", shown.join(", "))
            })
            .collect();
        write!(f, "{}", scopes.join("::"))
    }
}

/// Resolves variable names and checks for duplicates.
///
/// Resolution happens in two passes over the top-level declarations:
/// first, all function and record names are registered (to allow mutual recursion
/// and forward references), then each declaration body is resolved.
pub struct Resolver {
    pub scope_map: StackMap,
    // Local to this resolver, so renaming is reproducible across runs.
    next_id: usize,
    errors: Vec<Diagnostic>,
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}

impl Resolver {
    pub fn new() -> Self {
        Resolver {
            scope_map: StackMap::new(),
            next_id: 0,
            errors: Vec::new(),
        }
    }

    /// Returns the diagnostics accumulated so far during resolution.
    pub fn errors(&self) -> &[Diagnostic] {
        &self.errors
    }

    /// Records a [`Diagnostic`] at `span`.
    fn push_error(
        &mut self,
        span: Span,
        message: impl Into<String>,
        label: impl Into<String>,
    ) {
        let diag = Diagnostic::error_with_label(span, message, label);
        self.errors.push(diag);
    }

    /// Resolves all names in a program, mutating the AST in place.
    ///
    /// Top-level functions and records are registered first (so they can
    /// reference each other), then each body is resolved.
    ///
    /// Returns a vector of [`Diagnostic`]s for any errors encountered during resolution.
    pub fn resolve(&mut self, p: &mut Program) -> Vec<Diagnostic> {
        // Add length function here
        self.scope_map.add(
            "length".to_string(),
            IdEntry {
                unique_name: "length".to_string(),
                global: true,
            },
        );
        p.decls
            .iter_mut()
            .for_each(|d| self.resolve_only_top_level(d));
        debug!(scope_map = %self.scope_map, "Resolved Top Level");
        p.decls.iter_mut().for_each(|d| self.resolve_decl(d));
        self.errors.clone()
    }

    /// Registers top-level function and record names without resolving their bodies.
    fn resolve_only_top_level(&mut self, decl: &mut Declaration) {
        match decl {
            Declaration::Global(_) => (),
            Declaration::FunDecl(fun) => match self.scope_map.find(&fun.name) {
                Some(_) => {
                    self.push_error(
                        fun.span.clone(),
                        format!("Function {} was already defined", fun.name),
                        "Function already defined",
                    );
                    *decl = Declaration::Error;
                }
                None => self.scope_map.add(
                    fun.name.clone(),
                    IdEntry {
                        unique_name: fun.name.clone(),
                        global: true,
                    },
                ),
            },
            Declaration::RecordDecl(record) => match self.scope_map.find(&record.tag) {
                Some(_) => {
                    self.push_error(
                        record.span.clone(),
                        format!("Record {} was already defined", record.tag),
                        "Record already defined",
                    );
                    *decl = Declaration::Error;
                }
                None => self.scope_map.add(
                    record.tag.clone(),
                    IdEntry {
                        unique_name: record.tag.clone(),
                        global: true,
                    },
                ),
            },
            Declaration::Error => (), // Poison node from parser recovery; its error was already reported by the parser.
        }
    }

    /// Resolves a single declaration (global, function, or record).
    pub fn resolve_decl(&mut self, decl: &mut Declaration) {
        match decl {
            Declaration::Global(vd) => match self.scope_map.find_curr(&vd.name) {
                Some(entry) => {
                    self.push_error(
                        vd.span.clone(),
                        format!("Global {} already exists! Entry: {:?}", vd.name, entry),
                        "Global variable already exists",
                    );
                    *decl = Declaration::Error;
                }
                None => self.scope_map.add(
                    vd.name.clone(),
                    IdEntry {
                        unique_name: vd.name.clone(),
                        global: true,
                    },
                ),
            },
            Declaration::FunDecl(fun) => self.resolve_function(fun),
            Declaration::RecordDecl(record) => self.resolve_record(record),
            Declaration::Error => (), // Poison node from parser recovery; its error was already reported by the parser.
        }
    }

    /// Resolves a function: opens a new scope, renames parameters, then resolves the body.
    pub fn resolve_function(&mut self, fun: &mut Function) {
        self.scope_map.push();
        for p in &mut fun.params {
            if let Some(name) = p.take() {
                *p = self.add_local_var(&name, &fun.span);
            }
        }
        self.scope_map.push(); // Scope for body
        fun.body
            .stmts
            .iter_mut()
            .for_each(|s| self.resolve_stmt(s));
        self.scope_map.pop(); // body
        self.scope_map.pop(); // parameters
    }

    /// Resolves a record declaration.
    pub fn resolve_record(&mut self, rec: &mut Record) {
        let resolve_member = |_m| {
            //  I do not think we need to do anything here
        };
        rec.members.iter_mut().for_each(resolve_member)
    }

    /// Adds a local variable to the current scope with a fresh unique name.
    ///
    /// Returns the unique name if successful, or `None` if the variable already exists in the current scope.
    fn add_local_var(&mut self, name: &str, span: &logos::Span) -> Option<String> {
        // Check if var is already defined
        if self.scope_map.find_curr(name).is_some() {
            self.push_error(span.clone(), format!("Duplicate Variable {} found!", name), "Variable already exists");
            return None;
        }

        let unique_name = format!("{}.{}", name, self.next_id);
        self.next_id += 1;
        self.scope_map.add(
            name.to_string(),
            IdEntry {
                unique_name: unique_name.clone(),
                global: false,
            },
        );
        Some(unique_name)
    }

    /// Resolves a local variable declaration, including its initializer if present.
    ///
    /// The initializer is resolved *before* the name is added to the scope,
    /// so `x: int = x` refers to an outer `x`, not itself. Returns `false` if
    /// the name is already declared in the current scope.
    fn resolve_local_var_declaration(&mut self, vd: &mut VarDeclaration) -> bool {
        if let Some(e) = vd.init.as_mut() {
            self.resolve_expr(e)
        }
        match self.add_local_var(&vd.name, &vd.span) {
            Some(unique_name) => {
                vd.name = unique_name;
                true
            }
            None => false,
        }
    }

    /// Resolves the left-hand side of an assignment.
    pub fn resolve_lval(&mut self, lval: &mut LVal) {
        match lval {
            LVal::V(vd) => {
                let span = vd.span.clone();
                if !self.resolve_local_var_declaration(vd) {
                    *lval = LVal::Error(span);
                }
            }
            LVal::E(e) => self.resolve_expr(e),
            LVal::Error(_span) => (), // Poison node from parser recovery; its error was already reported by the parser.
        }
    }

    /// Resolves a function or procedure invocation and its arguments.
    ///
    /// Returns `false` if `name` was not declared, so the caller can poison the node.
    fn resolve_invocation(
        &mut self,
        name: &mut String,
        args: &mut [Expression],
        span: &Span,
        is_procedure: bool,
    ) -> bool {
        let found = if let Some(entry) = self.scope_map.find(name) {
            *name = entry.unique_name.clone();
            true
        } else {
            let (msg, label) = if is_procedure {
                (
                    format!("Procedure {} not declared", name),
                    "Procedure not declared",
                )
            } else {
                (
                    format!("Function {} was not declared!", name),
                    "Function not declared",
                )
            };
            self.push_error(span.clone(), msg, label);
            false
        };
        args.iter_mut().for_each(|arg| self.resolve_expr(arg));
        found
    }

    /// Resolves a statement, opening new scopes for compound blocks.
    pub fn resolve_stmt(&mut self, s: &mut Statement) {
        match &mut s.kind {
            StatementKind::LocalDecl(vd) => {
                let ok = self.resolve_local_var_declaration(vd);
                vd.dims.iter_mut().for_each(|e_opt| {
                    if let Some(e) = e_opt {
                        self.resolve_expr(e)
                    }
                });
                if !ok {
                    s.kind = StatementKind::Error;
                }
            }
            StatementKind::Return(es) => es.iter_mut().for_each(|e| self.resolve_expr(e)),
            StatementKind::Assign { lhs, rhs } => {
                rhs.iter_mut().for_each(|e| self.resolve_expr(e));
                lhs.iter_mut()
                    .for_each(|lval| self.resolve_lval(lval));
            }
            StatementKind::Compound(block) => {
                // New Scope
                self.scope_map.push();
                block
                    .stmts
                    .iter_mut()
                    .for_each(|s| self.resolve_stmt(s));
                self.scope_map.pop();
            }
            StatementKind::If {
                guard,
                then_br,
                else_br,
            } => {
                self.resolve_expr(guard);
                self.resolve_stmt(then_br);
                if let Some(els) = else_br.as_mut() {
                    self.resolve_stmt(els);
                }
            }
            StatementKind::While { guard, body } => {
                self.resolve_expr(guard);
                self.resolve_stmt(body);
            }
            StatementKind::Procedure { name, args } => {
                if !self.resolve_invocation(name, args, &s.span, true) {
                    s.kind = StatementKind::Error;
                }
            }
            StatementKind::Error => (), // Poison node from parser recovery; its error was already reported by the parser.
        }
    }

    /// Resolves an expression, replacing variable names with their unique names.
    ///
    /// The wildcard identifier `_` is left unchanged.
    pub fn resolve_expr(&mut self, e: &mut Expression) {
        match &mut e.kind {
            ExpressionKind::Error => (), // Poison node from parser recovery; its error was already reported by the parser.
            ExpressionKind::Var(n) if n == "_" => (),
            ExpressionKind::Var(x) => match self.scope_map.find(x) {
                Some(entry) => *x = entry.unique_name.clone(),
                None => {
                    self.push_error(
                        e.span.clone(),
                        format!("Undefined Variable {} found!", x),
                        "Undefined Variable",
                    );
                    e.kind = ExpressionKind::Error;
                }
            },
            ExpressionKind::ArrayLit(es) => es.iter_mut().for_each(|e| self.resolve_expr(e)),
            ExpressionKind::Binary(_, e1, e2) => {
                self.resolve_expr(e1);
                self.resolve_expr(e2)
            }
            ExpressionKind::Unary(_, e) => self.resolve_expr(e),
            ExpressionKind::Subscript(e, e1) => {
                self.resolve_expr(e);
                self.resolve_expr(e1);
            }
            ExpressionKind::Dot(e, _) => self.resolve_expr(e),
            ExpressionKind::Call { name, args } => {
                if !self.resolve_invocation(name, args, &e.span, false) {
                    e.kind = ExpressionKind::Error;
                }
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer::Lexer, parser::Parser, typecheck::Typechecker};

    fn parse_and_resolve(src: &str) -> (Program, Vec<Diagnostic>) {
        let mut parser = Parser::new(Lexer::new(src));
        let (mut prog, parse_errors) = parser.parse();
        assert!(parse_errors.is_empty(), "unexpected parse errors: {parse_errors:?}");
        let resolve_errors = Resolver::new().resolve(&mut prog);
        (prog, resolve_errors)
    }

    #[test]
    fn poisons_undefined_var_and_call_and_still_resolves_args() {
        let (prog, errors) = parse_and_resolve(
            "f(): int {
                return missing_fn(missing_arg)
            }",
        );
        assert_eq!(errors.len(), 2);
        let Declaration::FunDecl(fun) = &prog.decls[0] else {
            panic!("expected function");
        };
        let StatementKind::Return(vals) = &fun.body.stmts[0].kind else {
            panic!("expected return");
        };
        assert!(matches!(vals[0].kind, ExpressionKind::Error));
        // Typechecking should not panic on poisoned ExpressionKind::Error (unlike unpoisoned missing names).
        assert!(Typechecker::new().typecheck(&prog).is_err());
    }

    #[test]
    fn poisons_undefined_procedure_and_still_resolves_args() {
        let (prog, errors) = parse_and_resolve(
            "f() {
                missing_proc(missing_arg)
            }",
        );
        assert_eq!(errors.len(), 2);
        let Declaration::FunDecl(fun) = &prog.decls[0] else {
            panic!("expected function");
        };
        assert!(matches!(fun.body.stmts[0].kind, StatementKind::Error));
    }

    #[test]
    fn poisons_duplicate_declarations_and_parameters() {
        let (prog, errors) = parse_and_resolve(
            "f(x: int, x: int): int {
                y: int = 1
                y: int = missing_init
                y: int, z: int = 2, 3
                return x + z
            }
            f(): int {
                return 0
            }",
        );
        // Duplicate param x, duplicate local y, undefined missing_init, duplicate local y in multi-assign, duplicate function f.
        assert_eq!(errors.len(), 5, "errors: {errors:?}");
        assert!(matches!(prog.decls[1], Declaration::Error));
        let Declaration::FunDecl(fun) = &prog.decls[0] else {
            panic!("expected function");
        };
        assert!(fun.params[0].is_some());
        assert!(fun.params[1].is_none());
        assert!(matches!(fun.body.stmts[1].kind, StatementKind::Error));
        let StatementKind::Assign { lhs, .. } = &fun.body.stmts[2].kind else {
            panic!("expected assign");
        };
        assert!(matches!(lhs[0], LVal::Error(_)));
        assert!(matches!(lhs[1], LVal::V(_)));
    }
}
