#!/bin/sh
# build and run tc-cli for the host OS; no cd, so tc-cli opens in the caller's directory
set -e
root="$(dirname "$0")/.."
# keep target/ small: drop build files not touched for 3h, then empty dirs (cargo rebuilds what is missing)
if [ -d "$root/target" ]; then
  find "$root/target" -type f -mmin +180 -delete
  find "$root/target" -mindepth 1 -type d -empty -delete
fi
cargo build --profile fast --manifest-path "$root/Cargo.toml" --bin tc-cli
cp "$root/target/fast/tc-cli" ~/bin/
exec "$root/target/fast/tc-cli" "$@"
