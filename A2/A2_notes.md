# Assignment 2 — Notes on Better Diagnostics and Error Recovery

## Overview
Today, `rwetac` stops at the very first error it encounters: `Parser::parse`, `Resolver::resolve`, and `Typechecker::typecheck` all return `Result<_, Error>` using `?`, and `compile.rs` formats the single error as a one-line string (`"<Phase> failed at line:col. <msg>"`).

We upgrade error reporting across the front-end (`source_map.rs`, `ast.rs`, `parser.rs`, `resolver.rs`, `typecheck.rs`, `compile.rs`, plus a new `diagnostic.rs` module) so that the compiler:
1. Emits structured **`Diagnostic`** values with primary/secondary spans and suggestions,
2. Renders them in **rustc-style** multi-line snippets with line numbers, carets, and terminal-aware ANSI colors,
3. Recovers from syntax errors in the **parser** using synchronization points and an `ExpressionKind::Error` poison node, and
4. Accumulates **multiple errors** across parsing, resolution, and typechecking in a single run without cascading false positives.

---

## 1. The `Diagnostic` Type and Rustc-Style Rendering (`diagnostic.rs`, `source_map.rs`)

### Data Model
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Label {
    pub span: logos::Span,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub suggestions: Vec<String>, // rendered as `= help: ...` or `= note: ...`
}
```

### Extending `SourceMap`
Currently, `SourceMap` in `rwetac/src/source_map.rs` stores `_source: String` and `line_starts: Vec<usize>`, exposing only `get_line_column`. We expose helper methods on `SourceMap`:
- `get_line_text(&self, line_num: usize) -> &str`: slices `_source` from `line_starts[line_num - 1]` to the next newline (or EOF), stripping trailing `\r`/`\n`.
- `span_to_Snippet(&self, span: &Span)`: computes `(start_line, start_col)` and `(end_line, end_col)`, clamping zero-width spans (such as EOF spans `input_length..input_length` or synthetic semicolon spans) to length `1` so at least one `^` caret is always drawn.

### Rustc-Style Formatting & Terminal Colors
```text
error: mismatched types in variable initialization
 --> tests/inputs/broken.eta:4:14
  |
2 |     x: int = 5
  |        --- `x` was declared with type `int` here
...
4 |     y: bool = x + 1
  |        ----   ^^^^^ expected `bool`, found `int`
  |        |
  |        expected due to this type annotation
  |
  = help: compare the integer expression to produce a `bool`, e.g. `x + 1 != 0`
```
- **Gutter alignment:** The left gutter width is computed from the maximum line number across the primary and secondary spans (`max_line.to_string().len()`), keeping `-->` and `|` vertically aligned.
- **Primary vs. Secondary carets:** Primary spans use `^` (colored bold red for errors) with their inline message; secondary spans use `-` (colored bold blue/cyan) to provide context (e.g., `"x was declared here as int"` or `"function foo defined here"`).
- **TTY-aware coloring:** Using `std::io::IsTerminal` (`std::io::stderr().is_terminal()` / `stdout().is_terminal()`), ANSI escape codes are emitted only when outputting to a terminal and omitted when piped or snapshotted by `insta` in `tests/file_tests.rs`.

---

## 2. Parser Error Recovery and `ExpressionKind::Error` (`ast.rs`, `parser.rs`)

### Why Parser Recovery Needs Both Statement Sync and `ExpressionKind::Error`
In `parser.rs`, every parse method currently returns `PResult<T>` and aborts via `?`. To report multiple syntax errors per file, the parser stores `pub errors: Vec<Diagnostic>` inside `Parser<'a>` and recovers at three granularities:

1. **Expression-Level Recovery (`ExpressionKind::Error`):**
   When `parse_primary`, `parse_unary`, `parse_postfix`, or `parse_expression` encounters an unexpected token (e.g., `x = 1 + * 2` or unclosed parentheses `f(1, `), it records a `Diagnostic`, consumes the offending token (unless it is a synchronization delimiter like `;`, `}`, `)`, `]`), and returns:
   ```rust
   Expression {
       kind: ExpressionKind::Error,
       span: offending_span,
   }
   ```
   This allows the enclosing statement (`Assign`, `LocalDecl`, `If`, `While`, `Return`) to finish building a valid `Statement` node containing the `ExpressionKind::Error` child.

