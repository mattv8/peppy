#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
helper="$root/infra/release/macos-build-number.sh"
upload="$root/infra/release/upload-testflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/runner"
printf '#!/usr/bin/env bash\nexit 99\n' > "$tmp/bin/curl"
printf '#!/usr/bin/env bash\nexit 99\n' > "$tmp/bin/xcrun"
chmod +x "$tmp/bin/curl" "$tmp/bin/xcrun"
export PATH="$tmp/bin:$PATH" RUNNER_TEMP="$tmp/runner"

number="$(bash "$helper" 1.2.3)"
[[ "$number" == 1002003 ]]
number="$(bash "$helper" 999.999.999)"
[[ "$number" == 999999999 ]]
for version in 1000.0.0 1.1000.0 1.2.1000 1.2.3-beta 01.2.3; do
  if bash "$helper" "$version" >/dev/null 2>&1; then
    echo "accepted invalid version: $version" >&2
    exit 1
  fi
done
touch "$tmp/app.pkg"
for args in '' "$tmp/app.pkg 1.2.3 1002003"; do
  # Deliberate word splitting supplies the tested argument list.
  if bash "$upload" $args >/dev/null 2>&1; then
    echo 'upload accepted missing arguments or credentials' >&2
    exit 1
  fi
done
openssl ecparam -name prime256v1 -genkey -noout -out "$tmp/test.p8" 2>/dev/null
export APPLE_ASC_KEY_ID=TESTKEY APPLE_ASC_ISSUER_ID=test-issuer
export APPLE_ASC_PRIVATE_KEY="$(cat "$tmp/test.p8")" TEST_DIR="$tmp"
cat > "$tmp/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cat >/dev/null
output=''
connect=false
max_time=''
while (( $# )); do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    --connect-timeout) [[ "$2" == 10 ]]; connect=true; shift 2 ;;
    --max-time) max_time="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[[ "$connect" == true && "$max_time" -ge 1 && "$max_time" -le 60 ]]
exec 3>&1
exec > "$output"
[[ "$(stat -f %Lp "$API_PRIVATE_KEYS_DIR/AuthKey_TESTKEY.p8" 2>/dev/null || stat -c %a "$API_PRIVATE_KEYS_DIR/AuthKey_TESTKEY.p8")" == 600 ]]
count=0
[[ ! -f "$TEST_DIR/count" ]] || count="$(cat "$TEST_DIR/count")"
count=$((count + 1))
echo "$count" > "$TEST_DIR/count"
case "${CASE:-}" in
  processing) if [[ "$count" == 1 ]]; then echo '{"data":[{"attributes":{"processingState":"PROCESSING"}}]}'; else echo '{"data":[{"attributes":{"processingState":"VALID"}}]}'; fi; echo 200 >&3; exit ;;
  failed) echo '{"data":[{"attributes":{"processingState":"FAILED"}}]}'; echo 200 >&3; exit ;;
  retry) if [[ "$count" == 1 ]]; then echo 000 >&3; exit 28; fi; echo '{"data":[{"attributes":{"processingState":"VALID"}}]}'; echo 200 >&3; exit ;;
  server) if [[ "$count" == 1 ]]; then echo 503 >&3; exit 22; fi; echo '{"data":[{"attributes":{"processingState":"VALID"}}]}'; echo 200 >&3; exit ;;
  deadline) exit 28 ;;
esac
if [[ "${EXISTING:-false}" == true || "$count" -gt 1 ]]; then
  printf '%s\n' '{"data":[{"attributes":{"processingState":"VALID"}}]}'
else
  printf '%s\n' '{"data":[]}'
fi
echo 200 >&3
EOF
cat > "$tmp/bin/xcrun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == --find ]]; then exit 0; fi
printf '%s\n' "$@" > "$TEST_DIR/upload-args"
EOF
chmod +x "$tmp/bin/curl" "$tmp/bin/xcrun"
bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ -f "$tmp/upload-args" && ! -e "$tmp/runner/private_keys/AuthKey_TESTKEY.p8" ]]
rm "$tmp/upload-args" "$tmp/count"
EXISTING=true bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ ! -e "$tmp/upload-args" && ! -e "$tmp/runner/private_keys/AuthKey_TESTKEY.p8" ]]
rm "$tmp/count"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/bin/sleep"
chmod +x "$tmp/bin/sleep"
CASE=processing bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ ! -e "$tmp/upload-args" && "$(cat "$tmp/count")" -ge 2 ]]
rm "$tmp/count"
if CASE=failed bash "$upload" "$tmp/app.pkg" 1.2.3 1002003; then
  echo 'accepted failed existing build' >&2; exit 1
fi
rm "$tmp/count"
CASE=retry bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ "$(cat "$tmp/count")" -ge 2 ]]
rm "$tmp/count"
CASE=server bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ "$(cat "$tmp/count")" -ge 2 ]]
rm "$tmp/count"
if CASE=deadline TESTFLIGHT_TEST_DEADLINE_SECONDS=1 bash "$upload" "$tmp/app.pkg" 1.2.3 1002003; then
  echo 'accepted expired deadline' >&2; exit 1
fi
echo 'TestFlight validation tests passed'
