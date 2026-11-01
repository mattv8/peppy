#!/usr/bin/env python3
"""Check that CI path filters keep Rust and Android build closures in scope.

Checks:
- every crates/, services/ and vendor/ directory is matched by a named filter
  category and not by the catch-all `unclassified` category;
- every local path dependency reachable from a build root is matched by a
  filter category that triggers one of that root's CI outputs;
- every Android closure crate and root Cargo file is listed in android.paths.

Stdlib only; requires Python 3.11+ for tomllib.
"""

from __future__ import annotations

import argparse
import posixpath
import re
import sys
from collections import deque
from dataclasses import dataclass
from pathlib import Path

try:
    import tomllib
except ImportError:
    sys.exit("check-ci-scope-closure: Python 3.11+ is required (tomllib)")

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_FILTERS = REPO_ROOT / ".github" / "ci-paths-filter.yml"
DEFAULT_ANDROID_PATHS = REPO_ROOT / "infra" / "release" / "components" / "android.paths"

CLASSIFIED_PARENTS = ("crates", "services", "vendor")
UNCLASSIFIED = "unclassified"
NON_SCOPE_CATEGORIES = frozenset({"ci", UNCLASSIFIED})
MANIFEST = "Cargo.toml"
ANDROID_ROOT_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")
ANDROID_OUTPUT = "android"
DEPENDENCY_KEYS = ("dependencies", "dev-dependencies", "build-dependencies")
GLOB_METACHARACTERS = frozenset("*?[")

RUST_CATEGORIES = frozenset({"rust_shared", "rust_mobile", "rust_desktop", "rust_browser", "rust_server"})

# Filter categories that make each CI output true, per the rules in ci-scope.sh.
OUTPUT_TRIGGERS: dict[str, frozenset[str]] = {
    "desktop": RUST_CATEGORIES | {"desktop", "server", "web"},
    "android": frozenset({"rust_shared", "rust_mobile", ANDROID_OUTPUT}),
    "swift_bindings": frozenset({"rust_shared", "rust_mobile", "ios", "android_bindings"}),
    "container_image": frozenset(
        {"rust_shared", "rust_desktop", "rust_browser", "rust_server", "server", "web"}
    ),
}


@dataclass(frozen=True)
class BuildRoot:
    path: str
    outputs: tuple[str, ...]


BUILD_ROOTS = (
    BuildRoot("crates/mobile-bindings", (ANDROID_OUTPUT, "swift_bindings")),
    BuildRoot("apps/desktop/src-tauri", ("desktop",)),
    BuildRoot("crates/browser-bindings", ("container_image",)),
    BuildRoot("services/server", ("container_image",)),
    BuildRoot("services/push-relay", ("container_image",)),
)

# (negated, compiled glob); a later pattern overrides an earlier match, as in path filters.
Rule = tuple[bool, re.Pattern[str]]


@dataclass(frozen=True)
class Workspace:
    path_dependencies: dict[str, str]
    patch_dirs: tuple[str, ...]


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--filters", type=Path, default=DEFAULT_FILTERS, help="path filter YAML")
    parser.add_argument(
        "--android-paths", type=Path, default=DEFAULT_ANDROID_PATHS, help="android.paths list"
    )
    return parser.parse_args(argv)


