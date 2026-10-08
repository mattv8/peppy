#!/usr/bin/env python3
"""Choose a registered Git worktree for a development command."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from dataclasses import dataclass, replace
from pathlib import Path


class SelectionError(Exception):
    """The requested source tree cannot safely be selected."""


@dataclass(frozen=True)
class Worktree:
    path: Path
    branch: str | None
    head: str
    dirty: bool = False
    ahead: int = 0


def git(root: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(root), *arguments],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode:
        detail = result.stderr.strip() or "no diagnostic was provided"
        raise SelectionError(f"Git command failed in {root}: git {' '.join(arguments)}: {detail}")
    return result.stdout


def registered_worktrees(root: Path) -> list[Worktree]:
    records = git(root, "worktree", "list", "--porcelain", "-z").split("\0\0")
    worktrees = []
    for record in records:
        fields = record.split("\0")
        worktree_path = next((field[9:] for field in fields if field.startswith("worktree ")), None)
        if worktree_path and not any(field.startswith("prunable") for field in fields):
            path = Path(worktree_path).resolve()
            if path.exists():
                branch = next((field[7:] for field in fields if field.startswith("branch ")), None)
                if branch and branch.startswith("refs/heads/"):
                    branch = branch.removeprefix("refs/heads/")
                head = next((field[5:] for field in fields if field.startswith("HEAD ")), "unknown")
                worktrees.append(Worktree(path, branch, head))
    return worktrees


def origin_head_exists(root: Path) -> bool:
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "--quiet", "origin/HEAD"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = result.stderr.strip() or "no diagnostic was provided"
    raise SelectionError(f"could not check origin/HEAD in {root}: {detail}")


def is_dirty(worktree: Worktree) -> bool:
    return bool(git(worktree.path, "status", "--porcelain=v1", "-z"))


def ahead_of_origin_head(worktree: Worktree) -> int:
    return int(git(worktree.path, "rev-list", "--count", "origin/HEAD..HEAD").strip())


def qualifying_alternatives(root: Path, worktrees: list[Worktree]) -> list[Worktree]:
    current = root.resolve()
    alternatives = [worktree for worktree in worktrees if worktree.path != current]
    has_origin_head = origin_head_exists(root)
    candidates = [
        replace(
            worktree,
            dirty=is_dirty(worktree),
            ahead=ahead_of_origin_head(worktree) if has_origin_head else 0,
        )
        for worktree in alternatives
    ]
    if not has_origin_head:
        return candidates
    return [worktree for worktree in candidates if worktree.dirty or worktree.ahead]


def selected_override(worktrees: list[Worktree]) -> Path | None:
    source_tree = os.environ.get("PEPPY_SOURCE_TREE")
    if source_tree is None:
        return None
    branch_matches = [
        worktree for worktree in worktrees
        if source_tree in {worktree.branch, f"refs/heads/{worktree.branch}"}
    ]
    if len(branch_matches) == 1:
        return branch_matches[0].path
    if len(branch_matches) > 1:
        raise SelectionError(f"PEPPY_SOURCE_TREE branch is registered by multiple worktrees: {source_tree}")
    selected = Path(source_tree).expanduser().resolve()
    if selected not in {worktree.path for worktree in worktrees}:
        raise SelectionError(f"PEPPY_SOURCE_TREE must name a registered worktree branch or path: {source_tree}")
    return selected


def worktree_label(worktree: Worktree) -> str:
    return worktree.branch or f"detached at {worktree.head[:12]}"


def describe_worktree(worktree: Worktree, current: bool = False) -> str:
    details = [worktree_label(worktree), str(worktree.path)]
    if current:
        details.append("current")
    if worktree.ahead:
        details.append(f"ahead {worktree.ahead}")
    if worktree.dirty:
        details.append("dirty")
    return " — ".join(details)


def prompt_for_selection(current: Worktree, alternatives: list[Worktree]) -> Path:
    if not alternatives:
        return current.path
    if not sys.stdin.isatty():
        print(
            "Divergent worktrees are available; using the current checkout. "
            "Set PEPPY_SOURCE_TREE to select one noninteractively.",
            file=sys.stderr,
        )
        for worktree in alternatives:
            print(f"  {describe_worktree(worktree)}", file=sys.stderr)
        return current.path

    print("Select development source tree:", file=sys.stderr)
    print(f"  1) {describe_worktree(current, current=True)}", file=sys.stderr)
    for index, worktree in enumerate(alternatives, start=2):
        print(f"  {index}) {describe_worktree(worktree)}", file=sys.stderr)
    print("Selection [1]: ", end="", flush=True, file=sys.stderr)
    response = sys.stdin.readline()
    if response == "":
        raise SelectionError("worktree selection input ended")
    selection = response.strip()
    if not selection:
        return current.path
    if selection.lower() in {"q", "quit", "cancel"}:
        raise SelectionError("worktree selection cancelled")
    try:
        index = int(selection)
    except ValueError as error:
        raise SelectionError("invalid worktree selection") from error
    choices = [current.path, *(worktree.path for worktree in alternatives)]
    if index < 1 or index > len(choices):
        raise SelectionError("invalid worktree selection")
    return choices[index - 1]


def select_source(root: Path) -> Path:
    current = root.resolve()
    worktrees = registered_worktrees(current)
    if current not in {worktree.path for worktree in worktrees}:
        raise SelectionError(f"calling root is not a registered worktree: {current}")
    current_worktree = next(worktree for worktree in worktrees if worktree.path == current)
    override = selected_override(worktrees)
    if override is not None:
        selected = override
    else:
        selected = prompt_for_selection(current_worktree, qualifying_alternatives(current, worktrees))
    selected_worktree = next(worktree for worktree in worktrees if worktree.path == selected)
    print(f"Using source tree: {describe_worktree(selected_worktree)}", file=sys.stderr)
    return selected


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        print(select_source(args.root))
    except SelectionError as error:
        print(f"worktree-source: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
