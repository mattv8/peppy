#!/usr/bin/env python3
"""Derive ABI APKs from an unsigned universal APK."""

import argparse
import copy
import importlib.util
import os
import shutil
import sys
import tempfile
import zipfile
from pathlib import Path

spec = importlib.util.spec_from_file_location("abi_check", Path(__file__).with_name("check-android-abi-apk.py"))
abi_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(abi_check)


def split(source_path: Path, output_dir: Path) -> None:
    outputs = [output_dir / f"peppy-android-{abi}-unsigned.apk" for abi in sorted(abi_check.ABIS)]
    if any(path.exists() for path in outputs):
        raise abi_check.ApkError("output APK already exists")
    with tempfile.TemporaryDirectory(prefix=".peppy-abi-", dir=output_dir) as temporary_dir:
        staged = [Path(temporary_dir) / path.name for path in outputs]
        with zipfile.ZipFile(source_path) as source:
            infos = source.infolist()
            if any(abi_check.SIGNATURE.fullmatch(entry.filename) and entry.filename.upper() != "META-INF/MANIFEST.MF" for entry in infos):
                raise abi_check.ApkError("input APK contains signature files")
            with source_path.open("rb") as raw:
                if source.start_dir >= 24:
                    raw.seek(source.start_dir - 16)
                    if raw.read(16) == b"APK Sig Block 42":
                        raise abi_check.ApkError("input APK contains an APK Signing Block")
            if abi_check.abis(abi_check.entries(source)) != abi_check.ABIS:
                raise abi_check.ApkError("input APK must contain exactly arm64-v8a and x86_64")
            for abi, output in zip(sorted(abi_check.ABIS), staged):
                other = next(iter(abi_check.ABIS - {abi}))
                with zipfile.ZipFile(output, "x") as target:
                    for entry in infos:
                        if not entry.filename.startswith(f"lib/{other}/"):
                            output_entry = copy.copy(entry)
                            output_entry.extra = b""
                            with source.open(entry) as reader, target.open(output_entry, "w") as writer:
                                output_entry.external_attr = entry.external_attr
                                shutil.copyfileobj(reader, writer)
        abi_check.compare(source_path, dict(zip(sorted(abi_check.ABIS), staged)))
        created = []
        try:
            for source, output in zip(staged, outputs):
                os.link(source, output)
                created.append(output)
        except BaseException:
            for output in created:
                output.unlink()
            raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output_dir", type=Path)
    args = parser.parse_args()
    try:
        split(args.source, args.output_dir)
    except (abi_check.ApkError, OSError, zipfile.BadZipFile, RuntimeError) as error:
        print(f"ABI APK split failed: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
