"""Test helpers for Compose configs whose content is inlined in YAML."""

import importlib.util
import os
import tempfile
from contextlib import contextmanager
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
_SPEC = importlib.util.spec_from_file_location(
    "peppy_extract_config", ROOT / "infra/compose/extract-config.py"
)
_MODULE = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_MODULE)


def extract_compose_config(compose_file: Path, name: str) -> str:
    """Return the unescaped content of an inlined Compose config."""
    return _MODULE.extract_config(str(compose_file), name)


@contextmanager
def extracted_config_file(compose_file: Path, name: str, executable: bool = False):
    """Materialize an inlined config for a subprocess that requires a path."""
    descriptor, filename = tempfile.mkstemp(prefix=f".peppy-{name}-", dir=ROOT)
    path = Path(filename)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(extract_compose_config(compose_file, name))
        if executable:
            path.chmod(0o755)
        yield path
    finally:
        path.unlink(missing_ok=True)
