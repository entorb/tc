#!/bin/sh
set -e
# shellcheck disable=SC1091
. "$(dirname "$0")/lib_rs.sh"
# RustSec advisories: vulnerable, unsound, unmaintained crates
rs_check deny "cargo install --locked cargo-deny" cargo deny check advisories
