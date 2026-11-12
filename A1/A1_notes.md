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
*(where `flag` is a fresh synthetic boolean variable name, e.g., `$do_cond` or similar identifier that cannot collide with user variables).*

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
