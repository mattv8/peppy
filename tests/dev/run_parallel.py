"""Run the development Python test modules concurrently."""

import argparse
import concurrent.futures
import os
import subprocess
import sys
import time
from pathlib import Path


TEST_DIRECTORY = Path(__file__).parent
REPOSITORY_ROOT = TEST_DIRECTORY.parents[1]


def default_jobs() -> int:
    """Return the CPUs this process may use, honoring container CPU affinity limits."""
    try:
        return len(os.sched_getaffinity(0))
    except AttributeError:
        return os.cpu_count() or 1


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--jobs",
        type=int,
        default=default_jobs(),
        help="maximum concurrent test modules (default: CPU count)",
    )
    arguments = parser.parse_args()
    if arguments.jobs < 1:
        parser.error("--jobs must be at least 1")
    return arguments


def run_module(module: str) -> tuple[str, int, float, str]:
    started_at = time.perf_counter()
    result = subprocess.run(
        [sys.executable, "-m", "unittest", f"tests.dev.{module}"],
        cwd=REPOSITORY_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    return module, result.returncode, time.perf_counter() - started_at, result.stdout


def main() -> int:
    arguments = parse_arguments()
    modules = sorted(path.stem for path in TEST_DIRECTORY.glob("test_*.py"))
    if not modules:
        print(f"No test modules found in {TEST_DIRECTORY}", file=sys.stderr)
        return 1

    started_at = time.perf_counter()
    failed_modules = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=arguments.jobs) as executor:
        futures = [executor.submit(run_module, module) for module in modules]
        for future in concurrent.futures.as_completed(futures):
            module, return_code, duration, output = future.result()
            status = "PASS" if return_code == 0 else "FAIL"
            print(f"\n=== {module}: {status} ({duration:.1f}s) ===")
            print(output, end="" if output.endswith("\n") else "\n")
            if return_code:
                failed_modules.append(module)

    wall_time = time.perf_counter() - started_at
    failures = ", ".join(failed_modules) if failed_modules else "none"
    print(
        f"Summary: {len(modules)} modules run, failed modules: {failures}, "
        f"wall time: {wall_time:.1f}s"
    )
    return 1 if failed_modules else 0


if __name__ == "__main__":
    raise SystemExit(main())
