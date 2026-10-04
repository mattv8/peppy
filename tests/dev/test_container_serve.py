import os
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / 'infra/dev/container-run.sh'


class ContainerServeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root / 'source'
        (self.source / 'infra/dev').mkdir(parents=True)
        shutil.copy(ROOT / 'infra/dev/source-excludes.txt', self.source / 'infra/dev/source-excludes.txt')
        (self.source / 'Cargo.toml').write_text('[workspace]\n')
        (self.source / 'payload').write_text('one')
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        if not shutil.which('flock'):
            flock = self.tools / 'flock'
            flock.write_text('#!/bin/sh\nexit 0\n')
            flock.chmod(0o755)
        self.events = self.root / 'events'
        cargo = self.tools / 'cargo'
        cargo.write_text(r'''#!/usr/bin/env bash
set -eu
printf 'build\n' >> "$PEPPY_EVENTS"
target=${CARGO_TARGET_DIR:-target}
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
mkdir -p "$target/debug"
payload=$(cat payload)
cat > "$target/debug/peppy-server" <<EOF
#!/usr/bin/env bash
set -e
printf '%s:%s\n' "\$1" "$payload" >> "\$PEPPY_EVENTS"
[ "\${1}" = migrate ] && [ "\${PEPPY_FAIL_MIGRATE:-}" != 1 ]
[ "\${1}" != serve ] || { [ "\${PEPPY_BLOCK_SERVE:-}" != 1 ] || while :; do sleep 1; done; }
EOF
chmod +x "$target/debug/peppy-server"
[ "${PEPPY_FAIL_BUILD:-}" != 1 ]
''')
        cargo.chmod(0o755)
        inherited = {
            key: value for key, value in os.environ.items()
            if not key.startswith('CARGO_') and not key.startswith('PEPPY_')
            and key not in {'HOME'}
        }
        self.env = inherited | {
            'PEPPY_SOURCE_ROOT': str(self.source), 'PEPPY_WORKSPACE': str(self.root / 'workspace'),
            'CARGO_HOME': str(self.root / 'cargo-home'), 'CARGO_TARGET_DIR': str(self.root / 'target'),
            'PNPM_HOME': str(self.root / 'pnpm-home'), 'COREPACK_HOME': str(self.root / 'corepack-home'),
            'HOME': str(self.root / 'home'), 'PATH': f'{self.tools}{os.pathsep}{os.environ["PATH"]}',
            'PEPPY_EVENTS': str(self.events),
        }
        for key in ('CARGO_HOME', 'PNPM_HOME', 'COREPACK_HOME'):
            Path(self.env[key]).mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def run_runner(self, *args, **kwargs):
        return subprocess.run(['bash', str(RUNNER), *args], cwd=ROOT, text=True, capture_output=True, **kwargs)

    def test_inherited_target_is_not_used_by_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            outside = Path(directory) / 'outside-target'
            (outside / 'debug').mkdir(parents=True)
            sentinel = outside / 'debug/peppy-server'
            sentinel.write_text('do not overwrite')
            self.tearDown()
            with mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': str(outside)}):
                self.setUp()
            result = self.run_runner('build', 'server', env=self.env, timeout=3)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sentinel.read_text(), 'do not overwrite')

    def test_serve_builds_migrates_then_serves_from_relative_target_runtime_copy(self):
        result = self.run_runner('serve', env=self.env | {'CARGO_TARGET_DIR': 'custom-target'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.events.read_text().splitlines(), ['build', 'migrate:one', 'serve:one'])
        self.assertTrue((Path(self.env['HOME']) / '.local/lib/peppy/peppy-server').is_file())

    def test_build_or_migration_failure_never_serves(self):
        for failure in ('PEPPY_FAIL_BUILD', 'PEPPY_FAIL_MIGRATE'):
            with self.subTest(failure=failure):
                self.events.unlink(missing_ok=True)
                result = self.run_runner('serve', env=self.env | {failure: '1'})
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn('serve:', self.events.read_text() if self.events.exists() else '')

    def test_running_copy_survives_sync_and_target_replacement(self):
        process = subprocess.Popen(['bash', str(RUNNER), 'serve'], cwd=ROOT, env=self.env | {'PEPPY_BLOCK_SERVE': '1'}, text=True)
        try:
            self.wait_for_server()
            (self.source / 'payload').write_text('two')
            helper = self.run_runner('build', 'server', env=self.env, timeout=3)
            self.assertEqual(helper.returncode, 0, helper.stderr)
            self.assertIsNone(process.poll())
            self.assertIn('one', (Path(self.env['HOME']) / '.local/lib/peppy/peppy-server').read_text())
            self.assertNotIn('two', (Path(self.env['HOME']) / '.local/lib/peppy/peppy-server').read_text())
        finally:
            process.terminate()
            process.wait(timeout=5)

    def wait_for_server(self):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.events.exists() and 'serve:one' in self.events.read_text():
                return
            time.sleep(.02)
        self.fail('server did not start within 10 seconds')

    @unittest.skipUnless(shutil.which('flock'), 'requires real util-linux flock')
    def test_serve_releases_lock_before_exec(self):
        process = subprocess.Popen(['bash', str(RUNNER), 'serve'], cwd=ROOT, env=self.env | {'PEPPY_BLOCK_SERVE': '1'}, text=True)
        try:
            self.wait_for_server()
            helper = self.run_runner('build', 'server', env=self.env | {'PEPPY_WORKSPACE_LOCK_TIMEOUT': '1'}, timeout=3)
            self.assertEqual(helper.returncode, 0, helper.stderr)
        finally:
            process.terminate()
            process.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
