#!/usr/bin/env python3
"""Resolve the local development port without evaluating dotenv contents."""

import argparse
import os
import re
import secrets
import sys
from pathlib import Path
from typing import Optional
from urllib.parse import urlparse


DEFAULT_PORT = 7000
LOOPBACK_HOSTS = {"localhost", "127.0.0.1", "::1"}
DOTENV_ASSIGNMENT = re.compile(r"(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)")
INTERPOLATION = re.compile(r"\$(?:\{([^}]+)\}|([A-Za-z_][A-Za-z0-9_]*))")
DEFAULT_URL_PORTS = {"http": 80, "https": 443}


def dotenv_values(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.exists():
        return values
    for line_number, raw_line in enumerate(path.read_text().splitlines(), start=1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        match = DOTENV_ASSIGNMENT.fullmatch(line)
        if match is None:
            raise ValueError(f"{path}:{line_number}: expected KEY=value")
        key, raw_value = match.groups()
        value, interpolate = dotenv_value(path, line_number, raw_value)
        values[key] = interpolate_value(value, values) if interpolate else value
    return values


def dotenv_value(path: Path, line_number: int, raw_value: str) -> tuple[str, bool]:
    value = raw_value.strip()
    if not value:
        return "", True
    if value[0] == "'":
        match = re.fullmatch(r"'(.*)'(?:\s+#.*)?", value)
        if match is None:
            raise ValueError(f"{path}:{line_number}: unterminated single-quoted value")
        return match.group(1), False
    if value[0] == '"':
        match = re.fullmatch(r'"(.*)"(?:\s+#.*)?', value)
        if match is None:
            raise ValueError(f"{path}:{line_number}: unterminated double-quoted value")
        return match.group(1), True
    return re.split(r"\s+#", value, maxsplit=1)[0].rstrip(), True


def interpolate_value(value: str, values: dict[str, str]) -> str:
    def replace(match: re.Match[str]) -> str:
        expression = match.group(1)
        if expression is None:
            return interpolation_value(match.group(2), None, values) or ""
        default_match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)(:-|-)(.*)", expression)
        if default_match:
            name, operator, default = default_match.groups()
            resolved = interpolation_value(name, None, values)
            if (operator == ":-" and resolved in {None, ""}) or (operator == "-" and resolved is None):
                return default
            return resolved or ""
        if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", expression):
            return interpolation_value(expression, None, values) or ""
        raise ValueError(f"unsupported dotenv interpolation '${{{expression}}}'; use $NAME, ${{NAME}}, ${{NAME:-default}}, or ${{NAME-default}}")

    return INTERPOLATION.sub(replace, value)


def interpolation_value(name: Optional[str], default: Optional[str], values: dict[str, str]) -> Optional[str]:
    if name is None:
        return default or ""
    return os.environ.get(name, values.get(name, default))


def configured_value(name: str, values: dict[str, str]) -> Optional[str]:
    return os.environ[name] if name in os.environ else values.get(name)


def development_port(values: dict[str, str]) -> int:
    value = configured_value("API_HOST_PORT", values)
    if value is None:
        return DEFAULT_PORT
    try:
        port = int(value, 10)
    except ValueError as error:
        raise ValueError("API_HOST_PORT must be an integer between 1 and 65535") from error
    if not 1 <= port <= 65535:
        raise ValueError("API_HOST_PORT must be an integer between 1 and 65535")
    return port


def ensure_loopback_urls_match(port: int, values: dict[str, str]) -> None:
    for name in ("PUBLIC_API_URL", "PUBLIC_ATTACHMENT_URL"):
        value = configured_value(name, values)
        if not value:
            continue
        parsed = urlparse(value)
        if parsed.hostname not in LOOPBACK_HOSTS:
            continue
        try:
            configured_port = parsed.port or DEFAULT_URL_PORTS.get(parsed.scheme)
        except ValueError as error:
            raise ValueError(f"{name} has an invalid port") from error
        if configured_port is None:
            raise ValueError(f"{name} must use an http or https URL with an explicit port")
        if configured_port != port:
            raise ValueError(
                f"{name} uses loopback port {configured_port}; set it to {port} or choose a matching API_HOST_PORT"
            )


def resolved_values(dotenv: Path) -> tuple[int, dict[str, str]]:
    values = dotenv_values(dotenv)
    port = development_port(values)
    ensure_loopback_urls_match(port, values)
    web_ui_enabled(values)
    return port, values


def web_ui_enabled(values: dict[str, str]) -> bool:
    value = configured_value("WEB_UI_ENABLED", values)
    if value is None or value == "" or value in {"true", "1"}:
        return True
    if value in {"false", "0"}:
        return False
    raise ValueError("WEB_UI_ENABLED must be true, false, 1, or 0")


def public_api_origin(port: int, values: dict[str, str]) -> str:
    value = configured_value("PUBLIC_API_URL", values) or f"http://127.0.0.1:{port}"
    return configured_origin(value, "PUBLIC_API_URL")


def normalized_origin(value: str, name: str) -> str:
    parsed = urlparse(value)
    if parsed.path not in {"", "/"} or parsed.params or parsed.query or parsed.fragment:
        raise ValueError(f"{name} must be an http or https origin without credentials, path, query, or fragment")
    return serializable_origin(parsed, name)


def configured_origin(value: str, name: str) -> str:
    return serializable_origin(urlparse(value), name)


def serializable_origin(parsed, name: str) -> str:
    try:
        configured_port = parsed.port or DEFAULT_URL_PORTS.get(parsed.scheme)
    except ValueError as error:
        raise ValueError(f"{name} has an invalid port") from error
    if parsed.scheme not in DEFAULT_URL_PORTS or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError(f"{name} must be an http or https origin without credentials, path, query, or fragment")
    host = parsed.hostname.lower()
    if ":" in host:
        host = f"[{host}]"
    suffix = "" if configured_port == DEFAULT_URL_PORTS[parsed.scheme] else f":{configured_port}"
    return f"{parsed.scheme.lower()}://{host}{suffix}"


def write_setup(dotenv: Path) -> None:
    port, configured = resolved_values(dotenv)
    public_url = f"http://127.0.0.1:{port}"
    values = {
        "PEPPY_ENV": "development",
        "POSTGRES_DB": "peppy",
        "POSTGRES_USER": "peppy",
        "POSTGRES_PASSWORD": "synthetic-" + secrets.token_urlsafe(32),
        "S3_ACCESS_KEY": "synthetic-" + secrets.token_urlsafe(16),
        "S3_SECRET_KEY": "synthetic-" + secrets.token_urlsafe(32),
        "S3_BUCKET": "peppy-private",
        "PEPPY_REPLAY_RETENTION_DAYS": "30",
        "VAULT_ATTACHMENT_QUOTA_BYTES": "536870912",
        "API_HOST_PORT": str(port),
        "PUBLIC_API_URL": configured_value("PUBLIC_API_URL", configured) or public_url,
        "PUBLIC_ATTACHMENT_URL": configured_value("PUBLIC_ATTACHMENT_URL", configured) or public_url,
    }
    descriptor = os.open(dotenv, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as output:
        for key, value in values.items():
            output.write(f"{key}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("port", "smoke-url", "public-api-origin", "normalize-origin", "validate", "web-enabled", "write-setup"))
    parser.add_argument("argument", nargs="?", default=".env")
    arguments = parser.parse_args()
    try:
        if arguments.command == "normalize-origin":
            print(normalized_origin(arguments.argument, "apiOrigin"))
            return 0
        dotenv = Path(arguments.argument)
        if arguments.command == "write-setup":
            write_setup(dotenv)
            return 0
        port, values = resolved_values(dotenv)
        if arguments.command == "public-api-origin":
            print(public_api_origin(port, values))
            return 0
    except ValueError as error:
        print(f"development port: {error}", file=sys.stderr)
        return 1
    if arguments.command == "port":
        print(port)
    elif arguments.command == "smoke-url":
        print(f"http://127.0.0.1:{port}")
    elif arguments.command == "web-enabled":
        print("true" if web_ui_enabled(values) else "false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
