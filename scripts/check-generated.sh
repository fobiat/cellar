#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Cargo.lock is the generated workspace resolution file. Metadata must be able
# to use it without rewriting it, and the working tree must stay unchanged.
git diff --exit-code -- Cargo.lock
cargo metadata --locked --format-version 1 --no-deps >/dev/null
git diff --exit-code -- Cargo.lock
