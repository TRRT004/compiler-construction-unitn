#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_ZIP="${1:-$SCRIPT_DIR/A2_submission.zip}"

if [[ "$OUT_ZIP" != /* ]]; then
    OUT_ZIP="$(pwd)/$OUT_ZIP"
fi

STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT

cp -a "$REPO_ROOT/rwetac" "$STAGE_DIR/rwetac"
mkdir -p "$STAGE_DIR/original_repo"
cp -a "$SCRIPT_DIR/original_repo/Cargo.toml" "$STAGE_DIR/original_repo/Cargo.toml"
cp -a "$SCRIPT_DIR/original_repo/Cargo.lock" "$STAGE_DIR/original_repo/Cargo.lock"
cp -a "$SCRIPT_DIR/original_repo/architecture.md" "$STAGE_DIR/original_repo/architecture.md"
cp -a "$SCRIPT_DIR/original_repo/rwetac" "$STAGE_DIR/original_repo/rwetac"
cp -a "$REPO_ROOT/architecture.md" "$STAGE_DIR/architecture.md"
cp -a "$REPO_ROOT/Cargo.lock" "$STAGE_DIR/Cargo.lock"

cat > "$STAGE_DIR/Cargo.toml" << 'EOF'
[package]
name = "a2_diagnostics"
version = "0.1.0"
edition = "2024"
license.workspace = true

[dependencies]
anyhow = "1.0.99"
rwetac = { path = "rwetac" }

[workspace]
members = [
    "rwetac"
]
resolver = "3"

[workspace.package]
license = "MIT"
EOF

mkdir -p "$STAGE_DIR/src" "$STAGE_DIR/examples"
cp -a "$SCRIPT_DIR/src/main.rs" "$STAGE_DIR/src/main.rs"
cp -a "$SCRIPT_DIR/README.md" "$STAGE_DIR/README.md"

for f in "$SCRIPT_DIR/examples"/*.eta; do
    [ -e "$f" ] || continue
    cp -L "$f" "$STAGE_DIR/examples/$(basename "$f")"
done

rm -f "$OUT_ZIP"
(
    cd "$STAGE_DIR"
    zip -r -q "$OUT_ZIP" README.md rwetac original_repo src examples Cargo.toml Cargo.lock architecture.md
)

echo "Created zip archive at: $OUT_ZIP"
