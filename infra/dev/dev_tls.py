#!/usr/bin/env python3
"""Prepare and apply optional local development TLS."""

import argparse
import contextlib
import fcntl
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    from .dev_port import development_port, development_tls_enabled, dotenv_values, rewrite_loopback_url
except ImportError:
    from dev_port import development_port, development_tls_enabled, dotenv_values, rewrite_loopback_url


ROOT = Path(__file__).resolve().parents[2]
TLS_FILENAMES = ("ca.pem", "ca-key.pem", "server.pem", "server-key.pem")
PRIVATE_FILENAMES = ("ca-key.pem", "server-key.pem")
LEAF_RENEWAL_SECONDS = 30 * 24 * 60 * 60


class TlsError(RuntimeError):
    """Raised when local TLS cannot be safely prepared."""


def configured_debug(dotenv: Path) -> bool:
    try:
        return development_tls_enabled(dotenv_values(dotenv))
    except ValueError as error:
        raise TlsError(str(error)) from error


def tls_directory() -> Path:
    return Path(os.environ.get("PEPPY_DEV_TLS_DIR", ROOT / ".opencode/dev/tls")).resolve()


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
    values = dotenv_values(dotenv)
    port = development_port(values)
    for name in ("PUBLIC_API_URL", "PUBLIC_ATTACHMENT_URL"):
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


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("enabled", "prepare"):
        subparser = subparsers.add_parser(command)
        subparser.add_argument("dotenv", nargs="?", default=ROOT / ".env", type=Path)
    subparsers.add_parser("ca-path")
    compose = subparsers.add_parser("compose")
    compose.add_argument("--teardown", action="store_true")
    arguments, _ = parser.parse_known_args()
    try:
        if arguments.command == "enabled":
            print("true" if configured_debug(arguments.dotenv) else "false")
        elif arguments.command == "prepare":
            prepare(arguments.dotenv)
        elif arguments.command == "ca-path":
            print(tls_directory() / "ca.pem")
        else:
            command_arguments = sys.argv[sys.argv.index("compose") + 1:]
            if arguments.teardown:
                command_arguments.remove("--teardown")
            compose_arguments(ROOT / ".env", command_arguments, arguments.teardown)
    except (OSError, TlsError, ValueError) as error:
        print(f"development TLS: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
