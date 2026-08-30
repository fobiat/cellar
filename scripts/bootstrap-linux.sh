#!/usr/bin/env sh
# Bring a Linux host from nothing to a working Cellar.
#
#   ./scripts/bootstrap-linux.sh                  # prerequisites, then Cellar
#   ./scripts/bootstrap-linux.sh --check          # report only, change nothing
#   ./scripts/bootstrap-linux.sh --from-source    # build the CLI here
#   ./scripts/bootstrap-linux.sh --with-steamcmd --with-dotnet --with-mariadb
#   ./scripts/bootstrap-linux.sh --all            # every optional component
#
# `install.sh` installs Cellar and nothing else, which is right for a container
# image that already carries Wine. This is for a bare host, where the things
# Cellar supervises are missing too.
#
# POSIX sh, matching install.sh, and it never assumes root: each component is
# probed first and only what is genuinely missing escalates. A component needing
# a package manager it cannot reach prints the exact command and the run carries
# on, so a non-interactive run still does everything it can.
set -eu

VERSION="latest"
FROM_SOURCE=0
CHECK_ONLY=0
WANT_STEAMCMD=0
WANT_DOTNET=0
WANT_MARIADB=0
DOTNET_VERSION="10.0"
FAILED=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --from-source) FROM_SOURCE=1; shift ;;
        --check) CHECK_ONLY=1; shift ;;
        --with-steamcmd) WANT_STEAMCMD=1; shift ;;
        --with-dotnet) WANT_DOTNET=1; shift ;;
        --with-mariadb) WANT_MARIADB=1; shift ;;
        --dotnet-version) DOTNET_VERSION="$2"; shift 2 ;;
        --all) WANT_STEAMCMD=1; WANT_DOTNET=1; WANT_MARIADB=1; shift ;;
        -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

blue() { printf '\033[38;2;47;143;224m%s\033[0m\n' "$1"; }
grey() { printf '\033[38;2;122;125;129m%s\033[0m\n' "$1"; }
green() { printf '\033[38;2;111;168;98m%s\033[0m\n' "$1"; }
amber() { printf '\033[38;2;208;158;79m%s\033[0m\n' "$1"; }
die() { printf '\033[38;2;218;91;77merror:\033[0m %s\n' "$1" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

TEMP_DIR="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$TEMP_DIR'" EXIT INT TERM

# A missing optional component is a warning, not an exit: the caller asked for a
# host set up as far as this box allows, and a later component may still work.
warn() {
    amber "  !     $1"
    FAILED=$((FAILED + 1))
}

printf '\n'
blue '  * CELLAR'
grey '    bootstrapping a Linux host'
printf '\n'

case "$(uname -s)" in
    Linux) ;;
    *) die "this script is for Linux. macOS and Windows have their own paths." ;;
esac

case "$(uname -m)" in
    x86_64|amd64) ;;
    *) die "the s&box dedicated server is x86_64-only. This host is $(uname -m)." ;;
esac

# ------------------------------------------------------------ package manager

# Resolved once. Each entry is the install verb, because that is the only part
# that differs between them for this script's purposes.
PM=""
PM_INSTALL=""
if have pacman; then
    PM="pacman"
    PM_INSTALL="pacman -S --needed --noconfirm"
elif have apt-get; then
    PM="apt"
    PM_INSTALL="apt-get install -y --no-install-recommends"
elif have dnf; then
    PM="dnf"
    PM_INSTALL="dnf install -y"
elif have zypper; then
    PM="zypper"
    PM_INSTALL="zypper install -y"
fi

SUDO=""
if [ "$(id -u)" -ne 0 ] && have sudo; then
    SUDO="sudo"
fi

