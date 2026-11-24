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
if bash "$upload" >/dev/null 2>&1; then
  echo 'upload accepted missing arguments' >&2
  exit 1
fi
if bash "$upload" "$tmp/app.pkg" 1.2.3 1002003 >/dev/null 2>&1; then
  echo 'upload accepted missing credentials' >&2
  exit 1
fi
openssl ecparam -name prime256v1 -genkey -noout -out "$tmp/test.p8" 2>/dev/null
export APPLE_ASC_KEY_ID=TESTKEY APPLE_ASC_ISSUER_ID=test-issuer
APPLE_ASC_PRIVATE_KEY="$(cat "$tmp/test.p8")"
export APPLE_ASC_PRIVATE_KEY TEST_DIR="$tmp"
cat > "$tmp/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cat >/dev/null
output=''
connect=false
max_time=''
method=GET url='' data='' query=''
while (( $# )); do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    --connect-timeout) [[ "$2" == 10 ]]; connect=true; shift 2 ;;
    --max-time) max_time="$2"; shift 2 ;;
    --request) method="$2"; shift 2 ;;
    --data-binary) data="$2"; shift 2 ;;
    --data-urlencode) query="$query|$2"; shift 2 ;;
    https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
[[ "$connect" == true && "$max_time" -ge 1 && "$max_time" -le 60 ]]
exec 3>&1
exec > "$output"
[[ "$(stat -c %a "$API_PRIVATE_KEYS_DIR/AuthKey_TESTKEY.p8" 2>/dev/null || stat -f %Lp "$API_PRIVATE_KEYS_DIR/AuthKey_TESTKEY.p8")" == 600 ]]
if [[ "$url" == *betaGroups* || "$url" == *betaAppReviewSubmission* || "$query" == *'filter[betaGroups]'* ]]; then
  printf '%s %s %s\n' "$method" "$url" "$query" >> "$TEST_DIR/external-calls"
  case "$url:$method" in
    */betaGroups:GET)
      [[ "$query" == *'filter[app]=6835968609'* && "$query" == *'filter[name]=Beta seats'* && "$query" == *'filter[isInternalGroup]=false'* ]]
      case "${CASE:-}" in
        group-missing) echo '{"data":[]}' ;;
        group-duplicate) echo '{"data":[{"id":"group-1"},{"id":"group-2"}]}' ;;
        *) echo '{"data":[{"id":"group-1"}]}' ;;
      esac ;;
    */relationships/builds:POST)
      [[ "$url" == */betaGroups/group-1/relationships/builds ]]
      cp "${data#@}" "$TEST_DIR/group-payload"
      if [[ "${CASE:-}" == group-add-error ]]; then echo 400 >&3; exit 22; fi
      echo '{}' ;;
    */builds:GET)
      [[ "$query" == *'filter[id]=build-1'* && "$query" == *'filter[betaGroups]=group-1'* ]]
      if [[ "${CASE:-}" == group-existing ]]; then echo '{"data":[{"id":"build-1"}]}'; else echo '{"data":[]}'; fi ;;
    */betaAppReviewSubmission:GET)
      case "${CASE:-}" in
        review-404) echo 404 >&3; exit 22 ;;
        review-existing) echo '{"data":{"id":"review-1"}}' ;;
        *) echo '{"data":null}' ;;
      esac ;;
    */betaAppReviewSubmissions:POST)
      cp "${data#@}" "$TEST_DIR/review-payload"
      if [[ "${CASE:-}" == review-error ]]; then echo 409 >&3; exit 22; fi
      echo '{}' ;;
    *) echo 'unexpected external request' >&2; exit 1 ;;
  esac
  echo 200 >&3; exit
