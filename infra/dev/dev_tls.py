#!/usr/bin/env python3
"""Prepare and apply optional local development TLS."""

import argparse
import contextlib
import fcntl
import os
import re
import ssl
import subprocess
import sys
import tempfile
import zipfile
import zlib
from pathlib import Path
from typing import Optional

try:
    from .dev_port import development_port, development_tls_enabled, dotenv_values, rewrite_loopback_url
except ImportError:
    from dev_port import development_port, development_tls_enabled, dotenv_values, rewrite_loopback_url


ROOT = Path(__file__).resolve().parents[2]
TLS_FILENAMES = ("ca.pem", "ca-key.pem", "server.pem", "server-key.pem")
PRIVATE_FILENAMES = ("ca-key.pem", "server-key.pem")
LEAF_RENEWAL_SECONDS = 30 * 24 * 60 * 60
APK_CA_RESOURCE = "res/raw/peppy_dev_tls_ca.pem"
MAX_APK_CA_BYTES = 64 * 1024
PEM_CERTIFICATE_BEGIN = "-----BEGIN CERTIFICATE-----"


class TlsError(RuntimeError):
    """Raised when local TLS cannot be safely prepared."""


def configured_debug(dotenv: Path) -> bool:
    try:
        return development_tls_enabled(dotenv_values(dotenv))
    except ValueError as error:
        raise TlsError(str(error)) from error


def has_dotenv(root: Path) -> bool:
    path = root / ".env"
    return path.exists() or path.is_symlink()


def git_environment() -> dict[str, str]:
    """Return the caller's environment without repository overrides that would redirect git."""
    return {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}


def main_worktree(porcelain: str) -> Optional[Path]:
    """Return the main worktree named by the first `git worktree list --porcelain` entry, or None when bare."""
    entry = []
    for line in porcelain.splitlines():
        if not line:
            break
        entry.append(line)
    if "bare" in entry:
        return None
    path = next((line[len("worktree "):] for line in entry if line.startswith("worktree ")), None)
    return Path(path).resolve() if path else None


def registered_primary_worktree(root: Path) -> Optional[Path]:
    """Ask git for the primary checkout; None when git reports no usable primary."""
    listing = subprocess.run(
        ["git", "-C", str(root), "worktree", "list", "--porcelain"],
        text=True, errors="replace", capture_output=True, env=git_environment(),
    )
    if listing.returncode:
        return None
    return main_worktree(listing.stdout)


def gitfile_target(root: Path) -> Optional[Path]:
    """Return the existing gitdir named by root's `.git` gitfile, or None when there is no readable target."""
    marker = root / ".git"
    if not marker.is_file():
        return None
    try:
        gitdir_line = marker.read_text().strip()
    except (OSError, ValueError):
        return None
    if not gitdir_line.startswith("gitdir:"):
        return None
    git_dir = (root / gitdir_line[len("gitdir:"):].strip()).resolve()
    return git_dir if git_dir.is_dir() else None


def backlinks_checkout(git_dir: Path, root: Path) -> bool:
    """Return True when git_dir's `gitdir` backlink names root's `.git` gitfile, as linked-worktree admin metadata does."""
    try:
        target = Path((git_dir / "gitdir").read_text().strip())
    except (OSError, ValueError):
        return False
    return (git_dir / target if not target.is_absolute() else target).resolve() == (root / ".git").resolve()


def is_linked_worktree(root: Path) -> bool:
    """Return True for a linked worktree, including one whose commondir is missing but whose backlink proves it, and for a gitfile whose metadata is missing so it cannot be proven primary."""
    if not (root / ".git").is_file():
        return False
    git_dir = gitfile_target(root)
    if git_dir is None:
        return True
    return (git_dir / "commondir").is_file() or backlinks_checkout(git_dir, root)


def legacy_primary_worktree(root: Path) -> Optional[Path]:
    """Read the primary checkout from a standard `.git` gitfile; used only when git is unavailable."""
    git_dir = gitfile_target(root)
    if git_dir is None:
        return None
    try:
        common_dir = (git_dir / (git_dir / "commondir").read_text().strip()).resolve()
    except (OSError, ValueError):
        return None
    if common_dir.name != ".git" or not common_dir.is_dir():
        return None
    return common_dir.parent


