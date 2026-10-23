#!/usr/bin/env python3
"""Run the disposable, simulated gateway exercise without touching dev data."""
import argparse
import json
import os
import selectors
import secrets
import signal
import ssl
import stat
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Optional
from urllib.error import URLError
from urllib.request import Request, urlopen

try:
    from infra.dev import dev_tls
except ModuleNotFoundError:
    import dev_tls


ROOT = Path(__file__).resolve().parents[2]
RUNTIME_TIMEOUT = 60
COLD_BUILD_TIMEOUT = 600


def private_file(path: Path, content: str) -> Path:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as output:
        output.write(content)
    return path


def make_run_directory(base: Path) -> Path:
    base.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(base, 0o700)
    run = base / f"gateway-demo-{uuid.uuid4().hex}"
    run.mkdir(mode=0o700)
    return run


def synthetic_values():
    token = uuid.uuid4().hex
    values = {
        "PEPPY_ENV": "development",
        "POSTGRES_DB": "peppy_demo",
        "POSTGRES_USER": "peppy_demo",
        "POSTGRES_PASSWORD": f"synthetic-{token}",
        "S3_ACCESS_KEY": f"synthetic-{token[:16]}",
        "S3_SECRET_KEY": f"synthetic-{uuid.uuid4().hex}",
        "S3_BUCKET": "peppy-private",
        "PUBLIC_API_URL": "https://127.0.0.1",
        "PUBLIC_ATTACHMENT_URL": "https://127.0.0.1",
        "BIND_ADDR": "127.0.0.1:8080",
        "PEPPY_ISOLATED_DEMO": "1",
        "PEPPY_SETUP_TOKEN": f"synthetic-{uuid.uuid4().hex}",
        "PEPPY_DEMO_ADMIN_PASSWORD": f"synthetic-{uuid.uuid4().hex}",
    }
    return values


def synthetic_simulator_passphrase() -> str:
    labels = ("alpha", "bravo", "charlie", "delta", "echo", "foxtrot")
    return " ".join(f"{label}-{secrets.token_urlsafe(12)}" for label in labels)


def write_environment(run: Path) -> Path:
    values = synthetic_values()
    return private_file(run / "demo.env", "".join(f"{key}={value}\n" for key, value in values.items()))


def sanitized_environment(environment, values):
    return {key: value for key, value in environment.items() if key not in values and not key.startswith("COMPOSE_")}


def timeout_from_environment(name, default):
    value = os.environ.get(name, str(default))
    try:
        timeout = int(value)
    except ValueError:
        raise SystemExit(f"{name} must be a whole number of seconds")
    if timeout <= 0:
        raise SystemExit(f"{name} must be greater than zero")
    return timeout


def expected_origin(origin: object, configured_origin: str) -> bool:
    return isinstance(origin, str) and origin == configured_origin


def credential_metadata(path: Path, configured_origin: str):
    metadata = {
        "path": "gateway/credentials.json",
        "exists": False,
        "type": "missing",
        "mode": None,
        "uid": None,
        "gid": None,
        "bytes": None,
        "json_valid": False,
        "version": None,
        "version_valid": False,
        "nonempty_vault_id": False,
        "nonempty_device_id": False,
        "nonempty_token": False,
        "canonical_origin": False,
    }
    try:
        info = path.stat()
    except OSError:
        return metadata, False
    metadata.update({"exists": True, "mode": format(stat.S_IMODE(info.st_mode), "04o"), "uid": info.st_uid, "gid": info.st_gid, "bytes": info.st_size})
    if stat.S_ISREG(info.st_mode):
        metadata["type"] = "regular"
    elif stat.S_ISDIR(info.st_mode):
        metadata["type"] = "directory"
    else:
        metadata["type"] = "other"
    try:
        state = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError, UnicodeDecodeError):
        return metadata, False
    if not isinstance(state, dict):
        return metadata, False
    metadata["json_valid"] = True
    version = state.get("version")
    metadata["version"] = version if isinstance(version, int) else None
    metadata["version_valid"] = version == 1
    metadata["nonempty_vault_id"] = isinstance(state.get("vaultId"), str) and bool(state["vaultId"])
    metadata["nonempty_device_id"] = isinstance(state.get("deviceId"), str) and bool(state["deviceId"])
    metadata["nonempty_token"] = isinstance(state.get("deviceToken"), str) and bool(state["deviceToken"])
    metadata["canonical_origin"] = expected_origin(state.get("origin"), configured_origin)
    valid = metadata["type"] == "regular" and metadata["mode"] == "0600" and metadata["uid"] == os.getuid() and metadata["gid"] == os.getgid() and metadata["bytes"] > 0 and metadata["json_valid"] and metadata["version_valid"] and metadata["nonempty_vault_id"] and metadata["nonempty_device_id"] and metadata["nonempty_token"] and metadata["canonical_origin"]
    return metadata, valid


