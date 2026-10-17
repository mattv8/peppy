#!/usr/bin/env bash
set -euo pipefail

if (($# != 1)); then echo "usage: $0 <bin-dir>" >&2; exit 2; fi
bin_dir=$1
version=2.21
checksum=e7e94ce068878cdb8041602dc41f03e6271835df0b125066cb6fed8c367dfee4
url="https://downloads.sourceforge.net/project/rpmrebuild/rpmrebuild/$version/rpmrebuild-$version.tar.gz"

mkdir -p "$bin_dir"
bin_dir=$(CDPATH='' cd -- "$bin_dir" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
archive="$tmp/rpmrebuild-$version.tar.gz"
root_dir="$bin_dir/rpmrebuild-$version"

curl --fail --location --silent --show-error --retry 3 --retry-delay 1 --retry-connrefused "$url" --output "$archive"
if command -v sha256sum >/dev/null; then
  actual=$(sha256sum "$archive" | awk '{print $1}')
elif command -v shasum >/dev/null; then
  actual=$(shasum -a 256 "$archive" | awk '{print $1}')
else
  echo 'sha256sum or shasum is required to verify rpmrebuild' >&2
  exit 1
fi
[ "$actual" = "$checksum" ] || { echo 'rpmrebuild checksum mismatch' >&2; exit 1; }

payload="$tmp/rpmrebuild-$version"
mkdir "$payload"
tar -xzf "$archive" -C "$payload"
[ -f "$payload/rpmrebuild" ] || { echo 'rpmrebuild archive is missing its launcher' >&2; exit 1; }
if [ ! -f "$payload/rpmrebuild.sh" ] || [ ! -f "$payload/rpmrebuild_lib.src" ] || [ ! -d "$payload/plugins" ] || [ ! -d "$payload/locale" ]; then
  echo 'rpmrebuild archive is missing required runtime files' >&2
  exit 1
fi

rm -rf "$root_dir"
mv "$payload" "$root_dir"
{
  cat <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
EOF
  printf 'root_dir=%q\n' "$root_dir"
  cat <<'EOF'
export RPMREBUILD_ROOT_DIR="$root_dir"
exec "$root_dir/rpmrebuild.sh" "$@"
EOF
} > "$bin_dir/rpmrebuild"
chmod 0755 "$bin_dir/rpmrebuild"
