#!/usr/bin/env ruby
# frozen_string_literal: true

require 'fileutils'
require 'open3'
require 'tmpdir'
require 'yaml'

root = File.expand_path('..', __dir__)
checker = File.join(root, 'scripts', 'check-ci-shards.rb')
workflow = File.join(root, 'workflows', 'ci.yml')
scope = File.join(root, 'scripts', 'ci-scope.sh')
gate = File.join(root, 'scripts', 'ci-gate.sh')
run = ->(workflow_path = workflow, scope_path = scope, gate_path = gate) { Open3.capture3('ruby', checker, '--workflow', workflow_path, '--scope', scope_path, '--gate', gate_path) }
_out, error, status = run.call
abort(error) unless status.success?
mutate = lambda do |name, workflow_change: nil, scope_change: nil, gate_change: nil|
  Dir.mktmpdir("ci-topology-#{name}") do |dir|
    workflow_copy = File.join(dir, 'ci.yml')
    scope_copy = File.join(dir, 'ci-scope.sh')
    gate_copy = File.join(dir, 'ci-gate.sh')
    FileUtils.cp(workflow, workflow_copy)
    FileUtils.cp(scope, scope_copy)
    FileUtils.cp(gate, gate_copy)
    File.write(workflow_copy, workflow_change.call(File.read(workflow_copy))) if workflow_change
    File.write(scope_copy, scope_change.call(File.read(scope_copy))) if scope_change
    File.write(gate_copy, gate_change.call(File.read(gate_copy))) if gate_change
    YAML.safe_load(File.read(workflow_copy), aliases: true)
    _stdout, _stderr, mutation_status = run.call(workflow_copy, scope_copy, gate_copy)
    abort("topology mutation #{name} unexpectedly passed") if mutation_status.success?
  end
