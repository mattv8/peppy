#!/bin/sh
set -eu

fail() {
    printf 'peppy-caddy: %s\n' "$*" >&2
    exit 1
}

validate_host() {
    case "$1" in
        ''|*[!A-Za-z0-9.-]*|.*|*..*|*.) fail "invalid hostname: $1" ;;
    esac
}

: "${PUBLIC_HOST:?set PUBLIC_HOST}"
validate_host "$PUBLIC_HOST"

if [ -n "${WEB_CLIENT_HOST:-}" ]; then
    printf 'peppy-caddy: WEB_CLIENT_HOST is retired; serve the UI at https://%s/ and remove the legacy hostname and DNS record.\n' "$PUBLIC_HOST" >&2
fi

config=$(mktemp)
trap 'rm -f "$config"' EXIT
sed "s|__PEPPY_HOSTS__|$PUBLIC_HOST|" /etc/caddy/Caddyfile.template >"$config"
if [ "${1:-}" = validate ]; then
    exec caddy validate --config "$config" --adapter caddyfile
fi
exec caddy run --config "$config" --adapter caddyfile
