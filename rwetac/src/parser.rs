//! Recursive descent parser for the ETA language.
//!
//! Consumes a token stream from the [`Lexer`] and produces an
//! [`AST`](crate::ast). The parser is hand-written rather than generated, which gives
//! full control over error messages and recovery.
//!
//! Expressions are parsed using precedence climbing (Pratt parsing) via
//! [`Parser::parse_expression`]. Statements and declarations use straightforward
//! recursive descent. The parser uses [`peekmore`] for bounded lookahead — a second
//! token of lookahead is needed to distinguish function declarations from globals
//! at the top level (e.g. `foo(` vs `foo:`).

use crate::diagnostic::Diagnostic;
use crate::lexer::Lexer;
use crate::types::Type;
use crate::util::Merge;
use crate::{ast::*, token::Token};
use logos::Span;
use peekmore::PeekMore;
use peekmore::PeekMoreIterator;
use tracing::{debug, info, instrument, warn};

/// The lexer turns newlines into semicolons, so a [`Token::Semicolon`] found
/// where something else was expected nearly always means a line was broken in
/// the wrong place rather than that the user typed a `;`.
fn asi_hint(found: &Token) -> Option<&'static str> {
    match found {
        Token::Semicolon => Some(
            "Note: the line break here was turned into a statement separator, so this `;` is \
             not in the source. If this line was meant to continue the previous one, end the \
             previous line with the operator or comma",
        ),
        _ => None,
    }
}

