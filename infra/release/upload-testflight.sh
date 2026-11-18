#!/usr/bin/env bash
set +x
set -euo pipefail

if [[ $# != 3 || ! -f "${1:-}" ]]; then
  echo 'usage: upload-testflight.sh <pkg> <version> <build-number>' >&2
  exit 1
fi
pkg="$1" version="$2" build="$3"
expected="$(bash "$(dirname "$0")/macos-build-number.sh" "$version")"
[[ "$build" == "$expected" && "$build" != 0 ]] || { echo 'invalid build number' >&2; exit 1; }
for name in APPLE_ASC_KEY_ID APPLE_ASC_ISSUER_ID APPLE_ASC_PRIVATE_KEY RUNNER_TEMP; do
  [[ -n "${!name:-}" ]] || { echo "missing $name" >&2; exit 1; }
done
[[ "$APPLE_ASC_KEY_ID" =~ ^[A-Za-z0-9]+$ ]] || { echo 'invalid key ID' >&2; exit 1; }
umask 077
export API_PRIVATE_KEYS_DIR="$RUNNER_TEMP/private_keys"
mkdir -p "$API_PRIVATE_KEYS_DIR"
key="$API_PRIVATE_KEYS_DIR/AuthKey_${APPLE_ASC_KEY_ID}.p8"
[[ ! -e "$key" ]] || { echo 'key file already exists' >&2; exit 1; }
trap 'rm -f "$key"' EXIT
printf '%s\n' "$APPLE_ASC_PRIVATE_KEY" > "$key"
chmod 600 "$key"
deadline_seconds="${TESTFLIGHT_TEST_DEADLINE_SECONDS:-3600}"
[[ "$deadline_seconds" =~ ^[1-9][0-9]*$ ]] || { echo 'invalid deadline' >&2; exit 1; }
deadline=$((SECONDS + deadline_seconds))

query_build() {
  # Generate a short-lived token per request; curl reads the authorization header
  # from stdin rather than exposing the token in its process arguments.
  local token remaining timeout response status code
  response="$(mktemp "$RUNNER_TEMP/testflight-response.XXXXXX")"
  while (( SECONDS < deadline )); do
    token="$(python3 - "$key" "$APPLE_ASC_KEY_ID" "$APPLE_ASC_ISSUER_ID" <<'PY'
import base64, json, subprocess, sys, time
def enc(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=")
header = enc(json.dumps({"alg": "ES256", "kid": sys.argv[2], "typ": "JWT"}, separators=(",", ":")).encode())
payload = enc(json.dumps({"iss": sys.argv[3], "iat": int(time.time()), "exp": int(time.time()) + 600, "aud": "appstoreconnect-v1"}, separators=(",", ":")).encode())
message = header + b"." + payload
der = subprocess.run(["openssl", "dgst", "-sha256", "-sign", sys.argv[1]], input=message, stdout=subprocess.PIPE, check=True).stdout
if der[0] != 0x30 or der[2] != 0x02:
    raise ValueError("invalid ECDSA signature")
length = der[3]
r = int.from_bytes(der[4:4 + length], "big")
offset = 4 + length
if der[offset] != 0x02:
    raise ValueError("invalid ECDSA signature")
s = int.from_bytes(der[offset + 2:offset + 2 + der[offset + 1]], "big")
print((message + b"." + enc(r.to_bytes(32, "big") + s.to_bytes(32, "big"))).decode())
PY
)"
    remaining=$((deadline - SECONDS))
    (( remaining > 0 )) || break
    timeout=$remaining
    (( timeout <= 60 )) || timeout=60
    status=0
    code="$(printf 'header = "Authorization: Bearer %s"\n' "$token" | curl --config - --fail-with-body --silent --show-error --get --globoff \
      --connect-timeout 10 --max-time "$timeout" --output "$response" --write-out '%{http_code}' \
      --data-urlencode 'filter[app]=6835968609' --data-urlencode "filter[version]=$build" \
      --data-urlencode "filter[preReleaseVersion.version]=$version" \
      --data-urlencode 'fields[builds]=processingState' \
      'https://api.appstoreconnect.apple.com/v1/builds')" || status=$?
    if (( status == 0 )); then
      cat "$response"
      rm -f "$response"
      return 0
    fi
    if (( status == 22 )) && [[ ! "$code" =~ ^5[0-9][0-9]$ ]]; then
      echo "App Store Connect request failed (HTTP $code)" >&2
      rm -f "$response"
      return 1
    fi
    remaining=$((deadline - SECONDS))
    if (( remaining > 0 )); then
      (( remaining <= 5 )) || remaining=5
      sleep "$remaining"
    fi
  done
  rm -f "$response"
  echo 'Timed out waiting for App Store Connect' >&2
  return 1
}

state() {
  python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print(d[0]["attributes"]["processingState"] if d else "MISSING")'
}
response="$(query_build)"
current="$(printf '%s' "$response" | state)"
if [[ "$current" == MISSING ]]; then
  if xcrun --find altool >/dev/null 2>&1; then
    xcrun altool --upload-package "$pkg" --api-key "$APPLE_ASC_KEY_ID" --api-issuer "$APPLE_ASC_ISSUER_ID"
  else
    xcrun iTMSTransporter -m upload -assetFile "$pkg" -apiKey "$APPLE_ASC_KEY_ID" -apiIssuer "$APPLE_ASC_ISSUER_ID"
  fi
  deadline=$((SECONDS + deadline_seconds))
  current=MISSING
else
  echo "Build $version ($build) already exists: $current"
fi

while (( SECONDS < deadline )); do
  case "$current" in
    VALID) echo "Build $version ($build) is VALID"; exit 0 ;;
    INVALID|FAILED) echo "Build processing failed: $current" >&2; exit 1 ;;
  esac
  remaining=$((deadline - SECONDS))
  (( remaining <= 30 )) || remaining=30
  sleep "$remaining"
  (( SECONDS < deadline )) || break
  response="$(query_build)"
  current="$(printf '%s' "$response" | state)"
done
echo 'Timed out waiting for TestFlight processing' >&2
exit 1
