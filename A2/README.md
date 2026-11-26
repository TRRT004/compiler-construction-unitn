# Assignment 2 — Better Diagnostics

This directory contains the runnable before-and-after comparison project for Assignment 2:
1. **`examples/`**: Directory for the broken `.eta` programs demonstrating error reporting.
2. **`original_repo/`**: Git submodule pinned to the original class compiler (`main` branch) used to produce the "Before" output.
3. **`src/main.rs` (`a2_diagnostics`)**: Local runnable harness that runs **both** the original class compiler (`original_repo/rwetac`) and the Assignment 2 compiler (`rwetac`) on each `.eta` file and prints their outputs **side by side**.

*(If you cloned this repository without `--recursive`, run `git submodule update --init` first.)*

---

## Running the Project

### Run on All Examples in `A2/examples/`

From the repository root:

```bash
cargo run --bin a2_diagnostics
```

Or from inside `A2/`:

```bash
cargo run
```

### Run on Specific `.eta` Files or Directories

You can also pass one or more `.eta` file paths or directories as arguments:

```bash
cargo run --bin a2_diagnostics -- rwetac/tests/inputs/invalid/asi_binop_leading.eta rwetac/tests/inputs/invalid_semantic/semantic2.eta
```

---

## Running Tests

To run the `A2` harness tests:

```bash
cargo test -p a2_diagnostics
```

To run the full `rwetac` unit and integration tests:

```bash
cargo test -p rwetac --lib
```
