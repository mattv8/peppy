#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/gmlink"
cp "$root/build-aar.sh" "$tmp/build-aar.sh"
printf 'patch\n' > "$tmp/upstream-safety.patch"
shasum -a 256 "$tmp/upstream-safety.patch" > "$tmp/upstream-safety.patch.sha256"
printf 'package gmlink\n' > "$tmp/gmlink/placeholder.go"
: > "$tmp/go.mod"
: > "$tmp/go.sum"

cat > "$tmp/prepare-libgm.sh" <<'EOF'
#!/bin/sh
mkdir -p build/third_party/mautrix-gmessages build/bin
cp "$TEST_GOMOBILE" build/bin/gomobile
printf 'prepare\n' >> "$TEST_LOG"
EOF
cat > "$tmp/check-generated-host.sh" <<'EOF'
#!/bin/sh
printf 'host %s\n' "$1" >> "$TEST_LOG"
EOF
cat > "$tmp/bin/go" <<'EOF'
#!/bin/sh
printf 'go %s\n' "$*" >> "$TEST_LOG"
EOF
cat > "$tmp/bin/gomobile" <<'EOF'
#!/bin/sh
printf 'gomobile %s\n' "$*" >> "$TEST_LOG"
case " $* " in
  *' bind '*) mkdir -p build; : > build/gmlink.aar ;;
esac
EOF
cat > "$tmp/bin/unzip" <<'EOF'
#!/bin/sh
if [ "$1" = '-Z1' ]; then
  printf '%s\n' jni/arm64-v8a/libgojni.so jni/x86_64/libgojni.so
else
  printf 'fixture'
fi
EOF
cat > "$tmp/bin/llvm-readelf" <<'EOF'
#!/bin/sh
printf 'LOAD 0 0 0 0 0 0 0x4000\n'
EOF
chmod +x "$tmp"/build-aar.sh "$tmp"/prepare-libgm.sh "$tmp"/check-generated-host.sh "$tmp/bin"/*

TEST_GOMOBILE="$tmp/bin/gomobile" TEST_LOG="$tmp/log" ANDROID_NDK_HOME="$tmp/none" ANDROID_HOME="$tmp/none" HOME="$tmp/home" PATH="$tmp/bin:$PATH" "$tmp/build-aar.sh"

expected=$(cat <<'EOF'
prepare
go vet ./...
go vet go.mau.fi/mautrix-gmessages/pkg/libgm
go test ./...
go test -race go.mau.fi/mautrix-gmessages/pkg/libgm
go install golang.org/x/mobile/cmd/gomobile@v0.0.0-20261011034528-197770444867 golang.org/x/mobile/cmd/gobind@v0.0.0-20261011034528-197770444867
gomobile init
gomobile bind -target=android/arm64,android/amd64 -androidapi 26 -javapkg dev.peppy.gmlink -ldflags=-extldflags=-Wl,-z,max-page-size=16384 -o build/gmlink.aar ./gmlink
host build/gmlink.aar
EOF
)
actual=$(cat "$tmp/log")
[ "$actual" = "$expected" ] || {
  printf '%s\n%s\n' 'unexpected build-aar command sequence:' "$actual" >&2
  exit 1
}