2. **Statement-Level Synchronization (`synchronize_stmt`):**
   When `parse_statement` fails in `parse_block` (e.g., a malformed LHS `x : = 5` or missing `=` in assignment), `parse_block` catches the error, pushes the `Diagnostic`, and calls `synchronize_stmt()`:
   - Advances the token stream until it consumes a statement terminator (`Token::Semicolon`, which the lexer also inserts on newlines via ASI) or sees a statement/block boundary in the lookahead (`Token::RBrace`, `Token::If`, `Token::While`, `Token::Return`, `Token::Else`, `Token::Eof`) without consuming `}` so the enclosing block can still close cleanly.
   - **Interaction with Automatic Semicolon Insertion (ASI):** Because `Lexer` in `rwetac/src/lexer.rs` turns newlines after semicolon-candidate tokens into `Token::Semicolon`, synchronizing to `Token::Semicolon` naturally recovers at the end of the broken source line.

3. **Top-Level Declaration Synchronization (`synchronize_decl`):**
   If a top-level declaration in `parse_program` fails (`parse_function`, `parse_record`, or global `parse_variable_declaration`), the parser synchronizes by skipping tokens (tracking brace nesting depth `{ ... }`) until the matching closing `}` of the function/record or top-level `;`, and resumes at the next declaration.

---

## 3. Poisoning Semantics for `ExpressionKind::Error` & `Type::Error` (`resolver.rs`, `typecheck.rs`, `types.rs`)

Adding `ExpressionKind::Error` requires updating downstream passes so they can walk a partial AST and report all real semantic errors **without** emitting cascaded "ghost" errors caused by the earlier failure.

### In `Resolver` (`resolver.rs`)
- Instead of stopping at the first `ResolveError`, `Resolver` accumulates `pub errors: Vec<Diagnostic>`.
- To support secondary spans (e.g., *"duplicate variable `x`"* pointing back to *"previous declaration of `x` here"*), `IdEntry` in `StackMap` is extended to store `decl_span: logos::Span`.
- When `resolve_expr` encounters:
  - `ExpressionKind::Error`: does nothing (`Ok(())`), avoiding any complaint.
  - Undeclared `ExpressionKind::Var(x)`: emits an "undefined variable" diagnostic (optionally suggesting a close name from `StackMap` via edit distance), and rewrites the node in-place to `ExpressionKind::Error` (or leaves it and handles it in `Typechecker`) so `Typechecker` does not panic on `self.symtab.get(name)`!
  - Duplicate local/global/function/record declaration: emits a diagnostic with the primary span on the redeclaration and a secondary span on `existing_entry.decl_span`, while keeping the original binding in `StackMap` so subsequent uses still resolve cleanly.

### In `Typechecker` (`typecheck.rs`, `types.rs`)
- **Why `Typechecker::symtab.get` Panics Today:** Currently, `Typechecker::typecheck_expression` calls `self.symtab.get(name)` which panics if `name` is not in `symtab`. If the `Resolver` or `Parser` recovered from an error, a variable or function might be missing from `symtab`, or an expression might be `ExpressionKind::Error`.
- **Introducing `Type::Error` (or reusing poison rules on `Type`):**
  We add `Type::Error` to `enum Type` in `rwetac/src/types.rs` (or treat it distinctly from `Type::Unknown`, since `Type::Unknown` is specifically used for empty array literals `{}` and wildcard `_`).
  - `ExpressionKind::Error` typechecks to `Type::Error` without emitting any new diagnostic.
  - Any operation (`Binary`, `Unary`, `Subscript`, `Dot`, `Call`, `Assign`, `If`/`While` guards, `Return`) that receives `Type::Error` as an operand **silently returns `Type::Error`** (or `StatementType::Unit` / `StatementType::Void` as needed) without emitting a redundant type error.
  - Whenever a real type error occurs in an expression (e.g., `1 + true`, indexing a non-array, accessing a non-existent record field, or calling a function with wrong argument types), the typechecker records the `Diagnostic` into `self.diagnostics` and returns `Type::Error` so enclosing expressions do not trigger a chain of secondary type mismatches.
- **Secondary Spans in Typechecking:**
  By storing `decl_span: Option<Span>` on `symbols::Entry` and `MemberEntry`, type errors can attach rich secondary labels:
  - Variable assignment/initialization mismatch: secondary span on where the variable's type was declared.
  - Function call argument count/type mismatch: secondary span on the function signature declaration.
  - Return type mismatch or missing return on control-flow path (`block_t != StatementType::Void`): secondary span on the function's declared return type.

---

## 4. Pipeline Gatekeeping (`compile.rs`)
- Even though `Parser`, `Resolver`, and `Typechecker` all run and collect diagnostics into a combined list (allowing a single run to show syntax errors, name resolution errors, and type errors from different parts of the file), **compilation halts before `WtacGen`** if any `Severity::Error` diagnostic was emitted.
- This guarantees `WtacGen::generate` only ever lowers a completely valid, error-free AST, keeping codegen and the optimizer free of error-handling clutter.
