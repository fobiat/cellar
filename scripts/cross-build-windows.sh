#!/usr/bin/env bash
# Cross-compile the Windows binaries from Linux, in a container.
#
# The dedicated server is Windows-only, so a Windows build of the manager is not
# a nicety. This runs in Docker rather than needing mingw on the host, so the
# same command works on a developer machine and in CI.
#
# Output lands in dist/windows/.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/dist/windows"
mkdir -p "$out"

image="cellar-cross-windows"

# Built once and cached. Installing mingw on every run would dominate the build
# time and would break the moment the network did.
docker build -t "$image" -f "$root/scripts/cross-windows.Dockerfile" "$root/scripts"

docker run --rm \
  -v "$root:/work" \
  -v "cellar-cargo-registry:/usr/local/cargo/registry" \
  -v "cellar-cross-target:/work/target-windows" \
  -w /work \
  -e CARGO_TARGET_DIR=/work/target-windows \
  "$image" \
  cargo build --release --target x86_64-pc-windows-gnu \
    -p cellar-cli -p cellar-fake-server

# The container writes as root; copy out and fix ownership so the host can read
# the artifacts without sudo.
docker run --rm \
  -v "$root:/work" \
  -v "cellar-cross-target:/work/target-windows" \
  -w /work \
  "$image" \
  bash -c "cp target-windows/x86_64-pc-windows-gnu/release/*.exe dist/windows/ && chown -R $(id -u):$(id -g) dist"

echo
echo "Built:"
ls -la "$out"
