#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Cargo.lock is the generated workspace resolution file. Metadata must be able
# to use it without rewriting the file already under review.
lock_snapshot="$(mktemp)"
trap 'rm -f "$lock_snapshot"' EXIT
cp Cargo.lock "$lock_snapshot"
cargo metadata --locked --format-version 1 --no-deps >/dev/null
cmp --silent "$lock_snapshot" Cargo.lock
