#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export NODE_BIN="$(command -v "${NODE_BIN:-node}")"
cwzec_target="${CARGO_TARGET_DIR:-$PWD/service/target}"
(cd contracts && "$NODE_BIN" node_modules/hardhat/internal/cli/cli.js compile)
rustup run 1.92.0 cargo build --manifest-path service/Cargo.toml -p zama --example cwzec_lifecycle --locked --target-dir "$cwzec_target"
exec "$cwzec_target/debug/examples/cwzec_lifecycle"