fi
if [[ "$url" == *betaBuildLocalizations* ]]; then
  if [[ "${CASE:-}" == processing ]]; then
    [[ "$(cat "$TEST_DIR/count")" -ge 2 ]] || { echo 'localization requested before VALID' >&2; exit 1; }
  fi
  printf '%s %s\n' "$method" "$url" >> "$TEST_DIR/localization-calls"
  if [[ "${CASE:-}" == localization-error ]]; then echo 400 >&3; exit 22; fi
  if [[ "$method" == GET ]]; then
    if [[ "${CASE:-}" == localization-existing || "${CASE:-}" == patch-error ]]; then
      echo '{"data":[{"id":"loc-1","attributes":{"locale":"en-US"}}],"links":{}}'
    else
      echo '{"data":[],"links":{}}'
    fi
  else
    [[ "$data" == @* ]]
    cp "${data#@}" "$TEST_DIR/localization-$(printf '%s' "$method" | tr 'A-Z' 'a-z')"
    case "${CASE:-}" in
      post-error) [[ "$method" == POST ]]; echo 400 >&3; exit 22 ;;
      patch-error) [[ "$method" == PATCH ]]; echo 503 >&3; exit 22 ;;
      write-transport) [[ "$method" == POST ]]; echo 000 >&3; exit 28 ;;
    esac
    echo '{"data":{"id":"loc-1","type":"betaBuildLocalizations"}}'
  fi
  echo 200 >&3; exit
fi
count=0
[[ ! -f "$TEST_DIR/count" ]] || count="$(cat "$TEST_DIR/count")"
count=$((count + 1))
echo "$count" > "$TEST_DIR/count"
case "${CASE:-}" in
  processing) if [[ "$count" == 1 ]]; then echo '{"data":[{"id":"build-1","attributes":{"processingState":"PROCESSING"}}]}'; else echo '{"data":[{"id":"build-1","attributes":{"processingState":"VALID"}}]}'; fi; echo 200 >&3; exit ;;
  failed) echo '{"data":[{"attributes":{"processingState":"FAILED"}}]}'; echo 200 >&3; exit ;;
  retry) if [[ "$count" == 1 ]]; then echo 000 >&3; exit 28; fi; echo '{"data":[{"id":"build-1","attributes":{"processingState":"VALID"}}]}'; echo 200 >&3; exit ;;
  server) if [[ "$count" == 1 ]]; then echo 503 >&3; exit 22; fi; echo '{"data":[{"id":"build-1","attributes":{"processingState":"VALID"}}]}'; echo 200 >&3; exit ;;
  deadline) exit 28 ;;
esac
if [[ "${EXISTING:-false}" == true || "$count" -gt 1 ]]; then
  printf '%s\n' '{"data":[{"id":"build-1","attributes":{"processingState":"VALID"}}]}'
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
[[ -f "$tmp/group-payload" && -f "$tmp/review-payload" ]] || { echo 'external distribution missing' >&2; exit 1; }
python3 - "$tmp/group-payload" "$tmp/review-payload" <<'PY'
import json, pathlib, sys
group, review = [json.loads(pathlib.Path(path).read_text()) for path in sys.argv[1:]]
build = {"type": "builds", "id": "build-1"}
assert group == {"data": [build]}
assert review == {"data": {"type": "betaAppReviewSubmissions", "relationships": {"build": {"data": build}}}}
PY
[[ -f "$tmp/upload-args" && ! -e "$tmp/runner/private_keys/AuthKey_TESTKEY.p8" ]]
[[ -f "$tmp/localization-post" ]] || { echo 'VALID build must create TestFlight localization' >&2; exit 1; }
python3 - "$tmp/localization-post" "$root/packaging/macos/app-store/what-to-test.txt" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())["data"]
assert data["type"] == "betaBuildLocalizations"
assert data["attributes"] == {"locale": "en-US", "whatsNew": pathlib.Path(sys.argv[2]).read_text()}
assert data["relationships"]["build"]["data"] == {"type": "builds", "id": "build-1"}
PY
rm "$tmp/upload-args" "$tmp/count" "$tmp/group-payload" "$tmp/review-payload"
EXISTING=true bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ ! -e "$tmp/upload-args" && ! -e "$tmp/runner/private_keys/AuthKey_TESTKEY.p8" ]]
[[ -e "$tmp/group-payload" && -e "$tmp/review-payload" ]] || { echo 're-run skipped external distribution' >&2; exit 1; }
for scenario in group-missing group-duplicate group-existing review-404 review-existing review-error group-add-error; do
  rm -f "$tmp/count" "$tmp/external-calls" "$tmp/group-payload" "$tmp/review-payload"
  if CASE="$scenario" EXISTING=true bash "$upload" "$tmp/app.pkg" 1.2.3 1002003 >"$tmp/result" 2>&1; then
    case "$scenario" in group-duplicate|group-add-error) echo "accepted $scenario" >&2; exit 1 ;; esac
  else
    case "$scenario" in group-duplicate|group-add-error) continue ;; *) cat "$tmp/result"; exit 1 ;; esac
  fi
  case "$scenario" in
    group-missing) grep -q '::warning::' "$tmp/result"; [[ ! -e "$tmp/group-payload" && ! -e "$tmp/review-payload" ]] ;;
    group-existing) [[ ! -e "$tmp/group-payload" && -e "$tmp/review-payload" ]] ;;
    review-404) [[ -e "$tmp/review-payload" ]] ;;
    review-existing) [[ ! -e "$tmp/review-payload" ]] ;;
    review-error) grep -q '::warning::' "$tmp/result"; [[ -e "$tmp/review-payload" ]] ;;
  esac