# Package names differ per distro for exactly the packages this installs.
pkg_for() {
    component="$1"
    case "${PM}:${component}" in
        pacman:wine) echo "wine" ;;
        apt:wine) echo "wine64" ;;
        dnf:wine|zypper:wine) echo "wine" ;;

        pacman:mariadb) echo "mariadb" ;;
        apt:mariadb) echo "mariadb-server" ;;
        dnf:mariadb|zypper:mariadb) echo "mariadb-server" ;;

        *:curl) echo "curl" ;;
        *:rsync) echo "rsync" ;;
        *:tar) echo "tar" ;;
        *) echo "$component" ;;
    esac
}

install_pkg() {
    name="$(pkg_for "$1")"

    if [ -z "$PM" ]; then
        warn "no supported package manager found. Install '$name' yourself."
        return 1
    fi

    if [ "$(id -u)" -ne 0 ] && [ -z "$SUDO" ]; then
        warn "need root to install '$name'. Run: $PM_INSTALL $name"
        return 1
    fi

    grey "  Installing $name with $PM"
    # shellcheck disable=SC2086
    if $SUDO $PM_INSTALL "$name" >/dev/null 2>&1; then
        return 0
    fi

    warn "'$name' failed to install. Run: $SUDO $PM_INSTALL $name"
    return 1
}

# `want <command> <component>`: install only when the command is absent.
want() {
    if have "$1"; then
        green "  ok    $1"
        return 0
    fi

    if [ "$CHECK_ONLY" -eq 1 ]; then
        warn "$1 is missing"
        return 1
    fi

    install_pkg "$2" || return 1
    have "$1" && green "  ok    $1"
}

# ----------------------------------------------------------------- core tools

grey "  Core tools"
want curl curl || true
want tar tar || true

# rsync is not Cellar's own dependency. It is how a gamemode checkout reaches
# the external runtime copy the server actually loads, which is the documented
# development loop on every platform.
want rsync rsync || true

# Wine is not optional on Linux: Facepunch ships no Linux dedicated server, so
# `launcher = "wine"` is the only launcher a real deployment uses here.
printf '\n'
grey "  The server's runtime"
want wine wine || true

if have wine; then
    grey "        $(wine --version 2>/dev/null || echo 'version unknown')"
fi

# ------------------------------------------------------------------- steamcmd

if [ "$WANT_STEAMCMD" -eq 1 ]; then
    printf '\n'
    grey "  steamcmd, which is how sbox-server.exe gets onto this host"

    STEAM_DIR="${HOME}/.local/share/steamcmd"
    if have steamcmd; then
        green "  ok    steamcmd"
    elif [ -x "${STEAM_DIR}/steamcmd.sh" ]; then
        green "  ok    steamcmd (${STEAM_DIR})"
    elif [ "$CHECK_ONLY" -eq 1 ]; then
        warn "steamcmd is missing"
    else
        # Valve's tarball rather than a package: steamcmd sits in the AUR on
        # Arch and in non-free on Debian, so the package is the less portable
        # of the two and needs root besides.
        grey "  Fetching steamcmd from Valve"
        mkdir -p "$STEAM_DIR"
        if curl -fsSL "https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz" \
             | tar -xz -C "$STEAM_DIR" 2>/dev/null; then
            green "  ok    steamcmd (${STEAM_DIR}/steamcmd.sh)"
        else
            warn "steamcmd download failed. Install it from your distro instead."
        fi
    fi
fi

# --------------------------------------------------------- Windows .NET, Wine

