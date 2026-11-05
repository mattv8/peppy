#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mobile_pin='v0.0.0-20261011034528-197770444867'
cd "$root"

./prepare-libgm.sh
go vet ./...
go test ./...
GOBIN="$PWD/build/bin" go install "golang.org/x/mobile/cmd/gomobile@$mobile_pin" "golang.org/x/mobile/cmd/gobind@$mobile_pin"
PATH="$PWD/build/bin:$PATH" gomobile init
PATH="$PWD/build/bin:$PATH" gomobile bind -target=android/arm64,android/amd64 -androidapi 26 -javapkg dev.peppy.gmlink -ldflags='-extldflags=-Wl,-z,max-page-size=16384' -o build/gmlink.aar ./gmlink

unzip -Z1 build/gmlink.aar | grep '/libgojni\.so$' | while IFS= read -r library; do
	unzip -p build/gmlink.aar "$library" > build/libgojni.so
	llvm-readelf -lW build/libgojni.so | awk '$1 == "LOAD" { print $NF }' | while IFS= read -r alignment; do
		if [ "$((alignment))" -lt "$((0x4000))" ]; then
			printf '%s\n' "insufficient LOAD alignment: $alignment" >&2
			exit 1
		fi
	done
done