def primary_worktree(root: Path) -> Optional[Path]:
    """Return the registered primary checkout when root is a linked worktree, otherwise None."""
    if not (root / ".git").exists():
        return None
    try:
        primary = registered_primary_worktree(root)
    except OSError:
        return legacy_primary_worktree(root)
    # A separate-git-dir primary reports its git directory here; that is metadata, not a checkout.
    if primary is None or primary == root.resolve() or not (primary / ".git").exists():
        return None
    return primary


def config_root() -> Path:
    """Return the checkout that owns the development stack's .env and TLS directory."""
    override = os.environ.get("PEPPY_DEV_CONFIG_ROOT")
    if override:
        root = Path(override).resolve()
        if not has_dotenv(root):
            raise TlsError(f"PEPPY_DEV_CONFIG_ROOT has no .env: {root}")
        return root
    if has_dotenv(ROOT):
        return ROOT
    if not is_linked_worktree(ROOT):
        return ROOT
    primary = primary_worktree(ROOT)
    if primary is None:
        raise TlsError(
            f"{ROOT} is a linked worktree without .env, and its primary checkout cannot be resolved "
            "(separate git directory, bare repository, or missing worktree registration); "
            "set PEPPY_DEV_CONFIG_ROOT to the checkout whose .env owns the development stack"
        )
    return primary if has_dotenv(primary) else ROOT


def default_dotenv() -> Path:
    return config_root() / ".env"


def tls_directory() -> Path:
    if "PEPPY_DEV_TLS_DIR" in os.environ:
        return Path(os.environ["PEPPY_DEV_TLS_DIR"]).resolve()
    return (config_root() / ".opencode/dev/tls").resolve()


def run_openssl(arguments: list[str], error: str) -> None:
    result = subprocess.run(["openssl", *arguments], text=True, capture_output=True)
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise TlsError(f"{error}: {detail}")


def public_key(path: Path, certificate: bool) -> bytes:
    arguments = ["x509", "-in", str(path), "-pubkey", "-noout"] if certificate else ["pkey", "-in", str(path), "-pubout"]
    result = subprocess.run(["openssl", *arguments], capture_output=True)
    if result.returncode:
        raise TlsError("existing CA is malformed")
    return result.stdout


def validate_existing_ca(directory: Path) -> None:
    certificate, key = directory / "ca.pem", directory / "ca-key.pem"
    if not certificate.exists() and not key.exists():
        return
    if not certificate.is_file() or not key.is_file():
        raise TlsError(f"existing CA is incomplete; remove {directory} and rebuild local development TLS")
    run_openssl(["x509", "-in", str(certificate), "-noout", "-checkend", str(365 * 24 * 60 * 60)], "existing CA is malformed, expired, or expires within one leaf lifetime")
    text = subprocess.run(["openssl", "x509", "-in", str(certificate), "-noout", "-text"], text=True, capture_output=True, check=True).stdout
    if "CA:TRUE" not in text or public_key(certificate, True) != public_key(key, False):
        raise TlsError("existing CA is not a valid certificate authority")


def publish(source: Path, target: Path, mode: int) -> None:
    source.chmod(mode)
    source.replace(target)