class ChildController:
    def __init__(self):
        self.children = []

    def add(self, child: subprocess.Popen) -> subprocess.Popen:
        self.children.append(child)
        return child

    def cleanup(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
        for child in self.children:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)


class ProcessOutput:
    """Binary pipe reader: partial lines and EOF never block the controller."""
    def __init__(self, process: subprocess.Popen, log_path: Path):
        self.process = process
        self.log = open(log_path, "ab", buffering=0)
        self.selector = selectors.DefaultSelector()
        self.buffers = {}
        for stream, name in ((process.stdout, "stdout"), (process.stderr, "stderr")):
            if stream is not None:
                os.set_blocking(stream.fileno(), False)
                self.selector.register(stream, selectors.EVENT_READ, name)
                self.buffers[name] = b""

    def _read(self, timeout: float):
        for key, _ in self.selector.select(timeout):
            data = os.read(key.fileobj.fileno(), 65536)
            if not data:
                self.selector.unregister(key.fileobj)
                continue
            self.log.write(key.data.encode() + b": " + data)
            self.buffers[key.data] += data

    def checkpoint(self, source: str) -> int:
        return len(self.buffers.get(source, b""))

    def wait_for(self, marker: str, source: str, timeout: float, after=0) -> bool:
        needle = marker.encode()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.buffers.get(source, b"")[after:]:
                return True
            self._read(max(0, deadline - time.monotonic()))
            if self.process.poll() is not None and not self.selector.get_map():
                break
        return needle in self.buffers.get(source, b"")[after:]

    def wait_for_exit(self, timeout: float) -> Optional[int]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._read(min(0.2, max(0, deadline - time.monotonic())))
            if self.process.poll() is not None:
                while self.selector.get_map():
                    self._read(0)
                return self.process.returncode
        return None

    def close(self):
        self.selector.close()
        self.log.close()
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()


def checked(command, *, env, timeout=RUNTIME_TIMEOUT, **kwargs):
    return subprocess.run(command, env=env, timeout=timeout, check=True, **kwargs)


def tls_context(certificate_path: Path) -> ssl.SSLContext:
    return ssl.create_default_context(cafile=str(certificate_path))


def wait_for_ready(origin: str, context: ssl.SSLContext, timeout: float = RUNTIME_TIMEOUT) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urlopen(origin + "/readyz", timeout=2, context=context):
                return
        except URLError as error:
            if isinstance(error.reason, ssl.SSLCertVerificationError):
                raise RuntimeError("demo TLS certificate verification failed") from error
            time.sleep(0.2)
    raise RuntimeError("demo server did not become ready")


def replay_events(credential: dict, configured_origin: str, context: ssl.SSLContext):
    if not expected_origin(credential.get("origin"), configured_origin):
        raise RuntimeError("demo credential origin does not match expected public API URL")
    request = Request(configured_origin + "/v1/events?after=0&limit=200", headers={"Authorization": "Bearer " + credential["deviceToken"]})
    with urlopen(request, timeout=10, context=context) as response:
        return json.load(response)["events"]


def prepare_demo_tls(dotenv: Path, tls_directory: Path) -> None:
    environment = os.environ | {"DEBUG": "true", "PEPPY_DEV_TLS_DIR": str(tls_directory)}
    subprocess.run([sys.executable, str(ROOT / "infra/dev/dev_tls.py"), "prepare", str(dotenv)], env=environment, check=True)


