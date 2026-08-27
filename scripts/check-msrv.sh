#!/bin/sh
set -eu

toolchain="${SHUE_MSRV_TOOLCHAIN:-1.85.0}"
RUSTUP_TOOLCHAIN="$toolchain" cargo test --workspace --all-targets --locked

echo "minimum Rust verification passed"
