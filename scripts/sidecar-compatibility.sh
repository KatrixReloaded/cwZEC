#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export NODE_BIN="$(command -v "${NODE_BIN:-$PWD/.local/node-v24.22.1-darwin-arm64/bin/node}")"
cwzec_target="${CARGO_TARGET_DIR:-$PWD/service/target}"
rustup run 1.92.0 cargo build --manifest-path service/Cargo.toml -p zama --example sdk_compatibility --locked --target-dir "$cwzec_target"
exec "$cwzec_target/debug/examples/sdk_compatibility"
