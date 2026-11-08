#!/bin/sh
set -eu

module='go.mau.fi/mautrix-gmessages@v0.2610.1-0.20261103213626-d9140a5e807e'
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
destination="$root/build/third_party/mautrix-gmessages"
download=$(go mod download -json "$module")
source_dir=$(printf '%s\n' "$download" | sed -n 's/^[[:space:]]*"Dir": "\(.*\)",$/\1/p')

if [ -z "$source_dir" ] || [ ! -d "$source_dir" ]; then
	printf '%s\n' 'could not find downloaded libgm source' >&2
	exit 1
fi

rm -rf "$destination"
mkdir -p "$(dirname "$destination")"
cp -R "$source_dir" "$destination"
chmod -R u+w "$destination"

cd "$root"
shasum -a 256 -c upstream-safety.patch.sha256
patch -F 0 -d "$destination" -p0 < upstream-safety.patch

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp -R "$source_dir" "$tmp/module"
chmod -R u+w "$tmp/module"
patch -F 0 -d "$tmp/module" -p0 < upstream-safety.patch
diff -ruN "$tmp/module" "$destination"
