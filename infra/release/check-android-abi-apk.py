#!/usr/bin/env python3
"""Compare signed ABI APKs with their universal APK."""

import argparse
import re
import stat
import sys
import zipfile
from pathlib import Path

ABIS = frozenset({"arm64-v8a", "x86_64"})
SIGNATURE = re.compile(r"^META-INF/(?:MANIFEST\.MF|[^/]+\.(?:SF|RSA|DSA|EC))$", re.I)


class ApkError(ValueError):
    """An APK does not match the expected ABI contents."""


def entries(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    result = [entry for entry in archive.infolist() if not SIGNATURE.fullmatch(entry.filename)]
    names = [entry.filename for entry in result]
    if len(names) != len(set(names)):
        raise ApkError("duplicate ZIP entries")
    return result


def abis(infos: list[zipfile.ZipInfo]) -> set[str]:
    result = set()
    libraries = set()
    for entry in infos:
        parts = entry.filename.split("/")
        if parts[0] == "lib" and len(parts) > 1 and parts[1]:
            result.add(parts[1])
            mode = entry.external_attr >> 16
            if (len(parts) == 3 and parts[2].endswith(".so") and parts[2] != ".so"
                    and stat.S_IFMT(mode) in (0, stat.S_IFREG) and not entry.is_dir()):
                libraries.add(parts[1])
    if result != libraries:
        raise ApkError("each ABI subtree must contain a regular .so library")
    return result


def compare(universal_path: Path, apk_paths: dict[str, Path]) -> None:
    if set(apk_paths) != ABIS:
        raise ApkError("exactly one APK for each supported ABI is required")
    with zipfile.ZipFile(universal_path) as universal:
        original = entries(universal)
        if abis(original) != ABIS:
            raise ApkError("universal APK must contain exactly arm64-v8a and x86_64")
        for expected_abi, path in apk_paths.items():
            with zipfile.ZipFile(path) as apk:
                actual = entries(apk)
                found = abis(actual)
                if len(found) != 1 or not found <= ABIS:
                    raise ApkError(f"{path}: expected exactly one supported ABI")
                abi = next(iter(found))
                if abi != expected_abi:
                    raise ApkError(f"{path}: expected ABI {expected_abi}, found {abi}")
                other = next(iter(ABIS - found))
                expected = [entry for entry in original if not entry.filename.startswith(f"lib/{other}/")]
                if [entry.filename for entry in actual] != [entry.filename for entry in expected]:
                    raise ApkError(f"{path}: entry names or order differ from universal APK")
                for source, target in zip(expected, actual):
                    if source.compress_type != target.compress_type:
                        raise ApkError(f"{path}: compression differs for {source.filename}")
                    with universal.open(source) as left, apk.open(target) as right:
                        while True:
                            chunk = left.read(1024 * 1024)
                            if chunk != right.read(1024 * 1024):
                                raise ApkError(f"{path}: contents differ for {source.filename}")
                            if not chunk:
                                break


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("universal", type=Path)
    parser.add_argument("--abi", action="append", required=True, metavar="ABI=PATH")
    args = parser.parse_args()
    try:
        paths = {}
        for assignment in args.abi:
            abi, separator, path = assignment.partition("=")
            if not separator or not path or abi not in ABIS or abi in paths:
                raise ApkError(f"invalid or duplicate ABI assignment: {assignment}")
            paths[abi] = Path(path)
        compare(args.universal, paths)
    except (ApkError, OSError, zipfile.BadZipFile, RuntimeError) as error:
        print(f"ABI APK verification failed: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
