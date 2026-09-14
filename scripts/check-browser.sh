#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ ! -x target/debug/cellar || ! -x target/debug/cellar-fake-server ]]; then
    cargo build -p cellar-cli -p cellar-fake-server
fi

env -u NODE_ENV npm ci --prefix tests/browser
env -u NODE_ENV npm test --prefix tests/browser
