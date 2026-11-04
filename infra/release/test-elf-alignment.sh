#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/peppy-elf-alignment-test.XXXXXX")
trap 'rm -rf "$root"' EXIT

make_elf() {
  "$script_dir/create-elf-fixture.py" "$@"
}

make_archive() {
  python3 - "$1" "${@:2}" <<'PY'
import pathlib
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], 'w') as archive:
    for index, library in enumerate(sys.argv[2:]):
        archive.writestr(f'base/lib/arm64-v8a/libfixture{index}.so', pathlib.Path(library).read_bytes())
PY
}

expect_failure_reason() {
  local expected=$1
  shift
  local output
  if output=$("$@" 2>&1); then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
  if [[ $output != *"$expected"* ]]; then
    printf 'expected failure reason %q, got:\n%s\n' "$expected" "$output" >&2
    exit 1
  fi
}

mkdir -p "$root/libs/arm64-v8a"
make_elf "$root/libs/arm64-v8a/libaligned64.so"
make_elf "$root/libs/arm64-v8a/libaligned32.so" --bits 32
make_elf "$root/linker-shaped64.so" --virtual-address 16384 --file-size 120
make_elf "$root/linker-shaped32.so" --bits 32 --virtual-address 16384 --file-size 84
make_elf "$root/memory-larger64.so" --file-size 120 --memory-size 4096
make_elf "$root/memory-larger32.so" --bits 32 --file-size 84 --memory-size 4096
make_elf "$root/matching-remainder64.so" --file-offset 128 --file-size 1 --virtual-address 16512 --extend-to-load-segment
make_elf "$root/matching-remainder32.so" --bits 32 --file-offset 96 --file-size 1 --virtual-address 16480 --extend-to-load-segment
"$script_dir/check-elf-alignment.py" "$root/libs" "$root/linker-shaped64.so" "$root/linker-shaped32.so" "$root/memory-larger64.so" "$root/memory-larger32.so" "$root/matching-remainder64.so" "$root/matching-remainder32.so"

make_elf "$root/misaligned.so" --alignment 4096
make_elf "$root/file-span-out-of-bounds.so" --file-offset 120 --file-size 1
make_elf "$root/incongruent64.so" --file-offset 128 --file-size 1 --virtual-address 16384 --extend-to-load-segment
make_elf "$root/incongruent32.so" --bits 32 --file-offset 96 --file-size 1 --virtual-address 16384 --extend-to-load-segment
make_elf "$root/non-power-of-two.so" --alignment 24576
make_elf "$root/no-load.so" --program-type 0
make_elf "$root/invalid-class.so" --identifier-class 3
make_elf "$root/invalid-encoding.so" --identifier-encoding 3
make_elf "$root/out-of-bounds.so" --program-header-offset 4096
printf '\177ELF' > "$root/truncated.so"
expect_failure_reason 'alignment 4096 is below 16384' "$script_dir/check-elf-alignment.py" "$root/misaligned.so"
expect_failure_reason 'PT_LOAD segment 0 exceeds ELF bounds' "$script_dir/check-elf-alignment.py" "$root/file-span-out-of-bounds.so"
expect_failure_reason 'virtual address 16384 is incongruent with file offset 128 for alignment 16384' "$script_dir/check-elf-alignment.py" "$root/incongruent64.so"
expect_failure_reason 'virtual address 16384 is incongruent with file offset 96 for alignment 16384' "$script_dir/check-elf-alignment.py" "$root/incongruent32.so"
expect_failure_reason 'not a power of two' "$script_dir/check-elf-alignment.py" "$root/non-power-of-two.so"
expect_failure_reason 'no PT_LOAD segments' "$script_dir/check-elf-alignment.py" "$root/no-load.so"
expect_failure_reason 'unsupported ELF class 3' "$script_dir/check-elf-alignment.py" "$root/invalid-class.so"
expect_failure_reason 'unsupported ELF data encoding 3' "$script_dir/check-elf-alignment.py" "$root/invalid-encoding.so"
expect_failure_reason 'program header table exceeds ELF bounds' "$script_dir/check-elf-alignment.py" "$root/out-of-bounds.so"
expect_failure_reason 'ELF file is too short' "$script_dir/check-elf-alignment.py" "$root/truncated.so"
mkdir "$root/empty"
expect_failure_reason 'directory contains no native libraries' "$script_dir/check-elf-alignment.py" "$root/empty"

make_archive "$root/aligned.apk" "$root/libs/arm64-v8a/libaligned64.so"
make_archive "$root/aligned.aab" "$root/libs/arm64-v8a/libaligned32.so"
make_archive "$root/misaligned.apk" "$root/misaligned.so"
make_archive "$root/misaligned.aab" "$root/misaligned.so"
make_archive "$root/mixed.apk" "$root/libs/arm64-v8a/libaligned64.so" "$root/misaligned.so"
python3 - "$root/no-libraries.apk" "$root/unsupported-compression.aab" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], 'w') as archive:
    archive.writestr('classes.dex', b'dex')
with zipfile.ZipFile(sys.argv[2], 'w') as archive:
    archive.writestr('base/lib/arm64-v8a/libfixture.so', b'fixture')
with open(sys.argv[2], 'r+b') as archive:
    contents = bytearray(archive.read())
    for signature, method_offset in ((b'PK\x03\x04', 8), (b'PK\x01\x02', 10)):
        offset = contents.index(signature) + method_offset
        contents[offset:offset + 2] = (99).to_bytes(2, 'little')
    archive.seek(0)
    archive.write(contents)
PY
"$script_dir/check-elf-alignment.py" "$root/aligned.apk" "$root/aligned.aab"
expect_failure_reason 'alignment 4096 is below 16384' "$script_dir/check-elf-alignment.py" "$root/misaligned.apk"
expect_failure_reason 'alignment 4096 is below 16384' "$script_dir/check-elf-alignment.py" "$root/misaligned.aab"
expect_failure_reason 'alignment 4096 is below 16384' "$script_dir/check-elf-alignment.py" "$root/mixed.apk"
expect_failure_reason 'archive contains no native libraries' "$script_dir/check-elf-alignment.py" "$root/no-libraries.apk"
expect_failure_reason 'corrupt or unsupported archive' "$script_dir/check-elf-alignment.py" "$root/unsupported-compression.aab"
printf 'ELF alignment tests passed\n'