if [ "$WANT_DOTNET" -eq 1 ]; then
    printf '\n'
    grey "  The Windows .NET runtime, inside the Wine prefix"

    WINEPREFIX_DIR="${WINEPREFIX:-$HOME/.wine}"
    if [ "$CHECK_ONLY" -eq 1 ]; then
        if [ -d "$WINEPREFIX_DIR" ]; then
            green "  ok    wine prefix ${WINEPREFIX_DIR}"
        else
            warn "no wine prefix at ${WINEPREFIX_DIR}"
        fi
    elif ! have wine; then
        warn "Wine is missing, so its prefix cannot be prepared."
    else
        # Microsoft's channel installer, not winetricks. sbox-server.dll asks
        # for Microsoft.NETCore.App 10.0.0 and winetricks' newest verb is
        # dotnet9, so the verb route installs a runtime the server then refuses
        # to start on. It needs no root either, unlike installing winetricks.
        DOTNET_URL="https://aka.ms/dotnet/${DOTNET_VERSION}/dotnet-runtime-win-x64.exe"
        DOTNET_EXE="${TEMP_DIR}/dotnet-runtime-win-x64.exe"

        grey "  Fetching the .NET ${DOTNET_VERSION} runtime for Windows"
        if curl -fsSL -o "$DOTNET_EXE" "$DOTNET_URL"; then
            grey "  Installing it into ${WINEPREFIX_DIR}, silent and slow"
            if WINEPREFIX="$WINEPREFIX_DIR" WINEDEBUG=-all \
               wine "$DOTNET_EXE" /install /quiet /norestart >/dev/null 2>&1; then
                green "  ok    .NET ${DOTNET_VERSION} runtime"
            else
                warn "the runtime installer failed. Run it by hand:
        WINEPREFIX=$WINEPREFIX_DIR wine <installer> /install /quiet /norestart"
            fi
        else
            warn "could not download ${DOTNET_URL}"
        fi
    fi
fi

# -------------------------------------------------------------------- MariaDB

if [ "$WANT_MARIADB" -eq 1 ]; then
    printf '\n'
    grey "  MariaDB, for [database] and the web UI's history"
    want mariadbd mariadb || true

    if have mariadbd && [ "$CHECK_ONLY" -eq 0 ]; then
        grey "        Cellar can also host its own: set [mariadb].managed = true"
    fi
fi

# --------------------------------------------------------------------- Cellar

printf '\n'
grey "  Cellar itself"

HERE="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

if [ "$CHECK_ONLY" -eq 1 ]; then
    if have cellar; then
        green "  ok    $(cellar --version)"
    else
        warn "cellar is not installed"
    fi
elif [ "$FROM_SOURCE" -eq 1 ]; then
    have cargo || die "no cargo. Install a Rust toolchain, or drop --from-source."

    # The published asset tracks the last tag, so a checkout ahead of it builds
    # something the release does not carry yet. That is the whole reason this
    # flag exists.
    grey "  Building from source, which takes a few minutes"
    ( cd "${HERE}/.." && cargo build --release -p cellar-cli -p cellar-fake-server )

    INSTALL_DIR="${HOME}/.local/bin"
    mkdir -p "$INSTALL_DIR"
    for binary in cellar cellar-fake-server; do
        [ -f "${HERE}/../target/release/${binary}" ] \
            && install -m 0755 "${HERE}/../target/release/${binary}" "${INSTALL_DIR}/${binary}"
    done
    green "  ok    installed to ${INSTALL_DIR}"
else
    [ -f "${HERE}/install.sh" ] || die "install.sh is missing from ${HERE}"
    sh "${HERE}/install.sh" --version "$VERSION"
fi

# ---------------------------------------------------------------------- close

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/cellar"

printf '\n'
if [ "$FAILED" -gt 0 ]; then
    amber "  $FAILED component(s) above need attention."
else
    green "  Every requested component is present."
fi

if [ "$CHECK_ONLY" -eq 1 ]; then
    printf '\n'
    exit 0
fi

case ":${PATH}:" in
    *":${HOME}/.local/bin:"*) ;;
    *) printf '\n'; amber "  ~/.local/bin is not on PATH. Add it to your shell profile." ;;
esac

printf '\n'
printf '  Next:\n'
grey "    1. edit ${CONFIG_DIR}/cellar.toml"
grey "    2. cellar doctor --config ${CONFIG_DIR}/cellar.toml"
grey "    3. cellar run --config ${CONFIG_DIR}/cellar.toml"
printf '\n'
