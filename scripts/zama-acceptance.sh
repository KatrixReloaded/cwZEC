#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
umask 077
: "${DATABASE_URL:=postgresql://$(id -un)@127.0.0.1:5432/cwzec_milestone}"
export DATABASE_URL
rustup run 1.92.0 cargo test --manifest-path service/Cargo.toml -p zama --lib --locked --offline
rustup run 1.92.0 cargo test --manifest-path service/Cargo.toml -p app --lib zama --locked --offline
rustup run 1.92.0 cargo build --manifest-path service/Cargo.toml -p app --example zama_acceptance --locked --offline
exec ./service/target/debug/examples/zama_acceptance
