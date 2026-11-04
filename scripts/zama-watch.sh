#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
exec ./service/target/debug/zama-watch "$@"
