#!/bin/sh
set -e

root="$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)/.."
cd "$root"

echo "==> Confirming cooldown config is active:"
cargo -Vv 2>/dev/null || true
grep -A2 'global-min-publish-age' .cargo/config.toml &&
  echo "   ^ cooldown active from .cargo/config.toml"

# 1) Refresh the lockfile to the newest cooldown-safe versions.
# 'cargo upgrade' (cargo-edit) is not used: it ignores the cooldown.
echo "==> Updating Cargo.lock..."
cargo update

# 2) Sanity check.
echo "==> Building..."
cargo build

echo "DONE"