/// A recursive descent parser for the ETA language.
///
/// Wraps a [`Lexer`] in a [`PeekMoreIterator`] to allow bounded lookahead.
/// The parser is the second phase of compilation, invoked by [`compile::compile`](crate::compile::compile).
///
/// # Usage
///
/// ```ignore
/// let lexer = Lexer::new(source);
/// let mut parser = Parser::new(lexer);
/// let (ast, errors) = parser.parse();
/// ```
pub struct Parser<'a> {
    lexer: PeekMoreIterator<Lexer<'a>>,
    input_length: usize,
    errors: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    /// Creates a new parser from a [`Lexer`].
    pub fn new(lexer: Lexer<'a>) -> Self {
        let len = lexer.input_length;
        Parser {
            lexer: lexer.peekmore(),
            input_length: len,
            errors: Vec::new(),
        }
    }

    /// Returns `true` if the token stream is exhausted.
    pub fn is_eof(&mut self) -> bool {
        self.lexer.peek().is_none()
    }

    /// Returns the diagnostics accumulated so far during parsing.
    pub fn errors(&self) -> &[Diagnostic] {
        &self.errors
    }

    /// Records a [`Diagnostic`] at `span` and attaches an ASI suggestion when `found` is `;`.
    fn push_error(
        &mut self,
        span: Span,
        found: &Token,
        message: impl Into<String>,
        label: impl Into<String>,
    ) {
        let mut diag = Diagnostic::error_with_label(span, message, label);
        if let Some(hint) = asi_hint(found) {
            diag = diag.with_suggestion(hint);
        }
        self.errors.push(diag);
    }

    /// Records a [`Diagnostic`] for the current lookahead token and its span.
    fn push_unexpected_token(&mut self, message: impl Into<String>, label: impl Into<String>) {
        let found = self.peek().clone();
        let span = self.peek_span();
        self.push_error(span, &found, message, label);
    }

    /// Consumes and returns the next token and its span.
    ///
    /// # Panics
    ///
    /// Panics if called at EOF.
    fn consume(&mut self) -> (Token, Span) {
        match self.lexer.next() {
            Some(s_tok) => s_tok.unwrap(),
            None => panic!("Consume called on EOF"),
        }
    }

    /// Peek at next token **without** advancing the Iterator.
    ///
    /// Returns [`Token::Eof`] if the stream is exhausted.
    fn peek(&mut self) -> &Token {
        match self.lexer.peek() {
            Some(Ok(tok)) => &tok.0,
            _ => &Token::Eof,
        }
    }

    /// Returns the span of the next token without consuming it.
    ///
    /// Useful for constructing error messages when the token itself is not needed.
    fn peek_span(&mut self) -> logos::Span {
        match self.lexer.peek() {
            Some(Ok(tok)) => tok.1.clone(),
            _ => logos::Span {
                start: self.input_length,
                end: self.input_length,
            },
        }
    }

    /// Peeks `n` tokens ahead without consuming anything.
    ///
    /// Used for the two-token lookahead needed to distinguish declarations
    /// at the top level (e.g. `foo(` for functions vs `foo:` for globals).
    fn peek_n(&mut self, n: usize) -> Token {
        match self.lexer.peek_nth(n) {
            Some(Ok(tok)) => tok.0.clone(),
            _ => Token::Eof,
        }
    }

    /// Consumes the next token of the iterator if it matches `kind`.
    ///
    /// If the token matches `kind`, returns its [`Span`]. Otherwise records a
    /// [`Diagnostic`] and returns [`None`] without consuming the token.
    fn expect(&mut self, kind: Token) -> Option<Span> {
        if *self.peek() == kind {
            let span = self.peek_span();
            self.consume();
            Some(span)
        } else {
            let found = self.peek().clone();
            self.push_unexpected_token(
                "unexpected token",
                format!("expected {:?}, found {:?}", kind, found),
            );
            None
        }
    }

    /// Similar to [`Self::expect`], specialized for identifiers.
    fn expect_id(&mut self) -> Option<(String, Span)> {
        let span = self.peek_span();
        if let Token::Var(name) = self.peek() {
            let name = name.clone();
            self.consume();
            Some((name, span))
        } else {
            let found = self.peek().clone();
            self.push_unexpected_token(
                "expected identifier",
                format!("expected identifier, found {:?}", found),
            );
            None
        }
    }

    /// Entry point: parses the full token stream into a [`Program`] and a list of [`Diagnostic`]s.
    // #[instrument(name="Parsing",
    // skip(self))]
    pub fn parse(&mut self) -> (Program, Vec<Diagnostic>) {
        self.parse_program()
    }

    pub fn parse_program(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut decls = Vec::new();
        while !self.is_eof() {
            decls.push(self.parse_declaration());
            // Semcolons automatically added by Lexer
            if *self.peek() == Token::Semicolon {
                self.consume();
            } else if !self.is_eof() && !matches!(decls.last(), Some(Declaration::Error)) {
                let found = self.peek().clone();
                self.push_unexpected_token(
                    "expected `;` after declaration",
                    format!("expected `;`, found {:?}", found),
                );
            }
        }

        (Program { decls }, self.errors.clone())
    }

    /// Synchronizes the parser to the next declaration.
    fn syncronize_decl(&mut self, mut brace_depth: usize) {

        if let Token::Var(_) = self.peek()
            && self.peek_n(1) != Token::LParen && self.peek_n(1) != Token::Colon {
                self.consume();
        }

        while !self.is_eof() {
            let tok = self.peek().clone();
            match tok {
                Token::LBrace => {
                    self.consume();
                    brace_depth += 1;
                }
                Token::RBrace => {
                    self.consume();
                    if brace_depth == 0 {
                        return;
                    }
                    brace_depth -= 1;
                }
                Token::Semicolon => {
                    if brace_depth == 0 {
                        return;
                    } else {
                        self.consume();
                    }
                }
                Token::Record => {
                    if brace_depth == 0 {
                        return;
                    } else {
                        self.consume();
                    }
                }
                Token::Var(_)
                    if brace_depth == 0 && matches!(self.peek_n(1), Token::LParen | Token::Colon) => {
                        return;
                    }
                _ => {
                    self.consume();
                }
            }
        }
    }

    fn parse_declaration(&mut self) -> Declaration {
        match self.peek() {
            Token::Var(_) => match self.peek_n(1) {
                Token::LParen => if let Some(f) = self.parse_function() {
                    Declaration::FunDecl(f)
                } else {
                    Declaration::Error
                },
                Token::Colon => if let Some(vd) = self.parse_variable_declaration() {
                    Declaration::Global(vd)
                } else {
                    self.syncronize_decl(0);
                    Declaration::Error
                },
                _ => {
                    let found = self.peek().clone();
                    self.push_unexpected_token(
                        "invalid declaration",
                        format!("expected function or variable declaration, found {:?}", found),
                    );
                    self.syncronize_decl(0);
                    Declaration::Error
                }
            }
            Token::Record => if let Some(record) = self.parse_record() {
                Declaration::RecordDecl(record)
            } else {
                Declaration::Error
            },
            _ => {
                let found = self.peek().clone();
                self.push_unexpected_token(
                    "invalid declaration",
                    format!("not a valid declaration, found {:?}", found),
                );
                self.syncronize_decl(0);
                Declaration::Error
            }
        }
    }

    /// Parses a record type declaration. Members are newline-separated:
    /// `record Point {` / `x : int` / `y : bool` / `}`.
    fn parse_record(&mut self) -> Option<Record> {
        let Some(span_keyword) = self.expect(Token::Record) else {
            self.syncronize_decl(0);
            return None;
        };
        let Some((tag, span_name)) = self.expect_id() else {
            self.syncronize_decl(0);
            return None;
        };
        let Some(_) = self.expect(Token::LBrace) else {
            self.syncronize_decl(0);
            return None;
        };
        let members = self.parse_members();

        let Some(_) = self.expect(Token::RBrace) else {
            self.syncronize_decl(0);
            return None;
        };

        Some(Record {
            tag,
            members,
            span: span_keyword.merge(&span_name),
        })
    }

    /// Synchronizes the parser to the next member or end of record.
    fn syncronize_member(&mut self) {
        while !self.is_eof() {
            match self.peek() {
                Token::Semicolon => {
                    self.consume();
                    return;
                }
                Token::RBrace => {
                    return; // Leave RBrace for parse_record to see
                }
                _ => {
                    self.consume();
                }
            }
        }
    }

    fn parse_members(&mut self) -> Vec<Member> {
        let mut members = Vec::new();
        // x, y : int allowed!
        while *self.peek() != Token::RBrace && !self.is_eof() {
            let names_spans = self.parse_sep_list_comma(Token::Colon, |p| p.expect_id());
            let Some(_) = self.expect(Token::Colon) else {
                self.syncronize_member();
                members.push(Member::Error);
                continue;
            };
            let (ty, span_ty) = if matches!(self.peek(), Token::RBrace | Token::Semicolon | Token::Eof) {
                let found = self.peek().clone();
                self.push_unexpected_token(
                    "expected member type",
                    format!("expected type, found {:?}", found),
                );
                (Type::Error, self.peek_span())
            } else {
                self.parse_type()
            };
            let mut pushed_any = false;
            // match names with type
            for opt_name in names_spans {
                let Some((name, span)) = opt_name else {
                    members.push(Member::Error);
                    pushed_any = true;
                    continue;
                };
                members.push(Member::Var(MemberDecl {
                    name,
                    t: ty.clone(),
                    span: span.merge(&span_ty),
                }));
                pushed_any = true;
            }
            if !pushed_any {
                members.push(Member::Error);
            }

            if self.expect(Token::Semicolon).is_none() {
                self.syncronize_member();
            }
        }
        members
    }
    // sort(a: int[]) { } ratadd(p1:int, q1:int, p2:int, q2:int) : int, int
    // #[instrument(skip(self))]
    fn parse_function(&mut self) -> Option<Function> {
        let Some((name, span_name)) = self.expect_id() else {
            self.syncronize_decl(0);
            return None;
        };
        info!("Parsing function {}", name);
        let Some(_) = self.expect(Token::LParen) else {
            self.syncronize_decl(0);
            return None;
        };
        // Parse params with their spans
        let mut params = Vec::new();
        let mut param_spans = Vec::new();
        let mut types = Vec::new();
        for (p, p_span, ty) in self.parse_params() {
            params.push(p);
            param_spans.push(p_span);
            types.push(ty);
        }

        let Some(_) = self.expect(Token::RParen) else {
            self.syncronize_decl(0);
            return None;
        };

        // Check return type

        let ret_t: Vec<Type> = match self.peek() {
            Token::Colon => {
                self.consume();
                // Throw away span information
                let vec_span: Vec<(Type, std::ops::Range<usize>)> =
                    self.parse_sep_list_comma(Token::LBrace, |p| p.parse_type());
                vec_span.into_iter().map(|(ty, _)| ty).collect()
            }
            _ => vec![],
        };
        let f_type = Type::FunType {
            param_types: types,
            ret_type: ret_t,
        };
        let Some(_) = self.expect(Token::LBrace) else {
            self.syncronize_decl(0);
            return None;
        };
        // Parse Body
        let body = Block {
            stmts: self.parse_block(),
        };
        let Some(_) = self.expect(Token::RBrace) else {
            self.syncronize_decl(0);
            return None;
        };

        // Construct Type

        Some(Function {
            name,
            params,
            param_spans,
            f_type,
            body,
            span: span_name,
        })
    }

    fn parse_params(&mut self) -> Vec<(Option<String>, Option<Span>, Type)> {
        let ps = |p: &mut Self| {
            let opt_name = p.expect_id();
            let has_colon = p.expect(Token::Colon).is_some();
            let (t, opt_ty_span) = if matches!(
                p.peek(),
                Token::Comma | Token::RParen | Token::LBrace | Token::Semicolon | Token::Eof
            ) {
                if has_colon {
                    let found = p.peek().clone();
                    p.push_unexpected_token(
                        "expected parameter type",
                        format!("expected type, found {:?}", found),
                    );
                }
                (Type::Error, None)
            } else {
                let (ty, ty_span) = p.parse_type();
                (ty, Some(ty_span))
            };
            match opt_name {
                Some((name, name_span)) => {
                    let full_span = match opt_ty_span {
                        Some(ty_span) => name_span.merge(&ty_span),
                        None => name_span,
                    };
                    (Some(name), Some(full_span), t)
                }
                None => (None, None, t),
            }
        };
        self.parse_sep_list_comma(Token::RParen, ps)
    }

    // Only primary types (Used for array declarations)
    fn parse_prim_type(&mut self) -> (Type, Span) {
        if self.is_eof() {
            let span = self.peek_span();
            self.push_error(
                span.clone(),
                &Token::Eof,
                "unexpected end of input",
                "expected type, found Eof",
            );
            return (Type::Error, span);
        }
        // base type like int
        let (t, span) = self.consume();
        let ty = match t {
            Token::TInt => Type::Int,
            Token::TBool => Type::Bool,
            Token::Var(name) => Type::Record(name),
            _ => {
                self.push_error(
                    span.clone(),
                    &t,
                    "expected type",
                    format!("expected type, found {:?}", t),
                );
                return (Type::Error, span);
            }
        };
        (ty, span)
    }

    fn parse_type(&mut self) -> (Type, Span) {
        let (mut ty, span_ty) = self.parse_prim_type();
        // Parse array type
        let mut end_span = span_ty.clone();
        while let Token::LBrack = self.peek() {
            self.consume();
            end_span = self.expect(Token::RBrack).unwrap_or(end_span.clone());
            ty = Type::Array {
                elem_type: Box::new(ty),
            }
        }
        (ty, span_ty.merge(&end_span))
    }

    fn syncronize_stmt(&mut self, mut brace_depth: usize) {
        while !self.is_eof() {
            match self.peek() {
                Token::LBrace => {
                    self.consume();
                    brace_depth += 1;
                }
                Token::RBrace => {
                    if brace_depth == 0 {
                        return;
                    }
                    self.consume();
                    brace_depth -= 1;
                }
                Token::Semicolon => {
                    self.consume();
                    if brace_depth == 0 {
                        return;
                    }
                }
                _ => {
                    self.consume();
                }
            }
        }
    }

    fn parse_block(&mut self) -> Vec<Statement> {
        let mut stmts = Vec::new();
        while *self.peek() != Token::RBrace && !self.is_eof() {
            let stmt = self.parse_statement();
            if matches!(stmt.kind, StatementKind::Error) {
                self.syncronize_stmt(0);
            } else {
                stmts.push(stmt);
            }
        }
        stmts
    }

    fn parse_array_dims(
        &mut self,
        base_ty: Type,
        base_span: Span,
    ) -> (Type, Vec<Option<Expression>>, Span) {
        let mut dims = Vec::new();
        let mut span_end = base_span;
        while *self.peek() == Token::LBrack {
            self.consume(); // [
            let dim_expr = if *self.peek() != Token::RBrack && !self.is_eof() {
                Some(self.parse_expression(0)) // parse e.g. 3, n, etc.
            } else {
                None
            };
            let Some(tmp_span_end) = self.expect(Token::RBrack) else {
                dims.push(Some(dim_expr.unwrap_or_else(|| Expression {
                    kind: ExpressionKind::Error,
                    span: self.peek_span(),
                })));
                break;
            };
            span_end = tmp_span_end;
            dims.push(dim_expr);
        }

        // Base_ty + Number of dims = array type
        let mut var_type = base_ty;
        for _ in 0..dims.len() {
            var_type = Type::Array {
                elem_type: Box::new(var_type),
            };
        }
        (var_type, dims, span_end)
    }

    // len: int = 100
    // 2 n’: int = -1
    // 3 debug: bool = false
    // and all the array stuff
    fn parse_variable_declaration(&mut self) -> Option<VarDeclaration> {
        let (name, span_name) = self.expect_id()?;
        self.expect(Token::Colon)?;
        let (base_ty, span_ty) = self.parse_prim_type();
        let (var_type, dims, _) = self.parse_array_dims(base_ty, span_ty);

        // Check initializer
        let init = match self.peek() {
            Token::Equal => {
                self.consume();
                Some(self.parse_expression(0))
            }
            _ => None,
        };
        Some(VarDeclaration {
            name,
            init,
            var_type,
            dims,
            span: span_name,
        })
    }

    /// Parses a single statement.
    ///
    /// Statement parsing is the most involved part of the parser because
    /// assignments, variable declarations, and procedure calls all start with
    /// an identifier, so the parser must look ahead to distinguish them.
    pub fn parse_statement(&mut self) -> Statement {
        match self.peek() {
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::Return => self.parse_return(),
            Token::Do => self.parse_do_while(),
            Token::For => self.parse_for(),
            Token::Var(_) | Token::Int(_) => {
                // could be assignment or declaration or prcedure call.
                match self.peek_n(1) {
                    Token::LParen => self.parse_procedure(),
                    // If it is multi assign then it can contain variable declartions!
                    // a :int, b = 2, 4 is a multi assign!
                    // a.field = 2 a = 4 b, c[1] = 5 a, a[1] = 3, 4 a : int, b = 2, 3
                    //
                    _ => {
                        // Assign or declaration
                        let mut lvals = self.parse_lvals();
                        // If only one element and its declaratoin then we have a normal declaration and try to parse an initialzier
                        if lvals.len() == 1 && matches!(&lvals[0], LVal::V(_)) {
                            // parse initialzer
                            if let LVal::V(mut vd) = lvals.pop().unwrap() {
                                let (init, init_span) = match self.peek() {
                                    Token::Equal => {
                                        let Some(_) = self.expect(Token::Equal) else {
                                            return Statement {
                                                kind: StatementKind::Error,
                                                span: self.peek_span(),
                                            };
                                        };
                                        let expr = self.parse_expression(0);
                                        let expr_span = expr.span.clone();
                                        (Some(expr), expr_span)
                                    }
                                    _ => (None, vd.span.clone()),
                                };
                                let Some(_) = self.expect(Token::Semicolon) else {
                                    return Statement {
                                        kind: StatementKind::Error,
                                        span: self.peek_span(),
                                    };
                                };

                                vd.init = init;
                                // println!("Full Local : {:?}", vd);
                                let vd_span = vd.span.clone();
                                let kind = StatementKind::LocalDecl(vd);
                                Statement {
                                    kind,
                                    span: vd_span.merge(&init_span),
                                }
                            } else {
                                unreachable!("Should not be possible")
                            }
                        } else {
                            // Need to parse equal and rhs
                            let Some(_) = self.expect(Token::Equal) else {
                                return Statement {
                                    kind: StatementKind::Error,
                                    span: self.peek_span(),
                                };
                            };
                            // Relies on Semicolon
                            let rhs = self.parse_sep_list_comma(Token::Semicolon, |p| {
                                p.parse_expression(0)
                            });
                            if rhs.is_empty() {
                                let found = self.peek().clone();
                                self.push_unexpected_token(
                                    "missing expression",
                                    format!("expected expression after `=`, found {:?}", found),
                                );
                                return Statement {
                                    kind: StatementKind::Error,
                                    span: self.peek_span(),
                                };
                            }
                            let Some(_) = self.expect(Token::Semicolon) else {
                                return Statement {
                                    kind: StatementKind::Error,
                                    span: self.peek_span(),
                                };
                            };
                            let span_start = lvals
                                .first()
                                .map(|l| l.span())
                                .unwrap_or_else(|| self.peek_span());
                            let span_last = rhs.last().unwrap().span.clone();
                            let kind = StatementKind::Assign { lhs: lvals, rhs };
                            Statement {
                                kind,
                                span: span_start.merge(&span_last),
                            }
                        }
                    }
                }
            }
            // { block }
            Token::LBrace => {
                let Some(span_start) = self.expect(Token::LBrace) else {
                    return Statement {
                        kind: StatementKind::Error,
                        span: self.peek_span(),
                    };
                };
                let block = self.parse_block();
                let Some(span_end) = self.expect(Token::RBrace) else {
                    return Statement {
                        kind: StatementKind::Error,
                        span: self.peek_span(),
                    };
                };
                // The lexer only inserts the terminating semicolon when a newline
                // follows `}`, so in `} else {` there is none.
                if *self.peek() != Token::Else {
                    let Some(_) = self.expect(Token::Semicolon) else {
                        return Statement {
                            kind: StatementKind::Error,
                            span: self.peek_span(),
                        };
                    };
                }
                let kind = StatementKind::Compound(Block { stmts: block });
                Statement {
                    kind,
                    span: span_start.merge(&span_end),
                }
            }
            _ => {
                // `.`, `(`, `[` and the binary operators can only continue an
                // expression, so seeing one here means the previous line was
                // ended by an inserted semicolon.
                let tok = self.peek().clone();
                let span = self.peek_span();
                let continues_expr = self.peek_binop().is_some()
                    || matches!(tok, Token::Dot | Token::LParen | Token::LBrack);
                let mut diag = Diagnostic::error_with_label(
                    span.clone(),
                    "unexpected token at start of statement",
                    format!("unexpected {:?}", tok),
                );
                if continues_expr {
                    diag = diag.with_suggestion(
                        "The line break above ended the previous statement. To continue that \
                         expression instead, move this token to the end of the previous line",
                    );
                }
                self.errors.push(diag);
                Statement {
                    kind: StatementKind::Error,
                    span,
                }
            }
        }
    }

    fn parse_return(&mut self) -> Statement {
        let Some(span_key) = self.expect(Token::Return) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        // Relies on Semicolon
        let es = self.parse_sep_list_comma(Token::Semicolon, |p| p.parse_expression(0));
        let Some(_) = self.expect(Token::Semicolon) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let kind = StatementKind::Return(es);
        Statement {
            kind,
            span: span_key,
        }
    }

    fn parse_lvals(&mut self) -> Vec<LVal> {
        let ps = |p: &mut Self| {
            if p.peek_n(1) == Token::Colon {
                // Found a var declaration
                p.parse_local_decl()
            } else {
                // Hopefully an assignable expression like a var or a[1]
                let e = p.parse_expression(0);
                LVal::E(e)
            }
        };

        self.parse_sep_list_comma(Token::Equal, ps)
    }
    // a : int b : int[2][] Local and array Declaration b : int[2] = "ab" allowed!
    // However, here we only parse b:int[2] since this can be part of an multi assignment or normal declaration
    fn parse_local_decl(&mut self) -> LVal {
        let Some((name, span_name)) = self.expect_id() else {
            return LVal::Error(self.peek_span());
        };
        let Some(_) = self.expect(Token::Colon) else {
            return LVal::Error(self.peek_span());
        };
        let (base_ty, base_span) = self.parse_prim_type();
        let (var_type, dims, span_end) = self.parse_array_dims(base_ty, base_span);

        LVal::V(VarDeclaration {
            name,
            init: None,
            var_type,
            dims,
            span: span_name.merge(&span_end),
        })
    }

    fn parse_procedure(&mut self) -> Statement {
        debug!("Parsing procedure");
        let Some((name, span_name)) = self.expect_id() else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let Some(_) = self.expect(Token::LParen) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let args = self.parse_sep_list_comma(Token::RParen, |p| p.parse_expression(0));
        let Some(span_end) = self.expect(Token::RParen) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let Some(_) = self.expect(Token::Semicolon) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let kind = StatementKind::Procedure { name, args };
        Statement {
            kind,
            span: span_name.merge(&span_end),
        }
    }

    fn parse_while(&mut self) -> Statement {
        let Some(span_key) = self.expect(Token::While) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let guard = self.parse_expression(0);
        // Consume optional semicolon that lexer inserted after single-line condition
        if *self.peek() == Token::Semicolon {
            self.consume();
        }
        let body = Box::new(self.parse_statement());
        if matches!(body.kind, StatementKind::Error) {
            self.syncronize_stmt(0);
        }
        let kind = StatementKind::While { guard, body };
        Statement {
            kind,
            span: span_key,
        }
    }



    fn parse_do_while(&mut self) -> Statement {
        let Some(span_key) = self.expect(Token::Do) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };

        let Some(span_start) = self.expect(Token::LBrace) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let body_stmts = self.parse_block();
        let Some(span_end) = self.expect(Token::RBrace) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };

        // Consume optional semicolon that lexer inserted after closing brace newline
        if *self.peek() == Token::Semicolon {
            self.consume();
        }

        let Some(span_while) = self.expect(Token::While) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let guard = self.parse_expression(0);

        // Consume the terminating semicolon (explicit or inserted by newline)
        let Some(span_semi) = self.expect(Token::Semicolon) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };

        let flag_name = format!("_do_flag_{}", span_key.start);
        let flag_decl = Statement {
            kind: StatementKind::LocalDecl(VarDeclaration {
                name: flag_name.clone(),
                init: Some(Expression {
                    kind: ExpressionKind::Bool(true),
                    span: span_key.clone(),
                }),
                var_type: Type::Bool,
                dims: Vec::new(),
                span: span_key.clone(),
            }),
            span: span_key.clone(),
        };

        let inner_span = span_start.merge(&span_end);
        let inner_body = Statement {
            kind: StatementKind::Compound(Block { stmts: body_stmts }),
            span: inner_span.clone(),
        };

        // Update Flag
        let flag_lval = LVal::E(Expression {
            kind: ExpressionKind::Var(flag_name.clone()),
            span: span_key.clone(),
        });
        let flag_update = Statement {
            kind: StatementKind::Assign {
                lhs: vec![flag_lval],
                rhs: vec![guard],
            },
            span: span_while.merge(&span_semi),
        };

        // If `inner_body` unconditionally returns (type `Void`), placing `flag_update`
        // after it would fail `typecheck_block` as unreachable. In that case, place
        // `if (false) { flag_update }` before `inner_body` so the guard is still
        // resolved and typechecked while `inner_body` remains the final statement.
        let while_stmts = if Self::stmt_always_returns(&inner_body) {
            let dead_update = Statement {
                kind: StatementKind::If {
                    guard: Expression {
                        kind: ExpressionKind::Bool(false),
                        span: span_while.clone(),
                    },
                    then_br: Box::new(flag_update),
                    else_br: None,
                },
                span: span_while.merge(&span_semi),
            };
            vec![dead_update, inner_body]
        } else {
            vec![inner_body, flag_update]
        };

        let while_body = Statement {
            kind: StatementKind::Compound(Block {
                stmts: while_stmts,
            }),
            span: span_key.clone(),
        };

        let while_stmt = Statement {
            kind: StatementKind::While {
                guard: Expression {
                    kind: ExpressionKind::Var(flag_name),
                    span: span_key.clone(),
                },
                body: Box::new(while_body),
            },
            span: span_key.clone(),
        };

        // Outer complete block
        Statement {
            kind: StatementKind::Compound(Block {
                stmts: vec![flag_decl, while_stmt],
            }),
            span: span_key.merge(&span_semi),
        }
    }

    fn parse_for(&mut self) -> Statement {
        let Some(span_key) = self.expect(Token::For) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };

        let init_tok = self.peek().clone();
        let init = self.parse_statement();

        if !(matches!(init.kind, StatementKind::Assign { .. } | StatementKind::LocalDecl(_))) {
            if matches!(init.kind, StatementKind::Error) {
                self.syncronize_stmt(0);
                if matches!(self.peek(), Token::RBrace) || self.is_eof() {
                    return Statement {
                        kind: StatementKind::Error,
                        span: self.peek_span(),
                    };
                }
            } else {
                self.push_error(
                    init.span.clone(),
                    &init_tok,
                    "invalid for-loop initializer",
                    "expected assignment or declaration in for loop initializer",
                );
            }
        }

        let guard = self.parse_expression(0);

        let Some(_) = self.expect(Token::Semicolon) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };

        let step_lhs = self.parse_lvals();
        if step_lhs.is_empty() {
            let found = self.peek().clone();
            self.push_unexpected_token(
                "invalid for-loop step",
                format!("expected at least one l-value in for loop step, found {:?}", found),
            );
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        }
        if let Some(LVal::V(vd)) = step_lhs.iter().find(|l| matches!(l, LVal::V(_))) {
            self.push_error(
                vd.span.clone(),
                &Token::Colon,
                "invalid for-loop step",
                "variable declarations are not allowed in for loop step",
            );
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        }
        let Some(_) = self.expect(Token::Equal) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let mut step_rhs = vec![self.parse_expression(0)];
        while *self.peek() == Token::Comma && !self.is_eof() {
            self.consume();
            step_rhs.push(self.parse_expression(0));
        }

        // Consume optional semicolon that lexer inserted after single-line condition
        if *self.peek() == Token::Semicolon {
            self.consume();
        }

        if !(*self.peek() == Token::LBrace) {
            let found = self.peek().clone();
            self.push_unexpected_token(
                "unexpected token",
                format!("expected '{{' after for loop header, found {:?}", found),
            );
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        }

        let body = self.parse_statement();
        let body_span = body.span.clone();

        let step_span = match (step_lhs.first(), step_rhs.last()) {
            (Some(first), Some(last)) => first.span().merge(&last.span),
            _ => span_key.clone(),
        };
        let step_stmt = Statement {
            kind: StatementKind::Assign {
                lhs: step_lhs,
                rhs: step_rhs,
            },
            span: step_span.clone(),
        };

        // If `body` unconditionally returns (type `Void`), placing `step_stmt`
        // after it would fail `typecheck_block` as unreachable. In that case, place
        // `if (false) { step_stmt }` before `body` so the step is still resolved
        // and typechecked while `body` remains the final statement.
        let while_stmts = if Self::stmt_always_returns(&body) {
            let dead_step = Statement {
                kind: StatementKind::If {
                    guard: Expression {
                        kind: ExpressionKind::Bool(false),
                        span: step_span.clone(),
                    },
                    then_br: Box::new(step_stmt),
                    else_br: None,
                },
                span: step_span.clone(),
            };
            vec![dead_step, body]
        } else {
            vec![body, step_stmt]
        };

        let while_body = Statement {
            kind: StatementKind::Compound(Block {
                stmts: while_stmts,
            }),
            span: body_span.merge(&step_span),
        };

        let while_stmt = Statement {
            kind: StatementKind::While {
                guard,
                body: Box::new(while_body),
            },
            span: span_key.merge(&body_span),
        };

        Statement {
            kind: StatementKind::Compound(Block {
                stmts: vec![init, while_stmt],
            }),
            span: span_key.merge(&body_span),
        }
    }

    fn stmt_always_returns(stmt: &Statement) -> bool {
        match &stmt.kind {
            StatementKind::Return(_) => true,
            StatementKind::Compound(block) => {
                block.stmts.last().is_some_and(Self::stmt_always_returns)
            }
            StatementKind::If {
                then_br,
                else_br: Some(else_br),
                ..
            } => Self::stmt_always_returns(then_br) && Self::stmt_always_returns(else_br),
            _ => false,
        }
    }

    fn parse_if(&mut self) -> Statement {
        let Some(span_key) = self.expect(Token::If) else {
            return Statement {
                kind: StatementKind::Error,
                span: self.peek_span(),
            };
        };
        let guard = self.parse_expression(0);
        // Consume optional semicolon that lexer inserted after single-line condition
        if *self.peek() == Token::Semicolon {
            self.consume();
        }
        let then_br = Box::new(self.parse_statement());
        if matches!(then_br.kind, StatementKind::Error) {
            self.syncronize_stmt(0);
        }
        let else_br = if *self.peek() == Token::Else {
            self.consume();
            let br = Box::new(self.parse_statement());
            if matches!(br.kind, StatementKind::Error) {
                self.syncronize_stmt(0);
            }
            Some(br)
        } else {
            None
        };
        let kind = StatementKind::If {
            guard,
            then_br,
            else_br,
        };
        Statement {
            kind,
            span: span_key,
        }
    }

    /// Parses an expression using precedence climbing (Pratt parsing).
    ///
    /// `min_prec` is the minimum precedence level that the parsed expression must have.
    /// Callers start with `0` for a full expression; recursive calls pass `prec + 1`
    /// to enforce left-to-right associativity.
    #[instrument(skip(self))]
    pub fn parse_expression(&mut self, min_prec: usize) -> Expression {
        debug!("Current Token : {}", self.peek());
        let mut lhs = self.parse_unary();

        while let Some(op) = self.peek_binop() {
            let prec = self.binop_precedence(&op);
            if prec < min_prec {
                break;
            }
            self.consume(); // consume operator
            let rhs = self.parse_expression(prec + 1);
            let rhs_span = rhs.span.clone();
            let lhs_span = lhs.span.clone();
            let kind = ExpressionKind::Binary(op, Box::new(lhs), Box::new(rhs));
            lhs = Expression {
                kind,
                span: lhs_span.merge(&rhs_span),
            }
        }
        lhs
    }
    /// Parses a unary expression (`-x`, `!cond`).
    ///
    /// Unary operators bind tighter than any binary operator (precedence 10).
    /// If no unary operator is found, falls through to [`Self::parse_postfix`].
    fn parse_unary(&mut self) -> Expression {
        match self.peek() {
            Token::Minus => {
                let (_, span_key) = self.consume();
                let expr = self.parse_expression(10);
                let expr_span = expr.span.clone();
                let kind = ExpressionKind::Unary(UnaryOp::Neg, Box::new(expr));
                Expression {
                    kind,
                    span: span_key.merge(&expr_span),
                }
            }
            Token::Bang => {
                let (_, span_key) = self.consume();
                let expr = self.parse_expression(10);
                let expr_span = expr.span.clone();
                let kind = ExpressionKind::Unary(UnaryOp::Not, Box::new(expr));
                Expression {
                    kind,
                    span: span_key.merge(&expr_span),
                }
            }
            _ => self.parse_postfix(),
        }
    }

    /// Parses postfix operations: subscript (`a[i]`), field access (`a.x`),
    /// and function calls (`f(args)`).
    ///
    /// These can be chained arbitrarily, e.g. `a[1].field` or `matrix[i][j]`.
    fn parse_postfix(&mut self) -> Expression {
        let mut expr = self.parse_primary();
        // loop for a[1].field etc.
        loop {
            match self.peek() {
                //a[]
                Token::LBrack => {
                    self.consume();
                    let idx = self.parse_expression(0);
                    let Some(span_end) = self.expect(Token::RBrack) else {
                        return Expression {
                            kind: ExpressionKind::Error,
                            span: expr.span.clone(),
                        };
                    };
                    let expr_span = expr.span.clone();
                    let kind = ExpressionKind::Subscript(Box::new(expr), Box::new(idx));
                    expr = Expression {
                        kind,
                        span: expr_span.merge(&span_end),
                    }
                }
                // a.field
                Token::Dot => {
                    self.consume();
                    let Some((field, span_end)) = self.expect_id() else {
                        return Expression {
                            kind: ExpressionKind::Error,
                            span: expr.span.clone(),
                        };
                    };
                    let expr_span = expr.span.clone();
                    let kind = ExpressionKind::Dot(Box::new(expr), field);
                    expr = Expression {
                        kind,
                        span: expr_span.merge(&span_end),
                    }
                }
                // function call
                Token::LParen => {
                    self.consume();
                    let args =
                        self.parse_sep_list_comma(Token::RParen, |p| p.parse_expression(0));
                    let Some(span_end) = self.expect(Token::RParen) else {
                        return Expression {
                            kind: ExpressionKind::Error,
                            span: expr.span.clone(),
                        };
                    };
                    if let ExpressionKind::Var(name) = expr.kind {
                        let kind = ExpressionKind::Call { name, args };
                        expr = Expression {
                            kind,
                            span: expr.span.merge(&span_end),
                        }
                    } else {
                        self.errors.push(Diagnostic::error_with_label(
                            expr.span.clone(),
                            "invalid function call target",
                            "identifier expected before function call",
                        ));
                        return Expression {
                            kind: ExpressionKind::Error,
                            span: expr.span
                        };
                    }
                }
                _ => break,
            }
        }
        expr
    }

    /// Tries to interpret the current token as a binary operator.
    ///
    /// Returns `None` if the current token is not a binary operator,
    /// which signals [`parse_expression`](Self::parse_expression) to stop climbing.
    fn peek_binop(&mut self) -> Option<BinOp> {
        match self.peek() {
            Token::Plus => Some(BinOp::Add),
            Token::Minus => Some(BinOp::Sub),
            Token::Asterisk => Some(BinOp::Mult),
            Token::Div => Some(BinOp::Div),
            Token::Mod => Some(BinOp::Mod),
            Token::And => Some(BinOp::And),
            Token::Bar => Some(BinOp::Or),
            Token::LessThan => Some(BinOp::Lt),
            Token::LessEqual => Some(BinOp::Leq),
            Token::GreaterThan => Some(BinOp::Gt),
            Token::GreaterEqual => Some(BinOp::Geq),
            Token::DoubleEquals => Some(BinOp::Eq),
            Token::NotEquals => Some(BinOp::Neq),
            _ => None,
        }
    }
    /// Returns the precedence level of a binary operator.
    /// Higher values mean tighter binding.
    fn binop_precedence(&self, op: &BinOp) -> usize {
        match op {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Neq => 3,
            BinOp::Lt | BinOp::Leq | BinOp::Gt | BinOp::Geq => 4,
            BinOp::Add | BinOp::Sub => 5,
            BinOp::Mult | BinOp::Div | BinOp::Mod => 6,
        }
    }

    /// Parses a comma-separated list of items, stopping before `end`.
    ///
    /// Used for function arguments, return values, and other comma-delimited sequences.
    /// The `end` token is **not** consumed.
    fn parse_sep_list_comma<F, T>(&mut self, end: Token, mut f: F) -> Vec<T>
    where
        F: FnMut(&mut Self) -> T,
    {
        let mut elems = Vec::new();
        if *self.peek() == end || self.is_eof() {
            return elems;
        }

        elems.push(f(self));
        while *self.peek() == Token::Comma && !self.is_eof() {
            self.consume();
            if self.is_eof() {
                break;
            }
            let e = f(self);
            elems.push(e);
            if *self.peek() == end {
                break;
            }
        }
        elems
    }

    /// Like [`Self::parse_sep_list_comma`] but allows a trailing comma before `end`.
    fn parse_sep_list_comma_trailing<F, T>(&mut self, end: Token, mut f: F) -> Vec<T>
    where
        F: FnMut(&mut Self) -> T,
    {
        let mut elems = Vec::new();

        if *self.peek() == end {
            return elems;
        }

        while !self.is_eof() {
            elems.push(f(self));

            if *self.peek() == Token::Comma {
                self.consume();
                if *self.peek() == end {
                    break; // trailing comma before end is fine
                }
            } else {
                break;
            }
        }

        elems
    }

    /// Parses a primary (atomic) expression: literals, identifiers, parenthesized
    /// expressions, array literals (`{1, 2, 3}`), and string literals.
    fn parse_primary(&mut self) -> Expression {
        if self.is_eof() {
            let span = self.peek_span();
            self.push_error(
                span.clone(),
                &Token::Eof,
                "unexpected end of input",
                "expected expression, found Eof",
            );
            return Expression {
                kind: ExpressionKind::Error,
                span,
            };
        }
        let (tok, span_tok) = self.consume();
        let (kind, span) = match tok {
            Token::Int(num) => (ExpressionKind::Int(num), span_tok),
            Token::Var(name) => (ExpressionKind::Var(name), span_tok),
            Token::True => (ExpressionKind::Bool(true), span_tok),
            Token::False => (ExpressionKind::Bool(false), span_tok),
            Token::LParen => {
                let expr = self.parse_expression(0);
                let Some(span_rparen) = self.expect(Token::RParen) else {
                    return Expression {
                        kind: ExpressionKind::Error,
                        span: expr.span.clone(),
                    };
                };
                (expr.kind, span_tok.merge(&span_rparen))
            }
            Token::LBrace => {
                let expr = self.parse_array_literal();
                (expr.kind, expr.span.merge(&span_tok))
            }
            Token::StringLiteral(lit) => (
                self.parse_string_literal(&lit, span_tok.clone()),
                span_tok,
            ),
            tok => {
                self.push_error(
                    span_tok.clone(),
                    &tok,
                    "unexpected token in expression",
                    format!("expected expression, found {:?}", tok),
                );
                return Expression {
                    kind: ExpressionKind::Error,
                    span: span_tok,
                };
            }
        };
        Expression { kind, span }
    }

    fn parse_array_literal(&mut self) -> Expression {
        let elems = self.parse_sep_list_comma_trailing(Token::RBrace, |p| p.parse_expression(0));
        let Some(span_end) = self.expect(Token::RBrace) else {
            return Expression {
                kind: ExpressionKind::Error,
                span: self.peek_span(),
            };
        };
        Expression {
            kind: ExpressionKind::ArrayLit(elems),
            span: span_end,
        }
    }

    /// Parses a string literal into an array of integer character codes.
    ///
    /// ETA represents strings as `int[]`, so `"Hello"` becomes
    /// `{72, 101, 108, 108, 111}`.
    fn parse_string_literal(&mut self, lit: &str, span: Span) -> ExpressionKind {
        let elems = lit
            .chars()
            .map(|ch| Expression {
                kind: ExpressionKind::Int(ch as i64),
                span: span.clone(),
            })
            .collect();
        ExpressionKind::ArrayLit(elems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_src(src: &str) -> (Program, Vec<Diagnostic>) {
        let lexer = Lexer::new(src);
        let mut parser = Parser::new(lexer);
        parser.parse()
    }

    #[test]
    fn eof_truncated_inputs_do_not_panic() {
        for src in [
            "main(",
            "x:",
            "main() { if",
            "main() { x =",
            "main() { x = 1 +",
            "main() { a,",
            "main() { a: int[",
            "record R { x:",
            "main(a: int,",
        ] {
            let (_prog, errors) = parse_src(src);
            assert!(
                !errors.is_empty(),
                "expected at least one diagnostic for `{src}`"
            );
        }
    }

    #[test]
    fn empty_assignment_rhs_reports_error_without_panicking() {
        let (_prog, errors) = parse_src("main() {\n    x = ;\n}\n");
        assert!(!errors.is_empty());
    }

    #[test]
    fn syncronize_stmt_preserves_enclosing_rbrace() {
        let (prog, errors) = parse_src(
            "main() : int {\n    {\n        1, 2\n    }\n    return 0\n}\nfoo() : int {\n    return 1\n}\n",
        );
        assert!(!errors.is_empty());
        assert_eq!(prog.decls.len(), 2);
        assert!(matches!(prog.decls[0], Declaration::FunDecl(_)));
        assert!(matches!(prog.decls[1], Declaration::FunDecl(_)));
    }

    #[test]
    fn nested_error_in_if_or_while_does_not_duplicate_diagnostic() {
        let (_prog, if_errs) = parse_src("main() : int {\n    if: int = 3\n    return 0\n}\n");
        assert_eq!(if_errs.len(), 2, "got {if_errs:?}");

        let (_prog, while_errs) = parse_src("main() : int {\n    while true\n        + 1\n    return 0\n}\n");
        assert_eq!(while_errs.len(), 1, "got {while_errs:?}");
    }

    #[test]
    fn for_error_init_does_not_emit_duplicate_initializer_error() {
        let (_prog, errs) =
            parse_src("main() {\n    for + 1; true; i = i + 1 {\n    }\n}\n");
        assert_eq!(errs.len(), 1, "got {errs:?}");
    }

    #[test]
    fn params_recovery_keeps_subsequent_params_and_body() {
        let (prog, errs) = parse_src("foo(a, b: int) : int {\n    return b\n}\n");
        assert_eq!(errs.len(), 1, "got {errs:?}");
        match &prog.decls[0] {
            Declaration::FunDecl(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.body.stmts.len(), 1);
            }
            other => panic!("expected FunDecl, got {other:?}"),
        }

        let (prog2, errs2) = parse_src("foo(a: ) : int {\n    return 0\n}\n");
        assert_eq!(errs2.len(), 1, "got {errs2:?}");
        match &prog2.decls[0] {
            Declaration::FunDecl(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.body.stmts.len(), 1);
            }
            other => panic!("expected FunDecl, got {other:?}"),
        }
    }

    #[test]
    fn record_members_push_member_error_and_recover() {
        let (prog, errs) = parse_src("record P {\n    123: int\n    x: int = 5\n    y: int\n}\n");
        assert!(!errs.is_empty());
        match &prog.decls[0] {
            Declaration::RecordDecl(r) => {
                assert!(r.members.iter().any(|m| matches!(m, Member::Error)));
                assert!(r.members.iter().any(|m| matches!(m, Member::Var(v) if v.name == "y")));
            }
            other => panic!("expected RecordDecl, got {other:?}"),
        }
    }

    #[test]
    fn string_literal_elements_use_string_span() {
        let (prog, errs) = parse_src("main() : int[] {\n    return \"hi\"\n}\n");
        assert!(errs.is_empty(), "got {errs:?}");
        match &prog.decls[0] {
            Declaration::FunDecl(f) => match &f.body.stmts[0].kind {
                StatementKind::Return(exprs) => match &exprs[0].kind {
                    ExpressionKind::ArrayLit(elems) => {
                        assert_eq!(elems.len(), 2);
                        assert_eq!(elems[0].span, exprs[0].span);
                    }
                    other => panic!("expected ArrayLit, got {other:?}"),
                },
                other => panic!("expected Return, got {other:?}"),
            },
            other => panic!("expected FunDecl, got {other:?}"),
        }
    }
}