def run_inside_container() -> None:
    if os.environ.get("PEPPY_ISOLATED_DEMO") != "1":
        raise SystemExit("demo requires isolated dev-demo context")
    run = Path(os.environ.get("PEPPY_DEMO_RUN", "/artifacts/gateway-demo"))
    run.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(run, 0o700)
    gateway, desktop = run / "gateway", run / "desktop"
    gateway.mkdir(mode=0o700, exist_ok=True)
    desktop.mkdir(mode=0o700, exist_ok=True)
    env = os.environ.copy()
    context = tls_context(Path(env["PEPPY_DEV_TLS_CERT"]))
    server_env = {
        key: value
        for key, value in env.items()
        if key not in {"PEPPY_SIMULATOR_PASSPHRASE", "PEPPY_SETUP_TOKEN"}
    }
    server_env["PEPPY_SETUP_TOKEN"] = env["PEPPY_SETUP_TOKEN"]
    admission_env = server_env | {
        "PEPPY_DEMO_SETUP_TOKEN": env["PEPPY_SETUP_TOKEN"],
        "PEPPY_SIMULATOR_PASSPHRASE": env["PEPPY_SIMULATOR_PASSPHRASE"],
    }
    controller = ChildController()
    target_dir = Path(env.get("CARGO_TARGET_DIR", "target")) / "debug"
    server_binary = str(target_dir / "peppy-server")
    simulator_binary = str(target_dir / "peppy-gateway-simulator")
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    old_handlers = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        cold_build_timeout = timeout_from_environment("PEPPY_DEMO_CARGO_TIMEOUT", COLD_BUILD_TIMEOUT)
        checked(["cargo", "build", "--locked", "--quiet", "-p", "peppy-server", "-p", "peppy-gateway-simulator"], env=env, timeout=cold_build_timeout)
        checked([server_binary, "migrate"], env=server_env)
        server_log = os.fdopen(os.open(run / "server.log", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb")
        server = controller.add(subprocess.Popen([server_binary], env=server_env, stdout=server_log, stderr=subprocess.STDOUT))
        wait_for_ready(env["PUBLIC_API_URL"], context)
        checked([simulator_binary, "claim-owner", str(run / "owner")], env=admission_env)
        checked([simulator_binary, "admit-gateway", str(gateway)], env=admission_env)
        checked([simulator_binary, "admit-device", str(desktop)], env=admission_env)
        credentials_metadata, credentials_valid = credential_metadata(gateway / "credentials.json", env["PUBLIC_API_URL"])
        private_file(run / "credential-metadata.json", json.dumps(credentials_metadata, sort_keys=True) + "\n")
        if not credentials_valid:
            raise RuntimeError("gateway credential metadata validation failed; inspect private credential-metadata.json")
        credential = json.loads((desktop / "desktop-import.json").read_text())
        simulator = controller.add(subprocess.Popen([simulator_binary, "run", str(gateway)], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE))
        output = ProcessOutput(simulator, run / "simulator.log")
        try:
            if not output.wait_for("gateway running", "stdout", RUNTIME_TIMEOUT):
                raise RuntimeError("simulator did not start")
            if not output.wait_for("phase=media", "stderr", RUNTIME_TIMEOUT):
                raise RuntimeError("simulator did not reach media phase")
            baseline_events = len(replay_events(credential, env["PUBLIC_API_URL"], context))
            sync_checkpoint = output.checkpoint("stderr")
            simulator.stdin.write(b'{"type":"incoming","senderAddress":"+15555550123","body":"synthetic runnable demo message","providerMessageId":"demo-incoming-1"}\n')
            simulator.stdin.flush()
            if not output.wait_for("phase=sync", "stderr", RUNTIME_TIMEOUT, after=sync_checkpoint):
                raise RuntimeError("simulator did not sync")
            media_checkpoint = output.checkpoint("stderr")
            if not output.wait_for("phase=media", "stderr", RUNTIME_TIMEOUT, after=media_checkpoint):
                raise RuntimeError("simulator did not return to media phase")
            final_sync_checkpoint = output.checkpoint("stderr")
            if not output.wait_for("phase=sync", "stderr", RUNTIME_TIMEOUT, after=final_sync_checkpoint):
                raise RuntimeError("simulator did not complete post-media sync")
            simulator.stdin.write(b'{"type":"quit"}\n')
            simulator.stdin.flush()
            exit_code = output.wait_for_exit(RUNTIME_TIMEOUT)
            if exit_code != 0:
                raise RuntimeError(f"simulator exited {exit_code}; inspect private log {run / 'simulator.log'}")
        finally:
            output.close()
        if len(replay_events(credential, env["PUBLIC_API_URL"], context)) <= baseline_events:
            raise RuntimeError("real API replay did not contain the new synthetic simulator event")
        database = gateway / "gateway.sqlcipher"
        if not database.is_file() or database.stat().st_size == 0:
            raise RuntimeError("client SQLCipher store was not persisted")
        checked([simulator_binary, "run", str(gateway)], env=env, input=b'{"type":"quit"}\n', stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        print("simulated gateway demo verified; no carrier or production behavior was exercised")
    finally:
        controller.cleanup()
        if 'server_log' in locals():
            server_log.close()
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)


def run_host_demo() -> None:
    os.umask(0o077)
    run = make_run_directory(ROOT / ".opencode" / "dev" / "artifacts")
    values = synthetic_values()
    env_file = private_file(run / "demo.env", "".join(f"{key}={value}\n" for key, value in values.items()))
    tls_dotenv = private_file(run / "demo-tls.env", "DEBUG=true\n")
    project = "peppy-demo-" + uuid.uuid4().hex[:12]
    compose = ["docker", "compose", "--project-name", project, "--env-file", str(env_file), "-f", "docker-compose.yml", "-f", "infra/compose/compose.dev.yml", "-f", "infra/compose/compose.demo-tls.yml"]
    environment = sanitized_environment(os.environ, values)
    environment.update({"DEV_UID": str(os.getuid()), "DEV_GID": str(os.getgid()), "PEPPY_DEMO_RUN": "/artifacts/" + run.name, "PEPPY_SIMULATOR_PASSPHRASE": synthetic_simulator_passphrase(), "PEPPY_DEV_TLS_DIR": str(dev_tls.tls_directory())})
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    old_handlers = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
    try:
        prepare_demo_tls(tls_dotenv, Path(environment["PEPPY_DEV_TLS_DIR"]))
        for cache in ("cargo", "pnpm", "target"):
            checked(["docker", "volume", "create", f"peppy-dev-{cache}-{os.getuid()}-{os.getgid()}"], env=environment, timeout=RUNTIME_TIMEOUT, stdout=subprocess.DEVNULL, cwd=ROOT)
        build_timeout = timeout_from_environment("PEPPY_DEMO_BUILD_TIMEOUT", COLD_BUILD_TIMEOUT)
        cargo_timeout = timeout_from_environment("PEPPY_DEMO_CARGO_TIMEOUT", COLD_BUILD_TIMEOUT)
        environment["PEPPY_DEMO_CARGO_TIMEOUT"] = str(cargo_timeout)
        checked(compose + ["build", "dev"], env=environment, timeout=build_timeout, cwd=ROOT)
        checked(compose + ["up", "--detach", "--wait", "postgres", "seaweedfs"], env=environment, timeout=2 * RUNTIME_TIMEOUT, cwd=ROOT)
        checked(compose + ["up", "--abort-on-container-exit", "--exit-code-from", "dev", "dev", "caddy"], env=environment, timeout=cargo_timeout + 8 * RUNTIME_TIMEOUT, cwd=ROOT)
    finally:
        subprocess.run(compose + ["down", "--volumes", "--remove-orphans"], env=environment, timeout=RUNTIME_TIMEOUT, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=ROOT)
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--inside-container", action="store_true")
    arguments = parser.parse_args()
    if arguments.inside_container:
        run_inside_container()
    else:
        run_host_demo()
