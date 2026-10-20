"""Private-keychain support for post-bundle Developer ID DMG signing."""

from __future__ import annotations

import base64
import binascii
import contextlib
import os
import re
import secrets
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator


class SigningError(RuntimeError):
    """A signing prerequisite could not be completed safely."""


@dataclass(frozen=True)
class SigningIdentity:
    """The identity and optional private keychain for a codesign invocation."""

    identity: str
    keychain: Path | None
    certificate: Path | None = None


def security_run(*command: str, capture: bool = False) -> str:
    """Run security without leaking command arguments or command output on failure."""
    completed = subprocess.run(
        ("security", *command),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode:
        raise SigningError(f"security {command[0]} failed")
    return completed.stdout if capture else ""


def find_identity(identity: str, keychain: Path | None = None) -> bool:
    """Match an exact identity display name or SHA-1 fingerprint."""
    command = ["find-identity", "-v", "-p", "codesigning"]
    if keychain is not None:
        command.append(str(keychain))
    output = security_run(*command, capture=True)
    for line in output.splitlines():
        match = re.match(r'\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"(.*)"', line)
        if match and identity in (match.group(1), match.group(2)):
            return True
    return False


def certificate_environment() -> tuple[bytes, str]:
    """Decode the existing Tauri certificate inputs without exposing their values."""
    encoded = os.environ.get("APPLE_CERTIFICATE", "")
    password = os.environ.get("APPLE_CERTIFICATE_PASSWORD", "")
    if not encoded or not password:
        raise SigningError("Developer ID DMG repair requires an available identity or Tauri certificate inputs")
    try:
        certificate = base64.b64decode(encoded, validate=True)
    except (ValueError, binascii.Error) as error:
        raise SigningError("APPLE_CERTIFICATE is not valid base64") from error
    if not certificate:
        raise SigningError("APPLE_CERTIFICATE is empty")
    return certificate, password


def remove_owned_keychain_from_search_list(keychain: Path) -> None:
    """Remove only this helper's keychain if security added it to the user list."""
    entries = re.findall(r'"([^"]+)"', security_run("list-keychains", "-d", "user", capture=True))
    owned = str(keychain)
    if owned not in entries:
        return
    security_run("list-keychains", "-d", "user", "-s", *(entry for entry in entries if entry != owned))


def create_private_keychain(identity: str) -> tuple[SigningIdentity, Path]:
    """Import the caller's existing certificate into a 0700 private directory."""
    certificate, certificate_password = certificate_environment()
    temporary = Path(tempfile.mkdtemp(prefix="peppy-dmg-signing-"))
    keychain = temporary / "repair.keychain-db"
    try:
        os.chmod(temporary, 0o700)
        certificate_path = temporary / "certificate.p12"
        certificate_path.write_bytes(certificate)
        os.chmod(certificate_path, 0o600)
        keychain_password = secrets.token_urlsafe(32)
        security_run("create-keychain", "-p", keychain_password, str(keychain))
        security_run("unlock-keychain", "-p", keychain_password, str(keychain))
        security_run(
            "import",
            str(certificate_path),
            "-k",
            str(keychain),
            "-P",
            certificate_password,
            "-f",
            "pkcs12",
            "-t",
            "agg",
            "-T",
            "/usr/bin/codesign",
        )
        security_run("set-keychain-settings", "-lut", "21600", str(keychain))
        security_run("set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-k", keychain_password, str(keychain))
        if not find_identity(identity, keychain):
            raise SigningError("Developer ID signing identity is unavailable in the private repair keychain")
    except BaseException:
        _cleanup_keychain(keychain, temporary)
        raise
    return SigningIdentity(identity, keychain, certificate_path), temporary


def _cleanup_keychain(keychain: Path, temporary: Path) -> None:
    try:
        remove_owned_keychain_from_search_list(keychain)
    except SigningError:
        pass
    try:
        security_run("delete-keychain", str(keychain))
    except SigningError:
        pass
    shutil.rmtree(temporary, ignore_errors=True)


@contextlib.contextmanager
def developer_id_signing(identity: str | None) -> Iterator[SigningIdentity]:
    """Yield an existing identity or a private temporary-keychain identity."""
    if not identity:
        raise SigningError("Developer ID DMG repair requires APPLE_SIGNING_IDENTITY")
    if find_identity(identity):
        yield SigningIdentity(identity, None)
        return
    signing, temporary = create_private_keychain(identity)
    try:
        yield signing
    finally:
        _cleanup_keychain(signing.keychain, temporary)
