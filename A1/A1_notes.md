# Assignment 1 — Notes on `for` and `do`/`while` Desugaring

## Overview
We implement `for` and `do`/`while` loops by **parser-level desugaring** in `rwetac/src/parser.rs` (along with adding the `For` and `Do` tokens in `rwetac/src/token.rs`). By desugaring directly into existing `StatementKind` nodes (`Compound`, `While`, `LocalDecl`, `Assign`), downstream compiler passes (`Resolver`, `Typechecker`, `WtacGen`, CFG/Optimizer, `Emitter`) work without needing new AST variants.

---

## 1. `for` Loop Desugaring

### Surface Syntax
```eta
for <init>; <guard>; <step> {
    <body>
}
```
*(Example: `for i: int = 0; i < n; i = i + 1 { ... }`)*

> **Note on `<init>` syntax:** Using a general `<init>` statement (parsed via `lvals`, supporting local declarations like `i: int = 0`, uninitialized declarations like `<var>: <type>`, or assignments to existing variables like `i = 0`, as well as multi-assignments) is both clean and flexible. If needed, it can also be restricted strictly to `<var>: <type> = <expr>` or `<var>: <type>`.

### Desugared AST Equivalent
```eta
{
    <init>;
    while (<guard>) {
        {
            <body>
        }
        <step>;
    }
}
```

### Key Properties
1. **Mandatory Braces (`{ ... }`) for `<body>`:** Since ETA had no prior `for` or `do`/`while` syntax, we require the loop body to be enclosed in braces `{ ... }` (matching the assignment specification `for ... { ... }` and `do { ... } while (e)`). The parser explicitly expects `Token::LBrace` (`{`) and `Token::RBrace` (`}`), producing a parse error if someone writes a single-statement braceless loop body. This also makes `Token::LBrace` an unambiguous delimiter for the end of `<step>` in `for`.
2. **Lexical Scoping of `<init>`:** Wrapping the entire construct in an outer `Compound` block (`{ ... }`) ensures that any loop variable declared in `<init>` (such as `i: int = 0`) is scoped strictly to the `for` loop. The `Resolver` automatically pushes a scope on entering the outer block and pops it on exit, preventing variable leakage or collisions with subsequent `for` loops.
3. **Isolating `<body>` Scope from `<step>`:** While the `while`'s own `Compound` block already places everything inside the loop into a nested scope below `<init>`, wrapping the parsed `{ <body> }` statements in their own inner `Compound` block isolates declarations inside `<body>` from `<step>`. If `<body>` declares a local variable that shadows the loop variable (e.g., `i: int = 42`), that inner scope is popped before `<step>` (`i = i + 1`), ensuring `<step>` always resolves to the loop variable declared in `<init>` rather than the shadowed variable in `<body>`.
4. **Step Execution:** `<step>` is placed at the end of the `while` body so it runs after each iteration of `<body>` before re-evaluating `<guard>`.

---

## 2. `do`/`while` Loop Desugaring

### Surface Syntax
```eta
do {
    <body>
} while (<guard>)
```

### Desugared AST Equivalent
```eta
{
    flag: bool = true;
    while (flag) {
        {
            <body>
        }
        flag = <guard>;
    }
}
```
*(where `flag` is a fresh synthetic boolean variable name `_do_flag_<span_start>`, keyed by the byte offset of the `do` keyword so nested or sequential `do`/`while` loops never collide).*

### Why `flag = <guard>` at the end of the loop (instead of `while (guard | flag)` or body duplication)
1. **No Eager Evaluation Before Iteration 1:**
   In ETA, the binary `|` operator (`BinOp::Or`) is lowered eagerly in `WtacGen::gen_binary` (both operands are evaluated before the `Or` instruction; there is no short-circuiting). Writing `while (guard | flag)` or `while (flag | guard)` would evaluate `<guard>` *before* the first execution of `<body>`, which would break semantics if:
   - `<guard>` relies on variables initialized or updated in `<body>`,
   - `<guard>` contains function calls with side effects, or
   - `<guard>` would trap on the initial state before `<body>` runs (e.g., division by zero or out-of-bounds array indexing).
