#!/bin/sh
# build release binaries of all workspace crates for the host OS into target/release
set -e
cd "$(dirname "$0")/.."
cargo build --release --locked --workspace --bins
for f in target/release/tc-*; do
  case "$f" in *.d) ;; *) [ -f "$f" ] && [ -x "$f" ] && echo "$f" ;; esac
done
