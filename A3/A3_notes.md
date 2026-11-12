# Assignment 3 — Notes on Available Expressions and Common Subexpression Elimination (CSE)

## Overview
We implement **Available Expressions Analysis** as a forward, must dataflow analysis over the WTAC Control Flow Graph (`Cfg`) and use it to drive **Common Subexpression Elimination (CSE)** across basic blocks.

The implementation integrates into the existing optimization framework (`rwetac/src/opt/`), touching:
- `rwetac/src/opt/avail_expr.rs` *(new module)* — defines `AvailableExpressionsAnalysis` (implementing `solver::Analysis`) and the `cse` transformation pass,
- `rwetac/src/opt/cfg.rs` & `rwetac/src/opt/optimizer.rs` — stores `pub locals: Vec<(String, WType)>` on `Cfg` (instead of discarding `_locals` in `Cfg::new`) so `cse` can register fresh temporary variables, and wires `Pass::Cse` into `run_pass`,
- `rwetac/src/setting.rs` & `rwetac/src/opt/facts.rs` — registers the `cse` pass name for `--passes` and `FactsKind::Available` (or `avail`) for `--facts`.

---

## 1. Representing Expressions in WTAC (`Expr`)

### Which WTAC Instructions Are Candidates?
In `rwetac/src/wtac/wtac_ast.rs`, computations that produce values are:
- `Instruction::Binary { op, src1, src2, dst: Value::Var(_), t }`
- `Instruction::Unary { op, src, dst: Value::Var(_), t }`

We exclude instructions that cannot or should not be treated as pure value expressions:
1. **`Instruction::FCall`:** May have side effects (writing globals, heap memory, I/O) or read modified state.
2. **Memory operands (`Value::Mem`):** WTAC `Binary` and `Unary` operands are `Value::Var` or `Value::Imm` (`gen_and_convert_expression` loads `Value::Mem` via `Instruction::Copy` first), and memory loads are already handled by `AvailableLoadAnalysis` (`Pass::Stl` in `memory.rs`). In case a prior pass propagated a `Value::Mem` into an operand, we restrict `Expr` to operands that are `Value::Var` or `Value::Imm`.

### Canonicalizing Commutative Operations
We define a hashable, comparable canonical expression key (adding `#[derive(PartialEq, Eq, Hash)]` to `BinaryOp` and `UnaryOp` in `wtac_ast.rs`):
```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Binary {
        op: W::BinaryOp,
        src1: W::Value,
        src2: W::Value,
        t: W::WType,
    },
    Unary {
        op: W::UnaryOp,
        src: W::Value,
        t: W::WType,
    },
}
```
For commutative binary operators (`Add`, `Mult`, `And`, `Or`, `Eq`, `Neq`), `Expr::from_instr` orders `(src1, src2)` canonically so `a + b` and `b + a` map to the exact same `Expr`. This allows CSE to eliminate `b + a` when `a + b` is already available.

---

## 2. Available Expressions Analysis (Forward Must Dataflow)

An expression `e` is *available* at a program point if:
1. Every path from the function entry to that program point evaluates `e`, and
2. After the last evaluation of `e` on each path, none of the operand variables of `e` have been redefined.

