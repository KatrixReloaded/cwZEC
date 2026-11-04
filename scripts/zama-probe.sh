#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export NODE_BIN="$(command -v "${NODE_BIN:-node}")"
case "${1:-init}" in
  init|run) ;;
  *) echo 'Usage: scripts/zama-probe.sh [init|run]' >&2; exit 2 ;;
esac
if [[ "${1:-init}" == run ]]; then
  "$NODE_BIN" probes/zama/compile.mjs
fi
rustup run 1.92.0 cargo build --manifest-path service/Cargo.toml -p zama --example zama_probe --locked
exec service/target/debug/examples/zama_probe "${1:-init}"