2. **Why `flag` in `while (guard | flag)` Wouldn't Fold Away at the Loop Header:**
   Even when `<guard>` is initially true, the `while` header block in the CFG has two incoming edges: the entry edge (`flag = true`) and the loop back-edge (`flag = false`). Because Reaching Definitions sees two conflicting definitions of `flag` at the loop header join point, constant propagation cannot fold `flag` away.
3. **Avoids AST Blowup:**
   Unlike duplicating the body (`{ <body>; while (<guard>) { <body> } }`), using `flag = <guard>` keeps a single copy of `<body>` in the AST, avoiding exponential code duplication when `do`/`while` loops are nested.

---

## 3. Handling Unconditionally Returning Loop Bodies (`StatementType::Void`)

### The Problem
In ETA's type system (`rwetac/src/typecheck.rs`), statements have control-flow types: `StatementType::Unit` (may fall through) or `StatementType::Void` (always returns). `Typechecker::typecheck_block` enforces that **every statement in a block except the last must have type `Unit`**, rejecting any statement following a `Void` statement as unreachable (`"This statement always returns, so the statements after it are unreachable"`).

Because our desugaring appends `<step>` (for `for`) or `flag = <guard>` (for `do`/`while`) *after* `<body>` inside the `while` block, a user loop whose `<body>` unconditionally returns (e.g. a direct `return ...` or an `if`/`else` where both branches return) gives `<body>` type `Void`. Naively placing `<step>` or `flag = <guard>` after `<body>` would cause `typecheck_block` to fail on valid source programs.

At the same time, we cannot simply drop `<step>` or `flag = <guard>` when `<body>` returns, because `<step>` and `<guard>` must still undergo name resolution and typechecking (e.g., to reject undeclared identifiers or non-`bool` guards).

### The Solution (`stmt_always_returns`)
In `Parser`, `stmt_always_returns` structurally mirrors the `StatementType::Void` rules of `Typechecker`:
- `StatementKind::Return(_)` $\rightarrow$ `true`
- `StatementKind::Compound(block)` $\rightarrow$ `true` if `block.stmts.last()` always returns
- `StatementKind::If { then_br, else_br: Some(else_br), .. }` $\rightarrow$ `true` if both `then_br` and `else_br` always return
- Everything else $\rightarrow$ `false`

When `stmt_always_returns(&body)` is `true`, the parser wraps the synthetic trailing statement (`<step>` or `flag = <guard>`) in `if (false) { ... }` (with no `else` branch) and places it **before** `<body>`:

```eta
while (<guard_or_flag>) {
    if (false) {
        <step_or_flag_update>;
    }
    {
        <body> // remains the last statement in the block
    }
}
```

Why this works across all compiler phases:
1. **Resolution & Scoping:** `<body>` is wrapped in its own inner `Compound` block, so declarations inside `<body>` are never in scope for `<step>` or `<guard>` anyway; placing `if (false) { ... }` before `<body>` resolves against the exact same scope (`<init>` and outer scopes).
2. **Typechecking:** An `if` without an `else` always has `StatementType::Unit`, so `typecheck_block` checks `<step>` / `flag = <guard>` inside the `then` branch and then allows the `Void` `<body>` as the final statement of the `while` body.
3. **Runtime & Optimization:** At runtime `if (false)` is never taken, so `<body>` executes immediately on iteration 1 and returns; with `--opt`, constant folding and dead-code elimination remove the dead branch entirely. Note also that doing this discovered a bug in `Cfg::get_region` (`rwetac/src/opt/cfg.rs`) when a loop or outer `if` contained a nested `if`/`else` that returned in both branches, which was fixed by preventing `get_region` from swallowing `self.exit_id`.

---

## 4. Deliverable Examples & Test Organization

All end-to-end `.eta` test programs live in `runtime/tests/integration_tests/` (with `// EXPECT: <value>` headers) so `cargo test --workspace` compiles and executes each program in Wasmer both unoptimized and with `--opt`.

To keep the 6 deliverable example programs cleanly organized under `A1/` without duplicating files, `A1/examples/` contains relative symbolic links (`ln -s ../../runtime/tests/integration_tests/<test>.eta A1/examples/<name>.eta`) pointing to the corresponding integration test files.
