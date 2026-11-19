#!/usr/bin/env bash

set -euo pipefail
if [[ $# != 2 ]] || [[ "$1" != debugRuntimeClasspath && "$1" != sideloadRuntimeClasspath ]]; then
  echo "Usage: $0 {debugRuntimeClasspath|sideloadRuntimeClasspath} captured-log" >&2
  exit 2
fi
configuration="$1"
captured_log="$2"
source_home="${GRADLE_USER_HOME:-$HOME/.gradle}"
diagnostic_dir="$(mktemp -d "$RUNNER_TEMP/sideload-resolution.XXXXXX")"
fresh_home="$diagnostic_dir/home"
mkdir -p "$fresh_home" "$diagnostic_dir/project-cache"
if [[ -d "$source_home/wrapper/dists" ]]; then
  mkdir -p "$fresh_home/wrapper"
  ln -s "$source_home/wrapper/dists" "$fresh_home/wrapper/dists"
fi
if [[ -f "$source_home/gradle.properties" ]]; then
  echo "Original Gradle user-home gradle.properties: present"
else
  echo "Original Gradle user-home gradle.properties: absent"
fi
shopt -s nullglob
init_scripts=("$source_home/init.gradle" "$source_home/init.gradle.kts" "$source_home"/init.d/*.gradle "$source_home"/init.d/*.gradle.kts)
init_present=false
for script in "${init_scripts[@]}"; do
  [[ ! -f "$script" ]] || init_present=true
done
echo "Original Gradle user-home init scripts present: $init_present"

urls="$diagnostic_dir/urls"
perl -ne 'while (m{(https://repo[.]maven[.]apache[.]org/maven2/[A-Za-z0-9_./+-]+[.]pom)(?![A-Za-z0-9_.?%#/-])}g) { print "$1\n" }' "$captured_log" | awk '!seen[$0]++ && ++count <= 4' > "$urls"
index=0
while IFS= read -r url; do
  index=$((index + 1))
  body="$diagnostic_dir/pom-$index"
  status="$(curl --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 5 --max-time 15 --output "$body" --write-out '%{http_code}' "$url" 2>/dev/null)" || status="000"
  echo "Central POM: $url status=$status"
  if [[ "$status" == 200 ]]; then
    echo "SHA256: $(sha256sum "$body" | cut -d ' ' -f 1)"
  fi
done < "$urls"

for label in original fresh; do
  graph="$diagnostic_dir/$label-graph.log"
  if [[ "$label" == original ]]; then
    graph_home="$source_home"
    cache_args=()
  else
    graph_home="$fresh_home"
    cache_args=(--project-cache-dir "$diagnostic_dir/project-cache")
  fi
  status=0
  GRADLE_USER_HOME="$graph_home" ./gradlew --no-daemon --console=plain --dependency-verification=strict --info ${cache_args[@]+"${cache_args[@]}"} :app:dependencies --configuration "$configuration" > "$graph" 2>&1 || status=$?
  cat "$graph"
  failed_nodes="$(grep -cE -- '--- .* FAILED([[:space:]]|$)' "$graph" || true)"
  echo "$label graph: status=$status FAILED nodes=$failed_nodes"
done
