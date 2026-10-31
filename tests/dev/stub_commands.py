"""Reusable executable-command stubs for development-helper tests."""

from pathlib import Path
import shutil


RUNNER_DIRECTORY = Path(__file__).parent
RUNNERS = {
    "sh": RUNNER_DIRECTORY / "stub-runner-sh",
    "bash": RUNNER_DIRECTORY / "stub-runner-bash",
}
REPOSITORY_ROOT = RUNNER_DIRECTORY.parents[1]


def write_stub(path: Path, body: str, interpreter: str = "sh") -> Path:
    """Write a non-executable stub body and link its command to a shared runner."""
    runner = RUNNERS[interpreter]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.unlink(missing_ok=True)
    path.with_name(f"{path.name}.body").write_text(body)
    path.symlink_to(runner)
    return path


def install_android_scripts(repository: Path) -> Path:
    """Copy Android helper scripts into an isolated test repository."""
    helper_directory = repository / "infra/dev"
    helper_directory.mkdir(parents=True)
    for name in ("android.sh", "dev_port.py", "dev_tls.py"):
        shutil.copy(REPOSITORY_ROOT / "infra/dev" / name, helper_directory / name)
    return helper_directory / "android.sh"