Crucially, availability is a property of the **expression `e` and its operands**, *not* of the destination variable `x` that originally received `x = e` (even if `x` is later overwritten by `x = 99`, `e` itself remains available as long as `e`'s operands are untouched).

### Lattice and `solver::Analysis` Implementation

Following the pattern of `AvailableCopiesAnalysis` in `rwetac/src/opt/copies.rs`:

| Component | Definition |
| :--- | :--- |
| **Direction** | `Direction::Forwards` |
| **Fact (`Self::Fact`)** | `HashSet<Expr>` — the set of expressions available at a program point |
| **Combine (Meet $\sqcap$)** | Set **intersection** ($\bigcap$) over predecessor out-facts: an expression is available at a block entry iff it is available at the exit of *every* predecessor |
| **Entry `in_fact`** | $\emptyset$ (no expressions are available at function entry) |
| **Non-entry initial `out_fact`** | Universal set $U$ (`all_expressions(cfg)`) — required for a **must** analysis so loop back-edges do not annihilate facts at loop headers on the first solver iteration |

> **Why `init_facts` Must Seed Non-Entry Blocks with the Universal Set $U$:**
> In `Solver::solve` (`rwetac/src/opt/solver.rs`), when a loop header block is processed before its back-edge predecessor, `combine` intersects the entry predecessor's `out_fact` with the back-edge predecessor's `out_fact`. If `out_facts` were initialized to `HashSet::new()` ($\emptyset$), the intersection with $\emptyset$ would always yield $\emptyset$, preventing any expression computed before the loop (or inside the loop along all paths) from ever surviving across the loop header. Seeding non-entry `out_facts` with all expressions in the CFG starts at $\top$ and shrinks monotonically to the maximal fixed point.

### Transfer Function (`flow`)
For an instruction `instr`:
$$\text{OUT}[i] = (\text{IN}[i] \setminus \text{KILL}[i]) \cup \text{GEN}[i]$$
*(Note: In standard textbook order for available expressions, `GEN` happens before `KILL` for the defined variable so `x = x + 1` generates `x + 1` and immediately kills it because `x` was redefined; equivalently, we apply `KILL` first and only `GEN` `e` if `!e.uses_var(&def)`).*

1. **Call Kills (`Instruction::FCall`):**
   A function call may modify any global variable. Therefore, at `Instruction::FCall`, we kill every `expr` that references any global variable in `self.globals`.
2. **Definition Kills (`def_of_instr`):**
   If `instr` defines variable `d` (`Some(d) = liveness::def_of_instr(instr)`):
   - Kill every `expr` in `out_fact` that uses `d` (`expr.uses_var(&d)`).
3. **Generation (`GEN`):**
   If `instr` evaluates candidate expression `e` into `dst = Value::Var(d)`:
   - **Self-overwrite check:** Only insert `e` into `out_fact` if `!e.uses_var(&d)`.
     *(In `x = x + 1`, `x` is modified by the instruction itself, so `x + 1` is not available after the instruction executes.)*

---

## 3. Common Subexpression Elimination (CSE) via Fresh Temporaries

### Why We Always Introduce a Fresh Temporary `cse.k` (Instead of Reusing the Previous Destination)
We **never** directly reuse the destination variable (e.g. `x` or `tmp.5`) of an earlier evaluation `x = a + b`, for two reasons:
1. **Destination Reassignment:** Between `x = a + b` and a later `y = a + b`, `x` might be reassigned (`x = 42`). Because `a` and `b` were not modified, `a + b` is still in `AvailableExpressions`, but `x` no longer holds `a + b`!
2. **Cross-Branch Evaluations (`if`/`else`):** When `a + b` is evaluated in both branches of an `if`/`else`, `WtacGen` assigns the result to *different* variables in each branch (`tmp.1 = a + b` in `then`, `tmp.2 = a + b` in `else`). Neither `tmp.1` nor `tmp.2` dominates the join block.

Using a single fresh temporary `cse.k` for each eliminated expression `e` solves both cases uniformly:

```text
// Before CSE:
if (cond) {
    x.0 : i64 = a.0 + b.1
    x.0 : i64 = 99:i64          // x.0 is overwritten, but a.0 + b.1 is still available!
} else {
    tmp.2 : i64 = a.0 + b.1
}
z.3 : i64 = a.0 + b.1           // Redundant evaluation after join

// After CSE (using fresh temporary cse.0):
if (cond) {
    cse.0 : i64 = a.0 + b.1
    x.0 : i64 = cse.0
    x.0 : i64 = 99:i64
} else {
    cse.0 : i64 = a.0 + b.1
    tmp.2 : i64 = cse.0
}
z.3 : i64 = cse.0
```

> **Note — Why `cse.0 = a.0 + b.1` Stays Inside Each Branch (CSE vs. Code Hoisting):**
> 1. **Dynamic executions vs. static size:** At runtime, only *one* branch of the `if`/`else` executes, so `a.0 + b.1` is computed **once per execution** instead of twice (eliminating the second dynamic evaluation at `z.3`). Moving an expression *above* a branch when both arms compute it is a separate optimization (**Code Hoisting** via **Very Busy Expressions**, a *backward* must analysis).
> 2. **Operands assigned different values in each branch before `a.0 + b.1`:** Even if `a.0` is assigned *different* values in the two branches before `a.0 + b.1` is evaluated:
>    ```text
>    if (cond) {
>        a.0 = 10
>        x.0 = a.0 + b.1   // generates (a.0 + b.1) using a.0 = 10
>    } else {
>        a.0 = 20
>        y.0 = a.0 + b.1   // generates (a.0 + b.1) using a.0 = 20
>    }
>    z.3 = a.0 + b.1       // (a.0 + b.1) IS in out[then] ∩ out[else]!
>    ```
>    Here, `a.0 + b.1` **cannot** be hoisted before the `if` (because `a.0` depends on `cond`), yet `a.0 + b.1` **is** available at `z.3` because neither `a.0` nor `b.1` was modified *after* `a.0 + b.1` was evaluated in each branch. Placing `cse.0 = a.0 + b.1` at the evaluation site inside each branch captures `10 + b.1` or `20 + b.1` respectively, making `z.3 = cse.0` completely valid after the join.

### The CSE Algorithm Step-by-Step
1. **Identify Redundant Evaluations:**
   Walk the CFG and find any instruction `(block_id, instr_id)` computing expression `e` (`dst = e`) where `e` is already in `res.instr_in(block_id, instr_id)` (and ensure the instruction is not already writing to a `cse.*` temporary from an earlier iteration).
2. **Allocate One Fresh Temporary `cse.k` per Eliminated `Expr`:**
   For each distinct redundant expression `e` of type `t`:
   - Create a fresh local variable `cse.k` (`Value::Var(format!("cse.{k}"))`) and push `(format!("cse.{k}"), t)` onto `cfg.locals` (required so `Emitter` in `rwetac/src/emitter.rs` finds `cse.k` in `Bindings::locals` and emits `(local $cse.k ...)`).
3. **Rewrite Reaching Evaluation Sites and Redundant Sites:**
   - Search backward along CFG paths from each redundant site `(block_id, instr_id)` to find the reaching evaluation sites of `e` (the last evaluation of `e` along each incoming path).
   - At each reaching evaluation site `d = e`: replace it with two instructions:
     1. `cse.k : t = e` (computes `e` into the shared temporary)
     2. `d : t = cse.k` (copies `cse.k` into the original destination `d`)
   - At each redundant site `d2 = e` dominated by/reached by those evaluations: replace it with a single copy:
     - `d2 : t = cse.k`
4. **Renumber Block Instruction IDs:**
   Because inserting `d = cse.k` adds an instruction to `block.lab_instructions`, we re-index `block.lab_instructions` using `cfg::get_instr_id(block_id, idx)` so every instruction retains a unique `(block_id, instr_id)` key for subsequent dataflow passes in the fixpoint loop.

---

## 4. Interaction with Copy Propagation (`Pass::Copy`) and DCE (`Pass::Dce`)

Because CSE always introduces `cse.k : t = e; d : t = cse.k` and leaves cleanup to downstream passes, it composes cleanly inside `fixpoint(cp,copy,cf,cse,dce,...)`:

1. **When `d` Was Not Modified (`tmp.5 = a + b` ... `tmp.9 = a + b`):**
   - **CSE** rewrites the first site to `cse.0 = a + b; tmp.5 = cse.0` and the second site to `tmp.9 = cse.0`.
   - **Copy Propagation (`Pass::Copy`)** sees `tmp.5 = cse.0` and `tmp.9 = cse.0` in `AvailableCopiesAnalysis` and replaces all uses of `tmp.5` and `tmp.9` with `cse.0`.
   - **Dead Code Elimination (`Pass::Dce`)** sees `tmp.5` and `tmp.9` are now dead and removes both `tmp.5 = cse.0` and `tmp.9 = cse.0`, leaving only `cse.0 = a + b`!
2. **When `d` Was Modified Afterwards (`x.0 = a + b; x.0 = 99; z.3 = a + b`):**
   - `z.3 = cse.0` safely reads the preserved value in `cse.0` instead of the clobbered `x.0`, while `Copy` and `Dce` still propagate `cse.0` into `z.3`'s uses and eliminate dead copies.
3. **When `e` Was Computed in Both Branches of `if`/`else`:**
   - Both branches assign to the *same* `cse.0`, so at the merge block `z.3 = cse.0` works directly without φ-nodes.
