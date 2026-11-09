#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mobile_pin='v0.0.0-20261011034528-197770444867'
libgm_version='v0.2610.1-0.20261103213626-d9140a5e807e'
cd "$root"

expected_safety_patch_sha256=$(awk 'NR == 1 { print $1 }' upstream-safety.patch.sha256)
safety_patch_sha256=$(shasum -a 256 upstream-safety.patch | awk '{print $1}')
if [ -z "$expected_safety_patch_sha256" ] || [ "$safety_patch_sha256" != "$expected_safety_patch_sha256" ]; then
	printf '%s\n' 'upstream-safety.patch checksum does not match upstream-safety.patch.sha256' >&2
	exit 1
fi

./prepare-libgm.sh
go vet ./...
go vet go.mau.fi/mautrix-gmessages/pkg/libgm
go test ./...
go test -race go.mau.fi/mautrix-gmessages/pkg/libgm
GOBIN="$PWD/build/bin" go install "golang.org/x/mobile/cmd/gomobile@$mobile_pin" "golang.org/x/mobile/cmd/gobind@$mobile_pin"
PATH="$PWD/build/bin:$PATH" gomobile init
PATH="$PWD/build/bin:$PATH" gomobile bind -target=android/arm64,android/amd64 -androidapi 26 -javapkg dev.peppy.gmlink -ldflags='-extldflags=-Wl,-z,max-page-size=16384' -o build/gmlink.aar ./gmlink
./check-generated-host.sh build/gmlink.aar

find_readelf() {
	for ndk in "${ANDROID_NDK_HOME:-}" "${ANDROID_HOME:-}/ndk/27.2.12479018" "$HOME/Library/Android/sdk/ndk/27.2.12479018"; do
		[ -n "$ndk" ] || continue
		for candidate in "$ndk"/toolchains/llvm/prebuilt/*/bin/llvm-readelf; do
			if [ -x "$candidate" ]; then
				printf '%s\n' "$candidate"
				return 0
			fi
		done
	done
	command -v llvm-readelf 2>/dev/null || return 1
}

check_aar_alignment() {
	readelf=$(find_readelf) || {
		printf '%s\n' 'llvm-readelf not found' >&2
		return 1
	}
	entries=$(unzip -Z1 build/gmlink.aar)
	case "$entries" in
		*'jni/arm64-v8a/libgojni.so'*'jni/x86_64/libgojni.so'*) ;;
		*) printf '%s\n' 'missing required libgojni.so ABIs' >&2; return 1 ;;
	esac
	count=0
	for library in $entries; do
		case "$library" in
			*/libgojni.so)
				count=$((count + 1))
				unzip -p build/gmlink.aar "$library" > build/libgojni.so
				"$readelf" -lW build/libgojni.so > build/libgojni.readelf
				alignments=$(awk '$1 == "LOAD" { print $NF }' build/libgojni.readelf)
				if [ -z "$alignments" ]; then
					printf '%s\n' "no LOAD segments in $library" >&2
					return 1
				fi
				for alignment in $alignments; do
					if [ "$((alignment))" -lt "$((0x4000))" ]; then
						printf '%s\n' "insufficient LOAD alignment in $library: $alignment" >&2
						return 1
					fi
				done
				;;
		esac
	done
	if [ "$count" -ne 2 ]; then
		printf '%s\n' "expected 2 libgojni.so entries, found $count" >&2
		return 1
	fi
}

check_aar_alignment

aar_sha256=$(shasum -a 256 build/gmlink.aar | awk '{print $1}')
inputs_sha256=$(
	{
		find gmlink -maxdepth 1 -type f -name '*.go' -print | LC_ALL=C sort
		printf '%s\n' go.mod go.sum
	} | while IFS= read -r file; do
		shasum -a 256 "$file"
	done | shasum -a 256 | awk '{print $1}'
)
cat > build/gmlink.aar.provenance <<EOF
aar_sha256=$aar_sha256
libgm_version=$libgm_version
safety_patch_sha256=$safety_patch_sha256
inputs_sha256=$inputs_sha256
EOF
