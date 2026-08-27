#!/bin/sh
set -eu

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

echo "quality verification passed"

