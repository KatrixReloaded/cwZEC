#!/bin/sh
# Disposable-schema PostgreSQL tests and controlled RPC/SDK fixtures only.
# No live payment, deployment, key loading, or expensive ignored proof tests.
set -eu
cwzec_rust_bin=$(dirname "$(rustup which --toolchain 1.92.0 rustc)")
export PATH="$cwzec_rust_bin:$PATH"
export RUSTC="$cwzec_rust_bin/rustc"
cd "$(dirname "$0")/../service"
: "${DATABASE_URL:?Set DATABASE_URL to a disposable PostgreSQL database; see README clean setup}"
export DATABASE_URL
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked --offline
cargo test -p app --bin bridge-recovery --locked --offline
cargo test -p app --bin bridge-auto --locked --offline
cargo test -p app --lib --locked --offline
cargo test -p app --test store --locked --offline
cargo test -p app --test startup --locked --offline
cargo test -p zama --lib --locked --offline
cargo test -p zcash --lib --locked --offline

# Actual HTTP client helpers against a controlled loopback peer, no live state.
cd ..
python3 -m unittest discover -s scripts -p test_api_client.py
python3 -m py_compile scripts/api_request.py scripts/recovery_request.py scripts/http-live-check.py
