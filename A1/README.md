# Assignment 1 — Running the Examples

The 6 example `.eta` programs are located in `examples/`.

## Run All Examples

From the root directory, run:

```bash
cargo run
```

*(If running from the development repository workspace root instead of the packaged zip, use `cargo run --bin run_examples`.)*

This compiles each `.eta` file in `examples/` (both unoptimized and with `--opt`), executes the generated WebAssembly in `wasmer`, and checks the return value against each file's `// EXPECT:` directive.

To run a single example file:

```bash
cargo run -- examples/01_for_body_only_return.eta
```

## Run Tests

To run the example test suite:

```bash
cargo test
```

To run the full workspace test suite (`rwetac` and `runtime`):

```bash
cargo test --workspace
```
