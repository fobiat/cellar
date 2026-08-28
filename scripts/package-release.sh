#!/usr/bin/env bash
# Package release artifacts from binaries that are already built.
#
# The same two artifacts per platform the release workflow produces, so a
# release cut by hand is indistinguishable from one cut by CI:
#
#   cellar-<target>.tar.gz / .zip   the archive, for the installer scripts
#   cellar-<target> / .exe          the bare binary, for `cellar self-update`
#
# plus a .sha256 beside each. Both installers and self-update refuse to install
# anything whose checksum was not published, so a missing one is a broken
# release rather than an inconvenience.
#
#   ./scripts/package-release.sh          package whatever is built
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

out="$root/dist/release"
rm -rf "$out"
mkdir -p "$out"

docs=(cellar.toml.example README.md CHANGELOG.md LICENSE-MIT)

# ------------------------------------------------------------------- linux

linux_bin="$root/target/release/cellar"
if [ -x "$linux_bin" ]; then
    staging="$(mktemp -d)"
    trap 'rm -rf "$staging"' EXIT

    cp "$linux_bin" "$staging/"
    [ -x "$root/target/release/cellar-fake-server" ] \
        && cp "$root/target/release/cellar-fake-server" "$staging/"
    cp "${docs[@]}" "$staging/"

    tar -czf "$out/cellar-x86_64-unknown-linux.tar.gz" -C "$staging" .
    cp "$linux_bin" "$out/cellar-x86_64-unknown-linux"
    echo "packaged linux"
else
    echo "no linux release binary; run: cargo build --release" >&2
fi

# ----------------------------------------------------------------- windows

windows_bin="$root/dist/windows/cellar.exe"
if [ -f "$windows_bin" ]; then
    staging="$(mktemp -d)"

    cp "$windows_bin" "$staging/"
    [ -f "$root/dist/windows/cellar-fake-server.exe" ] \
        && cp "$root/dist/windows/cellar-fake-server.exe" "$staging/"
    cp "${docs[@]}" "$staging/"

    # zip, not tar: Windows expands a zip with no extra software, and
    # Expand-Archive is what install.ps1 calls.
    ( cd "$staging" && zip -qr "$out/cellar-x86_64-pc-windows.zip" . )
    cp "$windows_bin" "$out/cellar-x86_64-pc-windows.exe"

    rm -rf "$staging"
    echo "packaged windows"
else
    echo "no windows binary; run: ./scripts/cross-build-windows.sh" >&2
fi

# --------------------------------------------------------------- checksums

cd "$out"
for file in *; do
    case "$file" in *.sha256) continue ;; esac
    sha256sum "$file" > "$file.sha256"
done

# Every artifact must have one, because the installers refuse without it.
missing=0
for file in *; do
    case "$file" in *.sha256) continue ;; esac
    [ -f "$file.sha256" ] || { echo "no checksum for $file" >&2; missing=1; }
done
[ "$missing" -eq 0 ] || exit 1

echo
ls -la