end
mutate.call('missing-root-shard', workflow_change: ->(text) { text.sub('shard: [1, 2, 3, 4]', 'shard: [1, 2, 3]') })
mutate.call('unrestricted-reuse', workflow_change: ->(text) { text.sub("if: github.event_name == 'push' && github.ref == 'refs/heads/production'", 'if: true') })
mutate.call('shared-nightly-concurrency', workflow_change: ->(text) { text.sub("ci-${{ github.workflow }}-${{ github.event_name == 'schedule' && 'nightly' || github.event_name == 'workflow_dispatch' && format('dispatch-{0}', github.ref) || github.ref }}", 'ci-${{ github.workflow }}-${{ github.ref }}') })
mutate.call('shared-dispatch-concurrency', workflow_change: ->(text) { text.sub("ci-${{ github.workflow }}-${{ github.event_name == 'schedule' && 'nightly' || github.event_name == 'workflow_dispatch' && format('dispatch-{0}', github.ref) || github.ref }}", "ci-${{ github.workflow }}-${{ github.event_name == 'schedule' && 'nightly' || github.ref }}") })
mutate.call('root-excludes-shard', workflow_change: ->(text) { text.sub('shard: [1, 2, 3, 4]', "shard: [1, 2, 3, 4]\n        exclude:\n          - shard: 4") })
mutate.call('wrong-root-count', workflow_change: ->(text) { text.sub('workspace ${{ matrix.shard }} 4', 'workspace ${{ matrix.shard }} 3') })
mutate.call('literal-desktop-runner', workflow_change: ->(text) { text.sub('desktop ${{ matrix.index }} ${{ matrix.count }}', 'desktop 1 4') })
mutate.call('duplicate-windows-leg', workflow_change: ->(text) { text.sub('index: [1, 2, 3, 4]', 'index: [1, 2, 3, 3]') })
mutate.call('duplicate-updater-windows', scope_change: ->(text) { text.sub("DESKTOP_OS_MATRIX='[\"ubuntu-24.04\",\"macos-15\",\"windows-2025\"]'", "DESKTOP_OS_MATRIX='[\"ubuntu-24.04\",\"macos-15\",\"windows-2025\",\"windows-2025\"]'") })
mutate.call('missing-gate-need', workflow_change: ->(text) { text.sub(', frontend, desktop', ', desktop') })
mutate.call('timing-report-before-gate', workflow_change: ->(text) { text.sub("  timing-report:\n    needs: [gate]", "  timing-report:\n    needs: [changes]") })
mutate.call('missing-gate-env', workflow_change: ->(text) { text.sub("          FRONTEND_RESULT: ${{ needs.frontend.result }}\n", '') })
mutate.call('missing-gate-job', gate_change: ->(text) { text.sub(' frontend', '') })
mutate.call('frontend-overwrite', workflow_change: ->(text) { text.sub("          overwrite: true\n", '') })
mutate.call('skipped-required-frontend', workflow_change: ->(text) { text.sub("    if: needs.changes.outputs.desktop == 'true'", '    if: false') })
mutate.call('missing-posix-installer', workflow_change: ->(text) { text.gsub('bash .github/scripts/install-cargo-nextest.sh', 'bash missing-cargo-nextest.sh') })
mutate.call('missing-windows-installer-test', workflow_change: ->(text) { text.sub('install-cargo-nextest.Tests.ps1', 'missing-cargo-nextest.Tests.ps1') })
updater_test_step = "      - run: 'cargo test --locked --no-default-features --manifest-path apps/desktop/src-tauri/Cargo.toml --lib --bins updates::'\n"
mutate.call('updater-cache-saves', workflow_change: ->(text) { text.sub("save-if: false\n          cache-on-failure: true\n      - run: cargo check --locked --no-default-features", "save-if: true\n          cache-on-failure: true\n      - run: cargo check --locked --no-default-features") })
mutate.call('missing-updater-check', workflow_change: ->(text) { text.sub("      - run: cargo check --locked --no-default-features --manifest-path apps/desktop/src-tauri/Cargo.toml\n", '') })
mutate.call('updater-without-bins', workflow_change: ->(text) { text.sub(updater_test_step, updater_test_step.sub(' --bins', '')) })
mutate.call('desktop-cache-saves-on-pr', workflow_change: ->(text) { text.sub("shared-key: ${{ matrix.os }}-desktop\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: ${{ github.ref == 'refs/heads/production' }}", "shared-key: ${{ matrix.os }}-desktop\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: true") })
mutate.call('mismatched-updater-shared-key', workflow_change: ->(text) { text.sub("shared-key: ${{ matrix.os }}-desktop\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: false", "shared-key: ${{ matrix.os }}-updater\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: false") })
mutate.call('missing-integration-archive-env', workflow_change: ->(text) { text.sub("          PEPPY_NEXTEST_ARCHIVE: ${{ runner.temp }}/workspace-tests.tar.zst\n", '') })
mutate.call('integration-shard-cache', workflow_change: ->(text) { text.sub("      - name: Run workspace integration tests\n", "      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0\n      - name: Run workspace integration tests\n") })
mutate.call('missing-integration-doctest', workflow_change: ->(text) { text.sub("      - run: cargo test --workspace --locked --doc\n", '') })
mutate.call('integration-upload-no-overwrite', workflow_change: ->(text) { text.sub("          name: workspace-tests-${{ github.sha }}\n          path: ${{ runner.temp }}/workspace-tests.tar.zst\n          retention-days: 1\n          overwrite: true", "          name: workspace-tests-${{ github.sha }}\n          path: ${{ runner.temp }}/workspace-tests.tar.zst\n          retention-days: 1\n          overwrite: false") })
mutate.call('rust-cache-without-cleanup', workflow_change: ->(text) { text.sub("      - run: bash .github/scripts/remove-unpinned-rust-toolchains.sh 1.98.1\n        shell: bash\n", '') })
mutate.call('windows-archive-env', workflow_change: ->(text) { text.sub("          PEPPY_NEXTEST_ARCHIVE: ${{ runner.temp }}/desktop-tests.tar.zst\n", '') })
mutate.call('windows-shard-cache', workflow_change: ->(text) { text.sub("      - name: Run Windows desktop tests\n", "      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0\n      - name: Run Windows desktop tests\n") })
mutate.call('windows-build-overwrite', workflow_change: ->(text) { text.sub("          name: desktop-tests-windows-${{ github.sha }}\n          path: ${{ runner.temp }}/desktop-tests.tar.zst\n          retention-days: 1\n          overwrite: true", "          name: desktop-tests-windows-${{ github.sha }}\n          path: ${{ runner.temp }}/desktop-tests.tar.zst\n          retention-days: 1\n          overwrite: false") })
mutate.call('windows-build-cache-key', workflow_change: ->(text) { text.sub('shared-key: windows-2025-desktop', 'shared-key: wrong') })
mutate.call('windows-shard-count', workflow_change: ->(text) { text.sub('desktop ${{ matrix.index }} 4', 'desktop ${{ matrix.index }} 3') })
mutate.call('desktop-deny-all-platforms', workflow_change: ->(text) { text.sub("      - if: runner.os == 'Linux'\n        run: cargo install cargo-deny", "      - run: cargo install cargo-deny") })
mutate.call('missing-windows-script-test', workflow_change: ->(text) { text.sub('tests/dev/windows-desktop.Tests.ps1', 'tests/dev/missing.Tests.ps1') })
mutate.call('missing-windows-expiry-diagnostic', workflow_change: ->(text) { text.sub('Desktop test archive unavailable; it expires after one day. Re-run all jobs.', 'Archive unavailable.') })
mutate.call('windows-check-saves-cache', workflow_change: ->(text) { text.sub("shared-key: windows-2025-desktop\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: false", "shared-key: windows-2025-desktop\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: true") })
mutate.call('missing-windows-check', workflow_change: ->(text) { text.sub("      - run: cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml\n\n  desktop-windows:", "\n  desktop-windows:") })
puts 'CI topology mutation tests passed'