def unquote(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
        return value[1:-1]
    return value


def glob_to_regex(pattern: str) -> re.Pattern[str]:
    parts: list[str] = []
    index = 0
    while index < len(pattern):
        if pattern.startswith("**/", index):
            parts.append("(?:.*/)?")
            index += 3
        elif pattern.startswith("**", index):
            parts.append(".*")
            index += 2
        else:
            char = pattern[index]
            parts.append({"*": "[^/]*", "?": "[^/]"}.get(char, re.escape(char)))
            index += 1
    return re.compile("".join(parts))


def read_filters(path: Path) -> dict[str, list[Rule]]:
    categories: dict[str, list[Rule]] = {}
    current: list[Rule] | None = None
    for number, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.rstrip()
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line[0].isspace():
            if not line.endswith(":"):
                raise ValueError(f"{path}:{number}: expected a 'name:' category line")
            current = categories.setdefault(line[:-1], [])
            continue
        item = line.strip()
        if current is None or not item.startswith("- "):
            raise ValueError(f"{path}:{number}: expected a '- pattern' item")
        pattern = unquote(item[2:].strip())
        negated = pattern.startswith("!")
        current.append((negated, glob_to_regex(pattern.removeprefix("!"))))
    return categories


def read_path_list(path: Path) -> list[str]:
    entries = []
    for raw in path.read_text().splitlines():
        entry = raw.split("#", 1)[0].strip()
        if entry:
            entries.append(entry)
    return entries


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def normalize_path(path: str, base: str = "") -> str:
    return posixpath.normpath(posixpath.join(base, path))


def probe(directory: str) -> str:
    return f"{directory}/{MANIFEST}"


def matches_rules(rules: list[Rule], relpath: str) -> bool:
    matched = False
    for negated, regex in rules:
        if regex.fullmatch(relpath):
            matched = not negated
    return matched


def matching_categories(filters: dict[str, list[Rule]], relpath: str) -> set[str]:
    return {name for name, rules in filters.items() if matches_rules(rules, relpath)}


def classified_dirs() -> list[str]:
    dirs = []
    for parent in CLASSIFIED_PARENTS:
        base = REPO_ROOT / parent
        if not base.is_dir():
            continue
        for child in sorted(base.iterdir()):
            if child.is_dir() and not child.name.startswith("."):
                dirs.append(child.relative_to(REPO_ROOT).as_posix())
    return dirs


def check_classification(filters: dict[str, list[Rule]], directories: list[str]) -> list[str]:
    failures = []
    for directory in directories:
        matched = matching_categories(filters, probe(directory))
        if not matched - NON_SCOPE_CATEGORIES:
            failures.append(f"{directory} is matched by no named filter category")
        if UNCLASSIFIED in matched:
            failures.append(f"{directory} is matched by the unclassified category")
    return failures


def load_workspace() -> Workspace:
    manifest = load_toml(REPO_ROOT / MANIFEST)
    workspace_deps = manifest.get("workspace", {}).get("dependencies", {})
    path_dependencies = {
        name: normalize_path(spec["path"])
        for name, spec in workspace_deps.items()
        if isinstance(spec, dict) and "path" in spec
    }
    patch_dirs = tuple(
        normalize_path(spec["path"])
        for registry in manifest.get("patch", {}).values()
        for spec in registry.values()
        if isinstance(spec, dict) and "path" in spec
    )
    return Workspace(path_dependencies, patch_dirs)


def dependency_tables(manifest: dict) -> list[dict]:
    tables = [manifest.get(key, {}) for key in DEPENDENCY_KEYS]
    for target in manifest.get("target", {}).values():
        tables.extend(target.get(key, {}) for key in DEPENDENCY_KEYS)
    return tables


def local_dependency_dirs(manifest: dict, base: str, workspace: Workspace) -> list[str]:
    found = []
    for table in dependency_tables(manifest):
        for name, spec in table.items():
            if not isinstance(spec, dict):
                continue
            if "path" in spec:
                found.append(normalize_path(spec["path"], base))
            elif spec.get("workspace") is True and name in workspace.path_dependencies:
                found.append(workspace.path_dependencies[name])
    return found


def path_dependency_closure(root: str, workspace: Workspace) -> tuple[set[str], list[str]]:
    nodes: set[str] = set()
    problems: list[str] = []
    queue = deque([root, *workspace.patch_dirs])
    while queue:
        node = queue.popleft()
        if node in nodes:
            continue
        nodes.add(node)
        manifest_path = REPO_ROOT / node / MANIFEST
        if not manifest_path.is_file():
            problems.append(f"{root}: path dependency {node} has no {MANIFEST}")
            continue
        queue.extend(local_dependency_dirs(load_toml(manifest_path), node, workspace))
    return nodes, problems


def check_closure_triggers(
    root: BuildRoot, nodes: set[str], filters: dict[str, list[Rule]]
) -> list[str]:
    failures = []
    for output in root.outputs:
        triggers = OUTPUT_TRIGGERS[output]
        for node in sorted(nodes):
            if not matching_categories(filters, probe(node)) & triggers:
                failures.append(
                    f"{root.path}: closure crate {node} is not matched by a category that triggers '{output}'"
                )
    return failures


def is_covered(relpath: str, entries: list[str]) -> bool:
    return any(relpath == entry or relpath.startswith(f"{entry}/") for entry in entries)


def check_android_paths(nodes: set[str], entries: list[str]) -> list[str]:
    failures = [
        f"android.paths entry {entry} must be a literal path, not a glob"
        for entry in entries
        if GLOB_METACHARACTERS & set(entry)
    ]
    required = [*sorted(nodes), *ANDROID_ROOT_FILES]
    failures += [
        f"android closure input {relpath} is not covered by android.paths"
        for relpath in required
        if not is_covered(relpath, entries)
    ]
    return failures


def run_checks(filters_path: Path, android_paths: Path) -> list[str]:
    filters = read_filters(filters_path)
    entries = read_path_list(android_paths)
    workspace = load_workspace()
    failures = check_classification(filters, classified_dirs())
    for root in BUILD_ROOTS:
        nodes, problems = path_dependency_closure(root.path, workspace)
        failures += problems + check_closure_triggers(root, nodes, filters)
        if ANDROID_OUTPUT in root.outputs:
            failures += check_android_paths(nodes, entries)
    return failures


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        failures = run_checks(args.filters, args.android_paths)
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1
    for failure in failures:
        print(f"FAIL: {failure}")
    if failures:
        print(f"ci scope closure: {len(failures)} failure(s)")
        return 1
    print(f"ci scope closure: OK ({len(BUILD_ROOTS)} build roots, {len(classified_dirs())} classified dirs)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
