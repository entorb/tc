#!/bin/sh
set -e
# shellcheck disable=SC1091
. "$(dirname "$0")/lib_rs.sh"
# Auto-fix Rust formatting; CI gate: cargo fmt --all --check
rs_check fmt "rustup component add rustfmt" cargo fmt --all
