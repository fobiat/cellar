#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# The test loads every checked-in profile, the example config, and the embedded
# Kubernetes config. Keep that contract easy to run from CI and a clean clone.
cargo test -p cellar-core config::tests::every_shipped_profile_still_resolves_to_exactly_one_instance -- --exact
