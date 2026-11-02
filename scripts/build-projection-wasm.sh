#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Install once: rustup target add wasm32-unknown-unknown
cargo build -p rprof-project --target wasm32-unknown-unknown --profile projection
install -m 644 target/wasm32-unknown-unknown/projection/rprof_project.wasm ui/dist/project.wasm
node ui/scripts/check-projection-wasm.mjs --write
