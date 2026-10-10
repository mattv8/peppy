#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
DOTENV=${1:-"$ROOT/.env"}

python3 "$ROOT/infra/dev/dev_port.py" validate "$DOTENV"
base_url=$(python3 "$ROOT/infra/dev/dev_port.py" smoke-url "$DOTENV")
curl_args=(--fail --silent --show-error)
tls_enabled=$(python3 "$ROOT/infra/dev/dev_tls.py" enabled "$DOTENV")
if [[ $tls_enabled == true ]]; then
    curl_args+=(--cacert "$(python3 "$ROOT/infra/dev/dev_tls.py" ca-path)")
fi

if [[ $(python3 "$ROOT/infra/dev/dev_port.py" web-enabled "$DOTENV") == true ]]; then
    api_origin=$(python3 "$ROOT/infra/dev/dev_port.py" public-api-origin "$DOTENV")
    index=$(mktemp)
    config=$(mktemp)
    trap 'rm -f "$index" "$config"' EXIT
    curl "${curl_args[@]}" "$base_url/" > "$index"
    asset=$(python3 -c '
import re
import sys

match = re.search(r"(?:src|href)=\"(/[^\"]+\.(?:js|css))", open(sys.argv[1]).read())
assert match, "web index has no referenced asset"
print(match.group(1))
' "$index")
    curl "${curl_args[@]}" "$base_url$asset" >/dev/null
    curl "${curl_args[@]}" "$base_url/web/config.json" > "$config"
    advertised_origin=$(python3 - "$config" <<'PY'
import json
import sys

with open(sys.argv[1]) as config:
    actual = json.load(config)["apiOrigin"]
if not isinstance(actual, str):
    raise SystemExit("web config apiOrigin must be a string")
print(actual)
PY
)
    advertised_origin=$(python3 "$ROOT/infra/dev/dev_port.py" normalize-origin "$advertised_origin")
    if [[ $advertised_origin != "$api_origin" ]]; then
        echo "web config apiOrigin is '$advertised_origin'; expected '$api_origin'" >&2
        exit 1
    fi
fi

curl "${curl_args[@]}" "$base_url/healthz" >/dev/null
curl "${curl_args[@]}" "$base_url/readyz" >/dev/null
