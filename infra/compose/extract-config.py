#!/usr/bin/env python3
"""Extract literal Compose config content without requiring a YAML parser."""

from pathlib import Path
import sys
from typing import Sequence


def extract_config(path: str, name: str) -> str:
    """Return the named top-level Compose config's literal content."""
    try:
        lines = Path(path).read_text(encoding="utf-8").splitlines(keepends=True)
    except OSError as error:
        raise ValueError(f"cannot read Compose file {path}: {error}") from error

    configs_start = _top_level_configs_start(lines)
    config_start = _named_config_start(lines, configs_start, name)
    content_start = _content_start(lines, config_start, name)
    content = _literal_content(lines, content_start)
    return content.replace("$$", "$")


def _top_level_configs_start(lines: Sequence[str]) -> int:
    for index, line in enumerate(lines):
        if line == "configs:\n" or line == "configs:\r\n" or line == "configs:":
            return index
    raise ValueError("Compose file has no top-level configs: key")


def _named_config_start(lines: Sequence[str], configs_start: int, name: str) -> int:
    expected = f"  {name}:"
    for index in range(configs_start + 1, len(lines)):
        line = lines[index]
        if line and not line[0].isspace():
            break
        if line.rstrip("\r\n") == expected:
            return index
    raise ValueError(f"config {name!r} was not found")


def _content_start(lines: Sequence[str], config_start: int, name: str) -> int:
    for index in range(config_start + 1, len(lines)):
        line = lines[index]
        if line and not line[0].isspace():
            break
        if line.startswith("  ") and not line.startswith("    "):
            break
        if line.rstrip("\r\n") == "    content: |":
            return index + 1
    raise ValueError(f"config {name!r} has no content: | block")


def _literal_content(lines: Sequence[str], content_start: int) -> str:
    content_lines = []
    for line in lines[content_start:]:
        if line.strip() and not line.startswith("      "):
            break
        if line.startswith("      "):
            content_lines.append(line[6:])
        elif not line.strip():
            content_lines.append(line)
    while content_lines and not content_lines[-1].strip():
        content_lines.pop()
    return "".join(content_lines)


def main(arguments: Sequence[str]) -> int:
    if len(arguments) != 2:
        print("usage: extract-config.py <compose-file> <config-name>", file=sys.stderr)
        return 2
    try:
        sys.stdout.write(extract_config(*arguments))
    except ValueError as error:
        print(f"extract-config.py: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
