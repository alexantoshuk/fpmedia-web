#!/usr/bin/env bash
# CONTRIBUTING gates shared by CI (`.github/workflows/check.yml`) and
# `.githooks/pre-push`. Trunk build and e2e stay out — run those locally.
# Wasm `cargo check` omitted: Clippy --target wasm32 already typechecks those crates.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "check: fmt"
cargo fmt --all -- --check

echo "check: clippy (engine)"
cargo clippy -p fpmedia-web --all-targets --all-features -- -D warnings

echo "check: clippy (demo-player wasm)"
cargo clippy --target wasm32-unknown-unknown -p fpmedia-web-demo-player -- -D warnings

echo "check: clippy (components wasm)"
cargo clippy --target wasm32-unknown-unknown -p fpmedia-web-components -- -D warnings

echo "check: clippy (components native)"
cargo clippy -p fpmedia-web-components --all-targets -- -D warnings

echo "check: test (engine)"
cargo test -p fpmedia-web

echo "check: test (components)"
cargo test -p fpmedia-web-components

echo "check: ok"
