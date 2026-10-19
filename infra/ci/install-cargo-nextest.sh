#!/usr/bin/env bash
set -euo pipefail

if (($# != 1)); then echo "usage: $0 <bin-dir>" >&2; exit 2; fi
bin_dir=$1
version=0.9.148
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) triple=x86_64-unknown-linux-gnu; checksum=fc235b0b8e6769f1dd9efe757094f4562b7acd230d0a721d3157449f42f89f18b0288095579789bba2419a9c46b0a46cfd12b9306be20ace6c2852b456590e58 ;;
    Darwin-arm64) triple=universal-apple-darwin; checksum=b562270badb12720babfe095bb5df32ce32b4facaf466746cf4776e62c1f5f0c6c9cb0fa1f2ab3d8b3fdc7072fa104433b6ec1e35ea8a2accbad1c6277c4aca8 ;;
    *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
if [[ -x "$bin_dir/cargo-nextest" ]] && [[ $("$bin_dir/cargo-nextest" --version | awk '{print $2}') == "$version" ]]; then exit 0; fi
mkdir -p "$bin_dir"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
archive="$tmp/cargo-nextest.tar.gz"
url="https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/cargo-nextest-$version-$triple.tar.gz"
curl --proto '=https' --proto-redir '=https' --fail --location --silent --show-error "$url" --output "$archive"
if command -v sha512sum >/dev/null; then actual=$(sha512sum "$archive" | awk '{print $1}'); else actual=$(shasum -a 512 "$archive" | awk '{print $1}'); fi
if [[ $actual != "$checksum" ]]; then echo "cargo-nextest checksum mismatch" >&2; exit 1; fi
tar -xzf "$archive" -C "$tmp"
install "$tmp/cargo-nextest" "$bin_dir/cargo-nextest"
