#!/usr/bin/env sh
set -eu

config="${CELLAR_CONFIG:-/etc/cellar/cellar.toml}"

mkdir -p "$(dirname "$config")"
if [ ! -f "$config" ]; then
    cp /cellar.toml.example "$config"
    printf '%s\n' "cellar: created $config from the bundled example" >&2
fi

case "${1:-run}" in
    run)
        /cellar doctor --config "$config"
        exec /cellar run --config "$config"
        ;;
    doctor|config)
        exec /cellar --config "$config" "$@"
        ;;
    *)
        exec /cellar "$@"
        ;;
esac
