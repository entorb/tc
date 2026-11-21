#!/bin/sh
# shared helper for chk_rs_*.sh, sourced not run
cd "$(dirname "$0")/.." || exit 1

# usage: rs_check <required cargo subcommand> <install hint> <command...>
rs_check() {
  sub=$1
  hint=$2
  shift 2
  if [ ! -f Cargo.toml ]; then
    echo "SKIP: no Cargo.toml"
    exit 0
  fi
  # probe subcommand directly; cargo --list output differs across environments
  if ! cargo "$sub" --help >/dev/null 2>&1; then
    echo "missing cargo-$sub, install: $hint"
    exit 1
  fi
  out=$(mktemp)
  trap 'rm -f "$out"' EXIT INT TERM
  status=0
  "$@" >"$out" 2>&1 || status=$?
  if [ $status -ne 0 ]; then
    head -n 100 "$out"
  else
    echo OK
  fi
  exit $status
}
