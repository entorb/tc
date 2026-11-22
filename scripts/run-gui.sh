#!/bin/sh
# build and run tc-gui (desktop) for the host OS; no cd, so tc-gui opens in the caller's directory
set -e
root="$(dirname "$0")/.."
# keep target/ small: drop build files not touched for 3h, then empty dirs (cargo rebuilds what is missing)
if [ -d "$root/target" ]; then
  find "$root/target" -type f -mmin +180 -delete
  find "$root/target" -mindepth 1 -type d -empty -delete
fi
cargo build --profile fast --manifest-path "$root/Cargo.toml" -p tc-gui
cp "$root/target/fast/tc-gui" ~/bin/
exec "$root/target/fast/tc-gui" "$@"
