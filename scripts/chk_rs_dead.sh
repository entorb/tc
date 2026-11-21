#!/bin/sh
set -e
# shellcheck disable=SC1091
. "$(dirname "$0")/lib_rs.sh"
# unused dependencies; dead code itself is caught by chk_rs_lint (dead_code lint)
rs_check shear "cargo install --locked cargo-shear" cargo shear
