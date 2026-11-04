#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export NODE_BIN="$(command -v "${NODE_BIN:-node}")"
cd contracts
exec "$NODE_BIN" node_modules/hardhat/internal/cli/cli.js test "$@"
