#!/usr/bin/env bash
set -euo pipefail

gradle="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
resolver="$(cd "$(dirname "$0")" && pwd)/gradle-dependency-cache.init.gradle"
fixture=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/peppy-gradle-cache-test.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/repository/example/present/1" "$fixture/library"
printf 'not compiled\n' > "$fixture/repository/example/present/1/present-1.jar"
cat > "$fixture/repository/example/present/1/present-1.pom" <<'POM'
<project><modelVersion>4.0.0</modelVersion><groupId>example</groupId><artifactId>present</artifactId><version>1</version></project>
POM
cat > "$fixture/settings.gradle" <<'GRADLE'
rootProject.name = 'dependency-cache-fixture'
include 'library'
GRADLE
cat > "$fixture/build.gradle" <<'GRADLE'
plugins { id 'java' }
repositories { maven { url = uri('repository') } }
dependencies {
    implementation 'example:present:1'
    implementation project(':library')
}
tasks.named('compileJava') { doFirst { throw new GradleException('Compilation must not run') } }
GRADLE
printf "plugins { id 'java-library' }\n" > "$fixture/library/build.gradle"
"$gradle" -p "$fixture" --no-daemon --offline --console=plain --init-script "$resolver" resolveCiDependencies > "$fixture/success.log" 2>&1 || { cat "$fixture/success.log"; exit 1; }
grep -q 'Caching :library:runtimeClasspath' "$fixture/success.log"
if [[ -d "$fixture/build/classes" || -d "$fixture/library/build/libs" ]]; then
  echo 'dependency warming must not build project artifacts' >&2
  exit 1
fi
# Metadata resolves, but the artifact does not: graph-only reports cannot catch this.
mkdir -p "$fixture/repository/example/missing/1"
cat > "$fixture/repository/example/missing/1/missing-1.pom" <<'POM'
<project><modelVersion>4.0.0</modelVersion><groupId>example</groupId><artifactId>missing</artifactId><version>1</version></project>
POM
cat >> "$fixture/build.gradle" <<'GRADLE'
configurations { missingDependency { canBeResolved = true; canBeConsumed = false } }
dependencies { missingDependency 'example:missing:1' }
GRADLE
if "$gradle" -p "$fixture" --no-daemon --offline --console=plain --init-script "$resolver" resolveCiDependencies > "$fixture/failure.log" 2>&1; then
  echo 'unresolved dependency must fail the warming invocation' >&2
  exit 1
fi
grep -q 'missing-1.jar' "$fixture/failure.log"
grep -q 'BUILD FAILED' "$fixture/failure.log"
echo 'Gradle dependency cache fixture passed (success without compilation; missing dependency exits nonzero)'
