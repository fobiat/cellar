#!/usr/bin/env sh
# Small Linux tray launcher for Cellar. `yad` is optional; the same script can
# open the web UI or TUI directly when called from a desktop shortcut.
set -eu

cellar_bin=${CELLAR_BIN:-"$HOME/.local/bin/cellar"}
config_file=${CELLAR_CONFIG:-"${XDG_CONFIG_HOME:-$HOME/.config}/cellar/cellar.toml"}
web_url=${CELLAR_WEB_URL:-http://127.0.0.1:8081}
script_path=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/$(basename -- "$0")
icon_path=${CELLAR_ICON:-"$(dirname -- "$script_path")/cellar-icon.svg"}

headers() {
    if [ -n "${CELLAR_SESSION:-}" ]; then
        printf '%s\n' "Cookie: cellar_session=${CELLAR_SESSION}"
    fi
}

server_state() {
    if curl -fsS --max-time 2 "${web_url%/}/readyz" >/dev/null 2>&1; then
        printf '%s\n' ready
    elif curl -fsS --max-time 2 "${web_url%/}/healthz" >/dev/null 2>&1; then
        printf '%s\n' starting
    else
        printf '%s\n' offline
    fi
}

control() {
    action=$1
    if [ -n "${CELLAR_SESSION:-}" ]; then
        curl -fsS --max-time 5 -X POST -H "Cookie: cellar_session=${CELLAR_SESSION}" \
            "${web_url%/}/api/control/${action}" >/dev/null
    else
        curl -fsS --max-time 5 -X POST "${web_url%/}/api/control/${action}" >/dev/null
    fi
}

open_web() {
    command -v xdg-open >/dev/null 2>&1 || { echo 'xdg-open is required' >&2; exit 1; }
    xdg-open "$web_url" >/dev/null 2>&1 &
}

open_tui() {
    [ -x "$cellar_bin" ] || { echo "Cellar executable not found at $cellar_bin" >&2; exit 1; }
    if command -v x-terminal-emulator >/dev/null 2>&1; then
        x-terminal-emulator -e "$cellar_bin" --config "$config_file" tui --url "$web_url" &
    elif command -v gnome-terminal >/dev/null 2>&1; then
        gnome-terminal -- "$cellar_bin" --config "$config_file" tui --url "$web_url" &
    elif command -v konsole >/dev/null 2>&1; then
        konsole -e "$cellar_bin" --config "$config_file" tui --url "$web_url" &
    elif command -v xfce4-terminal >/dev/null 2>&1; then
        xfce4-terminal --command "$cellar_bin --config '$config_file' tui --url '$web_url'" &
    else
        echo 'No supported terminal emulator found' >&2
        exit 1
    fi
}

start_cellar() {
    if [ "$(server_state)" != offline ]; then
        exit 0
    fi
    [ -x "$cellar_bin" ] || { echo "Cellar executable not found at $cellar_bin" >&2; exit 1; }
    nohup "$cellar_bin" --config "$config_file" run >/dev/null 2>&1 &
}

tray() {
    command -v yad >/dev/null 2>&1 || {
        echo 'The Linux tray needs yad. Install it with your distribution package manager.' >&2
        exit 1
    }
    if [ -f "$icon_path" ]; then
        tray_image=$icon_path
    else
        tray_image=utilities-system-monitor
    fi
    yad --notification --image="$tray_image" --text="Cellar: $(server_state)" \
        --menu="Open web UI!$script_path --open-web|Open TUI!$script_path --open-tui|Start Cellar!$script_path --start|Restart server!$script_path --control restart|Stop server!$script_path --control stop|Exit Cellar!$script_path --control exit|Exit tray!quit" \
        --no-middle
}

action=${1:-tray}
case "$action" in
    --status) server_state ;;
    --open-web) open_web ;;
    --open-tui) open_tui ;;
    --start) start_cellar ;;
    --control) [ $# -eq 2 ] || exit 2; control "$2" ;;
    tray) tray ;;
    *) echo "usage: $0 [--status|--open-web|--open-tui|--start|--control ACTION]" >&2; exit 2 ;;
esac
