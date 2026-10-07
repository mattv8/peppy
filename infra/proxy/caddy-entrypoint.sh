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

hosts=$PUBLIC_HOST
if [ -n "${WEB_CLIENT_HOST:-}" ]; then
    validate_host "$WEB_CLIENT_HOST"
    public_host=$(printf '%s' "$PUBLIC_HOST" | tr '[:upper:]' '[:lower:]')
    web_client_host=$(printf '%s' "$WEB_CLIENT_HOST" | tr '[:upper:]' '[:lower:]')
    if [ "$web_client_host" != "$public_host" ]; then
        hosts="$hosts, $WEB_CLIENT_HOST"
    fi
fi

config=$(mktemp)
trap 'rm -f "$config"' EXIT
sed "s|__PEPPY_HOSTS__|$hosts|" /etc/caddy/Caddyfile.template >"$config"
if [ "${1:-}" = validate ]; then
    exec caddy validate --config "$config" --adapter caddyfile
fi
exec caddy run --config "$config" --adapter caddyfile
