#!/usr/bin/env sh
# Install Cellar on Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.sh | sh
#   ./install.sh --system --service      # /usr/local/bin plus a systemd unit
#   ./install.sh --from-file cellar.tar.gz
#
# POSIX sh, not bash: a minimal container image is exactly where this runs and
# exactly where bash is missing.
set -eu

REPO="fobiat/cellar"
TARGET="x86_64-unknown-linux"
VERSION="latest"
SYSTEM=0
SERVICE=0
FROM_FILE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --system) SYSTEM=1; shift ;;
        --service) SERVICE=1; SYSTEM=1; shift ;;
        --from-file) FROM_FILE="$2"; shift 2 ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

blue() { printf '\033[38;2;47;143;224m%s\033[0m\n' "$1"; }
grey() { printf '\033[38;2;122;125;129m%s\033[0m\n' "$1"; }
green() { printf '\033[38;2;111;168;98m%s\033[0m\n' "$1"; }
die() { printf '\033[38;2;218;91;77merror:\033[0m %s\n' "$1" >&2; exit 1; }

printf '\n'
blue '  * CELLAR'
grey '    a dedicated server manager for s&box'
printf '\n'

# ------------------------------------------------------------------ where

if [ "$SYSTEM" -eq 1 ]; then
    [ "$(id -u)" -eq 0 ] || die "--system needs root. Re-run with sudo, or drop it for a per-user install."
    INSTALL_DIR="/usr/local/bin"
    CONFIG_DIR="/etc/cellar"
else
    INSTALL_DIR="${HOME}/.local/bin"
    CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/cellar"
fi

mkdir -p "$INSTALL_DIR" "$CONFIG_DIR"

# Architecture, because an aarch64 box downloading an x86_64 binary produces a
# confusing "not found" from the kernel rather than anything useful.
case "$(uname -m)" in
    x86_64|amd64) TARGET="x86_64-unknown-linux" ;;
    aarch64|arm64) TARGET="aarch64-unknown-linux" ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

TEMP="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$TEMP'" EXIT INT TERM

# ------------------------------------------------------------------ fetch

if [ -n "$FROM_FILE" ]; then
    grey "  Using $FROM_FILE"
    TARBALL="$FROM_FILE"
    EXPECTED=""
else
    command -v curl >/dev/null 2>&1 || die "curl is needed"

    if [ "$VERSION" = "latest" ]; then
        API="https://api.github.com/repos/${REPO}/releases/latest"
    else
        API="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}"
    fi

    grey "  Looking up the release"
    RELEASE="$(curl -fsSL -H 'User-Agent: cellar-installer' "$API")" \
        || die "could not reach the release API"

    URL="$(printf '%s' "$RELEASE" \
        | grep -o "https://[^\"]*${TARGET}[^\"]*\.tar\.gz" | head -n1)"
    [ -n "$URL" ] || die "no ${TARGET} asset in that release"

    grey "  Downloading $(basename "$URL")"
    TARBALL="${TEMP}/cellar.tar.gz"
    curl -fsSL -o "$TARBALL" "$URL"

    # Refuse to install anything whose checksum was not published.
    EXPECTED="$(curl -fsSL "${URL}.sha256" 2>/dev/null | awk '{print $1}')" \
        || die "no published checksum; refusing to install an unverified binary"
    [ -n "$EXPECTED" ] || die "no published checksum; refusing to install an unverified binary"
fi

if [ -n "$EXPECTED" ]; then
    grey "  Verifying the checksum"
    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "$TARBALL" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
    else
        die "no sha256sum or shasum; cannot verify the download"
    fi

    [ "$ACTUAL" = "$EXPECTED" ] || die "checksum mismatch: expected $EXPECTED, got $ACTUAL"
    green "  Checksum matches"
fi

# ------------------------------------------------------------------ install

grey "  Installing to ${INSTALL_DIR}"
tar -xzf "$TARBALL" -C "$TEMP"

# Installed with `install`, not `cp`: it replaces the file atomically, so a
# running Cellar keeps its own inode and does not get a half-written binary.
for binary in cellar cellar-fake-server; do
    [ -f "${TEMP}/${binary}" ] && install -m 0755 "${TEMP}/${binary}" "${INSTALL_DIR}/${binary}"
done

# ------------------------------------------------------------------- config

CONFIG="${CONFIG_DIR}/cellar.toml"
if [ ! -f "$CONFIG" ]; then
    if [ -f "${TEMP}/cellar.toml.example" ]; then
        cp "${TEMP}/cellar.toml.example" "$CONFIG"
        green "  Wrote a starting config to ${CONFIG}"
    fi
else
    grey "  Left your existing config at ${CONFIG}"
fi

# ------------------------------------------------------------------ service

if [ "$SERVICE" -eq 1 ]; then
    grey "  Writing the systemd unit"

    cat > /etc/systemd/system/cellar.service <<UNIT
[Unit]
Description=Cellar, an s&box dedicated server manager
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
ExecStart=${INSTALL_DIR}/cellar run --config ${CONFIG}

# The engine installs no SIGTERM handler, so systemd's default TERM would kill
# it outright and skip the Steam logoff and the convar save. Cellar catches the
# signal and sends \`quit\` through the console instead, which needs time.
KillSignal=SIGTERM
TimeoutStopSec=60

Restart=on-failure
RestartSec=5

# Secrets belong in a root-only file, not in this unit.
EnvironmentFile=-/etc/cellar/cellar.env

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
UNIT

    if [ ! -f /etc/cellar/cellar.env ]; then
        cat > /etc/cellar/cellar.env <<'ENV'
# Secrets for Cellar. Read by systemd, never by git.
# CELLAR_DATABASE_URL=mysql://cellar:password@127.0.0.1/cellar
# CELLAR_GSLT=
# CELLAR_WEB_PASSWORD_HASH=
# CELLAR_DISCORD_WEBHOOK_URL=
ENV
        chmod 0600 /etc/cellar/cellar.env
    fi

    systemctl daemon-reload
    green "  Service installed. Start it with: systemctl enable --now cellar"
    grey  "  Put secrets in /etc/cellar/cellar.env first."
fi

# --------------------------------------------------------------------- done

printf '\n'
green "  Installed $("${INSTALL_DIR}/cellar" --version)"
printf '\n'
grey "    Config:  ${CONFIG}"
grey "    Binary:  ${INSTALL_DIR}/cellar"
printf '\n'
printf '  Next:\n'
grey "    1. edit ${CONFIG}"
grey "    2. cellar doctor"
grey "    3. cellar run"
printf '\n'

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) grey "  ${INSTALL_DIR} is not on your PATH. Add it to your shell profile."; printf '\n' ;;
esac
