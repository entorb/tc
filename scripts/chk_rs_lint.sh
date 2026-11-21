#!/bin/sh
set -e
# shellcheck disable=SC1091
. "$(dirname "$0")/lib_rs.sh"
# local: auto-fix first, then verify; CI runs the plain check command (see check.yml)
# -D warnings also fails on rustc dead_code / unused lints
rs_check clippy "rustup component add clippy" sh -c 'cargo clippy --workspace --all-targets --all-features --locked --fix --allow-dirty --allow-staged; cargo clippy --workspace --all-targets --all-features --locked -- -D warnings'
