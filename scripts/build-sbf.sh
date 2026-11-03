#!/usr/bin/env bash
# Build every program to target/deploy/<name>.so — the e2e tests and the
# quickstarts load these.
set -euo pipefail
cd "$(dirname "$0")/.."
for p in programs/*/; do
  echo "building $p"
  (cd "$p" && cargo build-sbf --quiet 2>/dev/null) || (cd "$p" && cargo build-sbf)
done
