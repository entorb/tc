#!/bin/sh
# refresh the CodeGraph index agents query (AGENTS.md: CodeGraph is mandatory)
cd "$(dirname "$0")/.." || exit 1
if ! command -v codegraph >/dev/null 2>&1; then
  echo "missing codegraph, install: https://github.com/colbymchenry/codegraph"
  exit 1
fi
if [ ! -d .codegraph ]; then
  echo "no index, run: codegraph init"
  exit 1
fi
out=$(codegraph sync 2>&1) || {
  echo "$out" | head -n 50
  exit 1
}
echo OK
