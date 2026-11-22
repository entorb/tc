#!/bin/sh
# build and run tc-gui (desktop) for the host OS; no cd, so tc-gui opens in the caller's directory
set -e
root="$(dirname "$0")/.."
# keep target/ small: drop build files not touched for 1h, then empty dirs (cargo rebuilds what is missing)
if [ -d "$root/target" ]; then
  find "$root/target" -type f -mmin +60 -delete
  find "$root/target" -mindepth 1 -type d -empty -delete
fi
exec cargo run --release --quiet --manifest-path "$root/Cargo.toml" -p tc-gui -- "$@"
cp "$root/target/release/tc-gui" ~/bin/