def write_ca(directory: Path) -> None:
    key, certificate = directory / "ca-key.pem", directory / "ca.pem"
    with tempfile.TemporaryDirectory(dir=directory) as temporary:
        staged_key, staged_certificate = Path(temporary) / key.name, Path(temporary) / certificate.name
        run_openssl(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:4096", "-out", str(staged_key)], "could not generate local CA key")
        run_openssl(["req", "-x509", "-new", "-key", str(staged_key), "-sha256", "-days", "3650", "-subj", "/CN=Peppy Local Development CA", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-addext", "subjectKeyIdentifier=hash", "-out", str(staged_certificate)], "could not generate local CA")
        publish(staged_key, key, 0o600)
        publish(staged_certificate, certificate, 0o644)


def leaf_is_current(directory: Path) -> bool:
    certificate, key, ca = directory / "server.pem", directory / "server-key.pem", directory / "ca.pem"
    if not certificate.is_file() or not key.is_file():
        return False
    result = subprocess.run(["openssl", "verify", "-CAfile", str(ca), str(certificate)], text=True, capture_output=True)
    if result.returncode:
        return False
    expires = subprocess.run(["openssl", "x509", "-in", str(certificate), "-noout", "-checkend", str(LEAF_RENEWAL_SECONDS)], capture_output=True)
    if expires.returncode or public_key(certificate, True) != public_key(key, False):
        return False
    details = subprocess.run(["openssl", "x509", "-in", str(certificate), "-noout", "-text"], text=True, capture_output=True).stdout
    san_line = re.search(r"X509v3 Subject Alternative Name:\s*\n\s*([^\n]*)", details)
    required_sans = {"DNS:localhost", "IP Address:127.0.0.1", "IP Address:0:0:0:0:0:0:0:1", "IP Address:10.0.2.2"}
    actual_sans = set(re.findall(r"DNS:[^,\s]+|IP Address:[^,\s]+", san_line.group(1) if san_line else ""))
    return "CA:FALSE" in details and "Subject Key Identifier" in details and "Authority Key Identifier" in details and required_sans <= actual_sans


def write_leaf(directory: Path) -> None:
    key, request, certificate = directory / "server-key.pem", directory / "server.csr", directory / "server.pem"
    temporary = Path(tempfile.mkdtemp(dir=directory))
    staged_key, staged_request, staged_certificate = temporary / key.name, temporary / request.name, temporary / certificate.name
    config = temporary / "server-openssl.cnf"
    ca_details = subprocess.run(["openssl", "x509", "-in", str(directory / "ca.pem"), "-noout", "-text"], text=True, capture_output=True, check=True).stdout
    authority_key_identifier = "keyid:always" if "Subject Key Identifier" in ca_details else "issuer"
    config.write_text(f"""[req]
distinguished_name = subject
prompt = no
req_extensions = request_extensions
[subject]
CN = localhost
[request_extensions]
subjectAltName = @sans
[extensions]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectKeyIdentifier = hash
authorityKeyIdentifier = {authority_key_identifier}
subjectAltName = @sans
[sans]
DNS.1 = localhost
IP.1 = 127.0.0.1
IP.2 = ::1
IP.3 = 10.0.2.2
""")
    try:
        run_openssl(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(staged_key)], "could not generate local TLS key")
        run_openssl(["req", "-new", "-key", str(staged_key), "-config", str(config), "-out", str(staged_request)], "could not generate local TLS request")
        serial = temporary / "ca.srl"
        run_openssl(["x509", "-req", "-in", str(staged_request), "-CA", str(directory / "ca.pem"), "-CAkey", str(directory / "ca-key.pem"), "-CAcreateserial", "-CAserial", str(serial), "-days", "365", "-sha256", "-extfile", str(config), "-extensions", "extensions", "-out", str(staged_certificate)], "could not sign local TLS certificate")
        publish(staged_key, key, 0o600)
        publish(staged_certificate, certificate, 0o644)
    finally:
        for path in temporary.iterdir():
            path.unlink(missing_ok=True)
        temporary.rmdir()


def secure_artifacts(directory: Path) -> None:
    directory.chmod(0o700)
    for filename in TLS_FILENAMES:
        path = directory / filename
        if path.exists():
            path.chmod(0o600 if filename in PRIVATE_FILENAMES else 0o644)


@contextlib.contextmanager
def preparation_lock(directory: Path):
    lock_path = directory / ".prepare.lock"
    with lock_path.open("a+") as lock:
        lock_path.chmod(0o600)
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def prepare(dotenv: Path) -> None:
    if not configured_debug(dotenv):
        return
    directory = tls_directory()
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory.chmod(0o700)
    with preparation_lock(directory):
        validate_existing_ca(directory)
        if not (directory / "ca.pem").exists():
            write_ca(directory)
        if not leaf_is_current(directory):
            write_leaf(directory)
        secure_artifacts(directory)


def compose_arguments(dotenv: Path, arguments: list[str], teardown: bool) -> None:
    enabled = configured_debug(dotenv)
    source = Path(os.environ.get("PEPPY_SOURCE_TREE", ROOT)).resolve()
    environment = os.environ.copy()
    environment["DEV_UID"] = str(os.getuid())
    environment["DEV_GID"] = str(os.getgid())
    environment["PEPPY_SOURCE_TREE"] = str(source)
    environment["DEBUG"] = "true" if enabled else "false"
    environment["PEPPY_SERVER_IMAGE"] = environment.get("PEPPY_SERVER_IMAGE") or "peppy-server:local"
    values = dotenv_values(dotenv)
    port = development_port(values)
    for name in ("PUBLIC_API_URL",):
        value = environment.get(name, values.get(name))
        if value:
            environment[name] = rewrite_loopback_url(value, port, enabled)
    files = [ROOT / "docker-compose.yml", ROOT / "infra/compose/compose.dev.yml"]
    if enabled or teardown:
        files.append(ROOT / "infra/compose/compose.dev-tls.yml")
        environment["PEPPY_DEV_TLS_DIR"] = str(tls_directory())
    if enabled and arguments and arguments[0] == "up" and "dev" in arguments and "caddy" not in arguments:
        arguments = [*arguments, "caddy"]
    command = ["docker", "compose", "--env-file", str(dotenv)]
    for compose_file in files:
        command.extend(("-f", str(compose_file)))
    os.execvpe(command[0], [*command, *arguments], environment)
    raise AssertionError("docker compose did not execute")


def certificate_der(pem: bytes, name: str) -> bytes:
    try:
        text = pem.decode("ascii")
    except UnicodeDecodeError as error:
        raise TlsError(f"{name} is not a PEM certificate") from error
    if text.count(PEM_CERTIFICATE_BEGIN) != 1:
        raise TlsError(f"{name} must contain exactly one PEM certificate")
    try:
        return ssl.PEM_cert_to_DER_cert(text)
    except ValueError as error:
        raise TlsError(f"{name} is not a PEM certificate") from error


def verify_apk(apk: Path) -> None:
    """Require an APK built with DEBUG=true to embed the current development CA."""
    if not configured_debug(default_dotenv()):
        return
    if not apk.is_file():
        raise TlsError(f"APK is missing: {apk}; rebuild it (Android: Rebuild and open)")
    try:
        with zipfile.ZipFile(apk) as archive:
            try:
                entry = archive.getinfo(APK_CA_RESOURCE)
            except KeyError as error:
                raise TlsError(f"{apk} does not contain {APK_CA_RESOURCE}; rebuild it (Android: Rebuild and open)") from error
            if entry.file_size > MAX_APK_CA_BYTES:
                raise TlsError(f"{apk}:{APK_CA_RESOURCE} is too large to be a CA certificate")
            embedded = archive.read(entry)
    except TlsError:
        raise
    except (zipfile.BadZipFile, RuntimeError, NotImplementedError, EOFError, zlib.error) as error:
        raise TlsError(f"{apk} is not a valid APK archive") from error
    ca = tls_directory() / "ca.pem"
    if not ca.is_file():
        raise TlsError(f"development TLS CA is missing at {ca}; run 'python3 infra/dev/dev_tls.py prepare' first")
    if certificate_der(embedded, f"{apk}:{APK_CA_RESOURCE}") != certificate_der(ca.read_bytes(), str(ca)):
        raise TlsError(f"{apk} does not trust the current development CA; rebuild it (Android: Rebuild and open)")


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("enabled", "prepare"):
        subparser = subparsers.add_parser(command)
        subparser.add_argument("dotenv", nargs="?", type=Path)
    subparsers.add_parser("ca-path")
    subparsers.add_parser("dotenv-path")
    verify = subparsers.add_parser("verify-apk")
    verify.add_argument("apk", type=Path)
    compose = subparsers.add_parser("compose")
    compose.add_argument("--teardown", action="store_true")
    arguments, _ = parser.parse_known_args()
    try:
        if arguments.command == "enabled":
            print("true" if configured_debug(arguments.dotenv or default_dotenv()) else "false")
        elif arguments.command == "prepare":
            prepare(arguments.dotenv or default_dotenv())
        elif arguments.command == "ca-path":
            print(tls_directory() / "ca.pem")
        elif arguments.command == "dotenv-path":
            print(default_dotenv())
        elif arguments.command == "verify-apk":
            verify_apk(arguments.apk)
        else:
            command_arguments = sys.argv[sys.argv.index("compose") + 1:]
            if arguments.teardown:
                command_arguments.remove("--teardown")
            compose_arguments(default_dotenv(), command_arguments, arguments.teardown)
    except (OSError, TlsError, ValueError) as error:
        print(f"development TLS: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