done
rm "$tmp/count"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/bin/sleep"
chmod +x "$tmp/bin/sleep"
CASE=processing bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
[[ ! -e "$tmp/upload-args" && "$(cat "$tmp/count")" -ge 2 ]]
rm "$tmp/count"
rm -f "$tmp/localization-calls"
if CASE=failed bash "$upload" "$tmp/app.pkg" 1.2.3 1002003; then
  echo 'accepted failed existing build' >&2; exit 1
fi
[[ ! -e "$tmp/localization-calls" ]] || { echo 'localized a failed build' >&2; exit 1; }
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
rm -f "$tmp/count" "$tmp/localization-patch"
CASE=localization-existing EXISTING=true bash "$upload" "$tmp/app.pkg" 1.2.3 1002003
python3 - "$tmp/localization-patch" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())["data"]
assert data["type"] == "betaBuildLocalizations" and data["id"] == "loc-1"
assert data["attributes"]["whatsNew"]
PY
rm -f "$tmp/count"
if CASE=localization-error EXISTING=true bash "$upload" "$tmp/app.pkg" 1.2.3 1002003; then
  echo 'accepted localization API error' >&2; exit 1
fi
for scenario in post-error patch-error write-transport; do
  rm -f "$tmp/count" "$tmp/localization-calls"
  if CASE="$scenario" EXISTING=true TESTFLIGHT_TEST_DEADLINE_SECONDS=3 bash "$upload" "$tmp/app.pkg" 1.2.3 1002003; then
    echo "accepted $scenario write failure" >&2; exit 1
  fi
  case "$scenario" in patch-error) method=PATCH ;; *) method=POST ;; esac
  expected_calls="$(printf 'GET https://api.appstoreconnect.apple.com/v1/builds/build-1/betaBuildLocalizations\n%s https://api.appstoreconnect.apple.com/v1/betaBuildLocalizations' "$method")"
  [[ "$method" != PATCH ]] || expected_calls="$expected_calls/loc-1"
  [[ "$(cat "$tmp/localization-calls")" == "$expected_calls" ]] || {
    echo "$scenario must make one localization GET and one $method attempt" >&2; exit 1
  }
  for payload in "$RUNNER_TEMP"/testflight-payload.*; do
    [[ ! -e "$payload" ]] || { echo "$scenario left a payload file" >&2; exit 1; }
  done
done
mkdir -p "$tmp/fixture/infra/release" "$tmp/fixture/packaging/macos/app-store"
cp "$upload" "$helper" "$tmp/fixture/infra/release/"
notes="$tmp/fixture/packaging/macos/app-store/what-to-test.txt"
python3 -c 'print("a" * 4001)' > "$notes"
if EXISTING=true bash "$tmp/fixture/infra/release/upload-testflight.sh" "$tmp/app.pkg" 1.2.3 1002003; then
  echo 'accepted oversized notes' >&2; exit 1
fi
: > "$notes"
if EXISTING=true bash "$tmp/fixture/infra/release/upload-testflight.sh" "$tmp/app.pkg" 1.2.3 1002003; then
  echo 'accepted empty notes' >&2; exit 1
fi
echo 'TestFlight validation tests passed'
