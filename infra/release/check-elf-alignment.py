#!/usr/bin/env python3
"""Fail closed when Android native ELF libraries lack 16 KiB LOAD alignment."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import struct
import sys
from typing import BinaryIO
import zipfile


MINIMUM_LOAD_ALIGNMENT = 16_384
ELF_MAGIC = b"\x7fELF"
ELF_HEADER_SIZES = {1: 52, 2: 64}
PROGRAM_HEADER_SIZES = {1: 32, 2: 56}
ELF_DATA_ENCODINGS = {1: "<", 2: ">"}
PT_LOAD = 1


class AlignmentError(ValueError):
    """An artifact contains no valid, sufficiently aligned native library."""


@dataclass(frozen=True)
class ElfLayout:
    elf_class: int
    byte_order: str
    program_header_offset: int
    program_header_size: int
    program_header_count: int


def read_exact(source: BinaryIO, byte_count: int) -> bytes:
    data = source.read(byte_count)
    if len(data) != byte_count:
        raise AlignmentError("truncated ELF data")
    return data


def parse_elf_layout(source: BinaryIO, library_size: int) -> ElfLayout:
    if library_size < 16:
        raise AlignmentError("ELF file is too short")
    identifier = read_exact(source, 16)
    if identifier[:4] != ELF_MAGIC:
        raise AlignmentError("missing ELF magic")

    elf_class = identifier[4]
    byte_order = ELF_DATA_ENCODINGS.get(identifier[5])
    if elf_class not in ELF_HEADER_SIZES:
        raise AlignmentError(f"unsupported ELF class {elf_class}")
    if byte_order is None:
        raise AlignmentError(f"unsupported ELF data encoding {identifier[5]}")
    if identifier[6] != 1:
        raise AlignmentError(f"unsupported ELF version {identifier[6]}")

    header_size = ELF_HEADER_SIZES[elf_class]
    if library_size < header_size:
        raise AlignmentError("truncated ELF header")
    header = identifier + read_exact(source, header_size - len(identifier))
    if elf_class == 1:
        program_header_offset = struct.unpack_from(f"{byte_order}I", header, 28)[0]
        elf_header_size = struct.unpack_from(f"{byte_order}H", header, 40)[0]
        program_header_size, program_header_count = struct.unpack_from(f"{byte_order}HH", header, 42)
    else:
        program_header_offset = struct.unpack_from(f"{byte_order}Q", header, 32)[0]
        elf_header_size = struct.unpack_from(f"{byte_order}H", header, 52)[0]
        program_header_size, program_header_count = struct.unpack_from(f"{byte_order}HH", header, 54)

    if elf_header_size != header_size:
        raise AlignmentError("unexpected ELF header size")
    expected_program_header_size = PROGRAM_HEADER_SIZES[elf_class]
    if program_header_size != expected_program_header_size:
        raise AlignmentError("unexpected ELF program header size")
    if program_header_count == 0 or program_header_count == 0xFFFF:
        raise AlignmentError("ELF has no supported program header table")
    if program_header_offset < header_size:
        raise AlignmentError("program header table overlaps ELF header")
    table_size = program_header_size * program_header_count
    if program_header_offset > library_size or table_size > library_size - program_header_offset:
        raise AlignmentError("program header table exceeds ELF bounds")
    return ElfLayout(elf_class, byte_order, program_header_offset, program_header_size, program_header_count)


def verify_elf(source: BinaryIO, library_size: int) -> None:
    layout = parse_elf_layout(source, library_size)
    source.seek(layout.program_header_offset)
    program_headers = read_exact(source, layout.program_header_size * layout.program_header_count)
    load_segments = 0
    for index in range(layout.program_header_count):
        offset = index * layout.program_header_size
        program_type = struct.unpack_from(f"{layout.byte_order}I", program_headers, offset)[0]
        if program_type != PT_LOAD:
            continue
        load_segments += 1
        if layout.elf_class == 1:
            file_offset = struct.unpack_from(f"{layout.byte_order}I", program_headers, offset + 4)[0]
            virtual_address = struct.unpack_from(f"{layout.byte_order}I", program_headers, offset + 8)[0]
            file_size = struct.unpack_from(f"{layout.byte_order}I", program_headers, offset + 16)[0]
        else:
            file_offset = struct.unpack_from(f"{layout.byte_order}Q", program_headers, offset + 8)[0]
            virtual_address = struct.unpack_from(f"{layout.byte_order}Q", program_headers, offset + 16)[0]
            file_size = struct.unpack_from(f"{layout.byte_order}Q", program_headers, offset + 32)[0]
        if file_offset > library_size or file_size > library_size - file_offset:
            raise AlignmentError(f"PT_LOAD segment {index} exceeds ELF bounds")
        alignment_offset = 28 if layout.elf_class == 1 else 48
        alignment_format = "I" if layout.elf_class == 1 else "Q"
        alignment = struct.unpack_from(
            f"{layout.byte_order}{alignment_format}", program_headers, offset + alignment_offset
        )[0]
        if alignment == 0 or alignment & (alignment - 1):
            raise AlignmentError(f"PT_LOAD segment {index} alignment {alignment} is not a power of two")
        if alignment < MINIMUM_LOAD_ALIGNMENT:
            raise AlignmentError(
                f"PT_LOAD segment {index} alignment {alignment} is below {MINIMUM_LOAD_ALIGNMENT}"
            )
        if virtual_address % alignment != file_offset % alignment:
            raise AlignmentError(
                f"PT_LOAD segment {index} virtual address {virtual_address} is incongruent with "
                f"file offset {file_offset} for alignment {alignment}"
            )
    if load_segments == 0:
        raise AlignmentError("ELF has no PT_LOAD segments")


def archive_library_names(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    libraries = []
    for entry in archive.infolist():
        path = Path(entry.filename)
        if path.is_absolute() or ".." in path.parts:
            raise AlignmentError(f"unsafe archive entry path: {entry.filename}")
        if not entry.is_dir() and entry.filename.endswith(".so"):
            libraries.append(entry)
    if not libraries:
        raise AlignmentError("archive contains no native libraries")
    return libraries


def verify_archive(path: Path) -> int:
    try:
        with zipfile.ZipFile(path) as archive:
            libraries = archive_library_names(archive)
            for entry in libraries:
                with archive.open(entry) as library:
                    try:
                        verify_elf(library, entry.file_size)
                    except AlignmentError as error:
                        raise AlignmentError(f"{entry.filename}: {error}") from error
    except (zipfile.BadZipFile, zipfile.LargeZipFile, RuntimeError, NotImplementedError, EOFError) as error:
        raise AlignmentError(f"corrupt or unsupported archive: {error}") from error
    return len(libraries)


def verify_library(path: Path) -> int:
    with path.open("rb") as library:
        verify_elf(library, path.stat().st_size)
    return 1


def verify_directory(path: Path) -> int:
    libraries = sorted(candidate for candidate in path.rglob("*.so") if candidate.is_file())
    if not libraries:
        raise AlignmentError("directory contains no native libraries")
    return sum(verify_library(library) for library in libraries)


def verify_artifact(path: Path) -> int:
    if path.is_dir():
        return verify_directory(path)
    if not path.is_file():
        raise AlignmentError("artifact does not exist or is not a regular file")
    if zipfile.is_zipfile(path):
        return verify_archive(path)
    return verify_library(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", nargs="+", type=Path, metavar="ARTIFACT")
    arguments = parser.parse_args()
    failed = False
    for artifact in arguments.artifacts:
        try:
            library_count = verify_artifact(artifact)
        except (AlignmentError, OSError, zipfile.BadZipFile) as error:
            print(f"{artifact}: {error}", file=sys.stderr)
            failed = True
        else:
            print(f"{artifact}: verified {library_count} native librar{'y' if library_count == 1 else 'ies'}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
