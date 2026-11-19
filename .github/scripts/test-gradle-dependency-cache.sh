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
// These are not inputs of any CI/release task and must never be resolved by warming.
configurations {
    create('unified-test-platform-gradle-work-action') { canBeResolved = true; canBeConsumed = false }
    create('unusedIdeMetadata') { canBeResolved = true; canBeConsumed = false }
    ['debugAndroidTestCompileClasspath', 'debugUnitTestRuntimeClasspath',
     'sideloadUnitTestCompileClasspath', 'releaseUnitTestRuntimeClasspath', 'releaseRuntimeClasspath', 'androidLintTool',
     'kotlinCompilerPluginClasspathSideloadUnitTest', 'robolectricDependency0'].each { name ->
        create(name) { canBeResolved = true; canBeConsumed = false }
        dependencies.add(name, 'example:present:1')
    }
}
dependencies {
    add('unified-test-platform-gradle-work-action', 'example:instrumentation-only:1')
    add('unusedIdeMetadata', 'example:ide-only:1')
}
tasks.named('compileJava') { doFirst { throw new GradleException('Compilation must not run') } }
GRADLE
printf "plugins { id 'java-library' }\n" > "$fixture/library/build.gradle"
"$gradle" -p "$fixture" --no-daemon --offline --console=plain --init-script "$resolver" resolveCiDependencies > "$fixture/success.log" 2>&1 || { cat "$fixture/success.log"; exit 1; }
grep -q 'Caching :library:runtimeClasspath' "$fixture/success.log"
for configuration in debugAndroidTestCompileClasspath debugUnitTestRuntimeClasspath sideloadUnitTestCompileClasspath releaseUnitTestRuntimeClasspath releaseRuntimeClasspath androidLintTool kotlinCompilerPluginClasspathSideloadUnitTest robolectricDependency0; do
  grep -q "Caching ::$configuration" "$fixture/success.log"
done
if grep -Eq 'Caching ::(unified-test-platform|unusedIdeMetadata)' "$fixture/success.log"; then
  echo 'warming must not visit instrumentation-only or IDE configurations' >&2
  exit 1
fi
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
dependencies { runtimeOnly 'example:missing:1' }
GRADLE
if "$gradle" -p "$fixture" --no-daemon --offline --console=plain --init-script "$resolver" resolveCiDependencies > "$fixture/failure.log" 2>&1; then
  echo 'unresolved dependency must fail the warming invocation' >&2
  exit 1
fi
grep -q 'missing-1.jar' "$fixture/failure.log"
grep -q 'BUILD FAILED' "$fixture/failure.log"
echo 'Gradle dependency cache fixture passed (success without compilation; missing dependency exits nonzero)'
