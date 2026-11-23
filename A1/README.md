# Assignment 1 — `for` and `do`/`while` Loops

This package contains the deliverables for Assignment 1:
1. **6 Example Programs:** Located in `examples/`, demonstrating surface loop constructs (`for` and `do`/`while`), nested loops, and scoping.
2. **Runnable Project:** A test and execution harness using `rwetac` and `wasmer`.
3. **Report:** The 1-page solution description and lessons learned are in `assignment_1.pdf`.

---

## Running the Examples

### Run All Examples

From the project root directory, run:

```bash
cargo run
```

*(If running from the development repository workspace root instead of the packaged directory, use `cargo run --bin run_examples`.)*

This compiles every `.eta` file in `examples/` (both unoptimized and with `--opt`), executes the generated WebAssembly in `wasmer`, and validates the output against the `// EXPECT:` directive in each file.

**Expected Output:**

```text
Running 6 Eta example(s)...

  [PASS] 01_for_body_only_return.eta          unopt: 42, opt: 42 (expected: 42)
  [PASS] 02_do_while_body_only_return.eta     unopt: 42, opt: 42 (expected: 42)
  [PASS] 03_for_scoping_and_step.eta          unopt: 566, opt: 566 (expected: 566)
  [PASS] 04_do_while_guard_and_nested.eta     unopt: 66, opt: 66 (expected: 66)
  [PASS] 05_for_with_do_while.eta             unopt: 54, opt: 54 (expected: 54)
  [PASS] 06_do_while_with_for.eta             unopt: 19, opt: 19 (expected: 19)

Summary: 6 passed, 0 failed.
```

### Run a Single Example

To run a specific `.eta` file:

```bash
cargo run -- examples/01_for_body_only_return.eta
```

---

## Running Tests

### Example Verification Test Suite

To verify that all examples pass both unoptimized and optimized execution:

```bash
cargo test
```

### Full Compiler & Workspace Test Suite

To run all workspace tests (including the full `rwetac` compiler test suite: lexer, parser, typechecker, optimizer, and WebAssembly emission):

```bash
cargo test --workspace
```
