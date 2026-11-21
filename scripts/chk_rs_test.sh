#!/bin/sh
set -e
# shellcheck disable=SC1091
. "$(dirname "$0")/lib_rs.sh"
# unit, integration and doc tests
rs_check test "rustup update" cargo test --workspace --all-features --locked --quiet
