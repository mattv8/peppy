#!/usr/bin/env python3
"""Create a minimal ELF fixture for release-tooling tests."""

from __future__ import annotations

import argparse
from pathlib import Path
import struct


ELF_MAGIC = b"\x7fELF"
ELF_VERSION = 1
PT_LOAD = 1


def create_elf_fixture(arguments: argparse.Namespace) -> None:
    byte_order = "<" if arguments.endianness == "little" else ">"
    elf_class = 1 if arguments.bits == 32 else 2
    header_size = 52 if arguments.bits == 32 else 64
    program_header_size = 32 if arguments.bits == 32 else 56
    identifier = bytearray(16)
    identifier[:4] = ELF_MAGIC
    identifier[4] = arguments.identifier_class or elf_class
    identifier[5] = arguments.identifier_encoding or (1 if arguments.endianness == "little" else 2)
    identifier[6] = ELF_VERSION

    header = bytearray(header_size)
    header[:16] = identifier
    if arguments.bits == 32:
        struct.pack_into(
            f"{byte_order}HHIIIIIHHHHHH",
            header,
            16,
            3,
            40,
            ELF_VERSION,
            0,
            arguments.program_header_offset,
            0,
            0,
            header_size,
            program_header_size,
            1,
            0,
            0,
            0,
        )
        program_header = struct.pack(
            f"{byte_order}IIIIIIII",
            arguments.program_type,
            arguments.file_offset,
            arguments.virtual_address,
            arguments.virtual_address,
            arguments.file_size,
            arguments.memory_size,
            5,
            arguments.alignment,
        )
    else:
        struct.pack_into(
            f"{byte_order}HHIQQQIHHHHHH",
            header,
            16,
            3,
            183,
            ELF_VERSION,
            0,
            arguments.program_header_offset,
            0,
            0,
            header_size,
            program_header_size,
            1,
            0,
            0,
            0,
        )
        program_header = struct.pack(
            f"{byte_order}IIQQQQQQ",
            arguments.program_type,
            5,
            arguments.file_offset,
            arguments.virtual_address,
            arguments.virtual_address,
            arguments.file_size,
            arguments.memory_size,
            arguments.alignment,
        )
    contents = header
    if arguments.program_header_offset == header_size:
        contents += program_header
    if arguments.extend_to_load_segment:
        contents += b"\0" * max(0, arguments.file_offset + arguments.file_size - len(contents))
    Path(arguments.output).write_bytes(contents)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--bits", choices=(32, 64), type=int, default=64)
    parser.add_argument("--endianness", choices=("little", "big"), default="little")
    parser.add_argument("--alignment", type=int, default=16_384)
    parser.add_argument("--file-offset", type=int, default=0)
    parser.add_argument("--file-size", type=int, default=0)
    parser.add_argument("--memory-size", type=int)
    parser.add_argument("--virtual-address", type=int, default=0)
    parser.add_argument("--extend-to-load-segment", action="store_true")
    parser.add_argument("--program-type", type=int, default=PT_LOAD)
    parser.add_argument("--program-header-offset", type=int)
    parser.add_argument("--identifier-class", type=int)
    parser.add_argument("--identifier-encoding", type=int)
    arguments = parser.parse_args()
    if arguments.program_header_offset is None:
        arguments.program_header_offset = 52 if arguments.bits == 32 else 64
    if arguments.memory_size is None:
        arguments.memory_size = arguments.file_size
    create_elf_fixture(arguments)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
