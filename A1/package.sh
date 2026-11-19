#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_ZIP="${1:-$SCRIPT_DIR/A1_submission.zip}"

# Resolve OUT_ZIP to an absolute path
if [[ "$OUT_ZIP" != /* ]]; then
    OUT_ZIP="$(pwd)/$OUT_ZIP"
fi

STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT

# 1. Copy runtime, rwetac, and architecture.md (referenced by rwetac/src/lib.rs via include_str!)
cp -a "$REPO_ROOT/runtime" "$STAGE_DIR/runtime"
cp -a "$REPO_ROOT/rwetac" "$STAGE_DIR/rwetac"
cp -a "$REPO_ROOT/architecture.md" "$STAGE_DIR/architecture.md"

# 2. Copy Cargo.lock and create a root Cargo.toml with run_examples as the root binary package
cp -a "$REPO_ROOT/Cargo.lock" "$STAGE_DIR/Cargo.lock"
cat > "$STAGE_DIR/Cargo.toml" << 'EOF'
[package]
name = "run_examples"
version = "0.1.0"
edition = "2024"
license.workspace = true

[dependencies]
anyhow = "1.0.99"
rwetac = { path = "rwetac" }
wasmer = "6.0.1"

[workspace]
members = [
    "runtime",
    "rwetac"
]
resolver = "3"

[workspace.package]
license = "MIT"
EOF

# 3. Copy A1/src/main.rs to top-level src/main.rs, A1/README.md to README.md, and A1/examples/* to examples/
mkdir -p "$STAGE_DIR/src"
cp -a "$SCRIPT_DIR/src/main.rs" "$STAGE_DIR/src/main.rs"
cp -a "$SCRIPT_DIR/README.md" "$STAGE_DIR/README.md"

# Dereference symlinks (-L) so examples/* are standalone files that work on all OSes (Windows/macOS/Linux)
cp -rL "$SCRIPT_DIR/examples" "$STAGE_DIR/examples"

# 4. Build the zip archive
rm -f "$OUT_ZIP"
(
    cd "$STAGE_DIR"
    zip -r -q "$OUT_ZIP" README.md runtime rwetac src examples Cargo.toml Cargo.lock architecture.md
)

echo "Created zip archive at: $OUT_ZIP"
