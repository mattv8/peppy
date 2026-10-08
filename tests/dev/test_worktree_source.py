import importlib.util
import io
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PICKER = ROOT / "infra/dev/worktree_source.py"


def load_picker():
    spec = importlib.util.spec_from_file_location("worktree_source", PICKER)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class TtyInput(io.StringIO):
    def isatty(self):
        return True


class WorktreeSourceTests(unittest.TestCase):
    def git(self, root, *args):
        return subprocess.run(["git", *args], cwd=root, check=True, text=True, capture_output=True)

    def create_repository(self):
        directory = tempfile.TemporaryDirectory()
        root = Path(directory.name) / "main checkout"
        root.mkdir()
        self.git(root, "init", "-q")
        self.git(root, "config", "user.email", "tests@example.test")
        self.git(root, "config", "user.name", "Test User")
        (root / "tracked.txt").write_text("base\n")
        self.git(root, "add", "tracked.txt")
        self.git(root, "commit", "-qm", "base")
        return directory, root

    def select(self, root, *, source_tree=None, input=None):
        env = os.environ | {"PEPPY_REPOSITORY_ROOT": str(root)}
        if source_tree is not None:
            env["PEPPY_SOURCE_TREE"] = source_tree
        return subprocess.run(
            ["python3", str(PICKER), "--root", str(root)],
            input=input,
            text=True,
            capture_output=True,
            env=env,
        )

    def test_explicit_registered_worktree_is_normalized_and_selected_without_prompt(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate tree"
        self.git(root, "worktree", "add", "-qb", "alternate", str(alternate))

        result = self.select(root, source_tree=str(alternate / "."))

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(alternate.resolve()))

    def test_explicit_registered_branch_selects_its_worktree(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate tree"
        self.git(root, "update-ref", "refs/remotes/origin/HEAD", "HEAD")
        self.git(root, "worktree", "add", "-qb", "feature/source", str(alternate))

        result = self.select(root, source_tree="feature/source")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(alternate.resolve()))

    def test_unregistered_explicit_source_fails_before_emitting_a_selection(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)

        result = self.select(root, source_tree=str(root.parent / "missing"))

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("registered worktree", result.stderr)

    def test_invalid_interactive_selection_fails_without_selecting_a_tree(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate"
        self.git(root, "worktree", "add", "-qb", "alternate", str(alternate))
        (alternate / "tracked.txt").write_text("dirty\n")

        picker = load_picker()
        with mock.patch.object(picker.sys, "stdin", TtyInput("99\n")), mock.patch.object(picker.sys, "stderr", io.StringIO()):
            with self.assertRaisesRegex(picker.SelectionError, "invalid worktree selection"):
                picker.select_source(root)

    def test_cancelled_interactive_selection_fails_without_selecting_a_tree(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate"
        self.git(root, "worktree", "add", "-qb", "alternate", str(alternate))
        (alternate / "tracked.txt").write_text("dirty\n")

        picker = load_picker()
        with mock.patch.object(picker.sys, "stdin", TtyInput("cancel\n")), mock.patch.object(picker.sys, "stderr", io.StringIO()):
            with self.assertRaisesRegex(picker.SelectionError, "cancelled"):
                picker.select_source(root)

    def test_end_of_interactive_input_aborts_without_selecting_a_tree(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate"
        self.git(root, "worktree", "add", "-qb", "alternate", str(alternate))
        (alternate / "tracked.txt").write_text("dirty\n")

        picker = load_picker()
        with mock.patch.object(picker.sys, "stdin", TtyInput()), mock.patch.object(picker.sys, "stderr", io.StringIO()):
            with self.assertRaisesRegex(picker.SelectionError, "ended"):
                picker.select_source(root)

    def test_noninteractive_run_keeps_current_tree_and_explains_divergent_alternative(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate"
        self.git(root, "worktree", "add", "-qb", "alternate", str(alternate))
        (alternate / "untracked.txt").write_text("dirty\n")

        result = self.select(root)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(root.resolve()))
        self.assertIn("PEPPY_SOURCE_TREE", result.stderr)
        self.assertIn("alternate", result.stderr)
        self.assertIn(str(alternate), result.stderr)
        self.assertIn("dirty", result.stderr)

    def test_current_tree_is_announced_when_no_alternatives_exist(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)

        result = self.select(root)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(root.resolve()))
        self.assertIn("Using source tree", result.stderr)
        self.assertIn(str(root), result.stderr)

    def test_prompt_shows_branch_path_ahead_count_and_dirty_marker(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "alternate tree"
        self.git(root, "update-ref", "refs/remotes/origin/HEAD", "HEAD")
        self.git(root, "worktree", "add", "-qb", "feature/source", str(alternate))
        (alternate / "ahead.txt").write_text("ahead\n")
        self.git(alternate, "add", "ahead.txt")
        self.git(alternate, "commit", "-qm", "ahead")
        (alternate / "dirty.txt").write_text("dirty\n")

        picker = load_picker()
        stderr = io.StringIO()
        with mock.patch.object(picker.sys, "stdin", TtyInput("\n")), mock.patch.object(picker.sys, "stderr", stderr):
            picker.select_source(root)

        self.assertIn("feature/source", stderr.getvalue())
        self.assertIn(str(alternate), stderr.getvalue())
        self.assertIn("ahead 1", stderr.getvalue())
        self.assertIn("dirty", stderr.getvalue())

    def test_prompt_labels_detached_worktree_with_a_path_containing_spaces(self):
        directory, root = self.create_repository()
        self.addCleanup(directory.cleanup)
        alternate = root.parent / "detached tree"
        self.git(root, "worktree", "add", "--detach", str(alternate))
        (alternate / "dirty.txt").write_text("dirty\n")

        picker = load_picker()
        stderr = io.StringIO()
        with mock.patch.object(picker.sys, "stdin", TtyInput("\n")), mock.patch.object(picker.sys, "stderr", stderr):
            picker.select_source(root)

        self.assertIn("detached at", stderr.getvalue())
        self.assertIn(str(alternate), stderr.getvalue())

    def test_prunable_worktree_record_is_skipped_even_when_path_exists(self):
        picker = load_picker()
        root = Path("/registered root")
        output = "\0".join((
            f"worktree {root}", "HEAD deadbeef", "branch refs/heads/main", "",
            "worktree /still-present", "HEAD deadbeef", "branch refs/heads/old", "prunable stale", "", "",
        ))

        with mock.patch.object(picker, "git", return_value=output), mock.patch.object(picker.Path, "exists", return_value=True):
            worktrees = picker.registered_worktrees(root)

        self.assertEqual([worktree.path for worktree in worktrees], [root.resolve()])

    def test_status_failure_surfaces_its_git_diagnostic(self):
        picker = load_picker()
        worktree = picker.Worktree(Path("/broken"), "broken", "deadbeef")
        failure = subprocess.CompletedProcess([], 128, "", "fatal: bad index file")

        with mock.patch.object(picker.subprocess, "run", return_value=failure):
            with self.assertRaisesRegex(picker.SelectionError, "git status --porcelain=v1 -z: fatal: bad index file"):
                picker.is_dirty(worktree)


if __name__ == "__main__":
    unittest.main()
