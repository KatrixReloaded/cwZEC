#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cwzec_node="$(command -v "${NODE_BIN:-node}")"
# Only the explicit public RPC configuration and listen port cross this boundary.
exec env -i SEPOLIA_RPC_URL="${SEPOLIA_RPC_URL:-https://ethereum-sepolia-rpc.publicnode.com}" SIDECAR_PORT="${SIDECAR_PORT:-8788}" "$cwzec_node" "$PWD/sidecar/src/main.ts"
