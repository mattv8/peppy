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
mutate.call('root-excludes-shard', workflow_change: ->(text) { text.sub('shard: [1, 2, 3, 4]', "shard: [1, 2, 3, 4]\n        exclude:\n          - shard: 4") })
mutate.call('wrong-root-count', workflow_change: ->(text) { text.sub('workspace ${{ matrix.shard }} 4', 'workspace ${{ matrix.shard }} 3') })
mutate.call('literal-desktop-runner', workflow_change: ->(text) { text.sub('desktop ${{ matrix.index }} ${{ matrix.count }}', 'desktop 1 4') })
mutate.call('duplicate-windows-leg', scope_change: ->(text) { text.sub('"index":4,"count":4', '"index":3,"count":4') })
mutate.call('duplicate-updater-windows', scope_change: ->(text) { text.sub("DESKTOP_OS_MATRIX='[\"ubuntu-24.04\",\"macos-15\",\"windows-2025\"]'", "DESKTOP_OS_MATRIX='[\"ubuntu-24.04\",\"macos-15\",\"windows-2025\",\"windows-2025\"]'") })
mutate.call('missing-gate-need', workflow_change: ->(text) { text.sub(', frontend, desktop', ', desktop') })
mutate.call('missing-gate-env', workflow_change: ->(text) { text.sub("          FRONTEND_RESULT: ${{ needs.frontend.result }}\n", '') })
mutate.call('missing-gate-job', gate_change: ->(text) { text.sub(' frontend', '') })
mutate.call('frontend-overwrite', workflow_change: ->(text) { text.sub("          overwrite: true\n", '') })
mutate.call('skipped-required-frontend', workflow_change: ->(text) { text.sub("    if: needs.changes.outputs.desktop == 'true'", '    if: false') })
mutate.call('missing-posix-installer', workflow_change: ->(text) { text.gsub('bash .github/scripts/install-cargo-nextest.sh', 'bash missing-cargo-nextest.sh') })
mutate.call('missing-windows-installer-test', workflow_change: ->(text) { text.sub('install-cargo-nextest.Tests.ps1', 'missing-cargo-nextest.Tests.ps1') })
mutate.call('updater-cache-saves', workflow_change: ->(text) { text.sub("save-if: false\n          cache-on-failure: true\n      - run: cargo check --locked --no-default-features", "save-if: true\n          cache-on-failure: true\n      - run: cargo check --locked --no-default-features") })
mutate.call('desktop-cache-saves-every-shard', workflow_change: ->(text) { text.sub("save-if: ${{ github.ref == 'refs/heads/production' && matrix.index == 1 }}", "save-if: ${{ github.ref == 'refs/heads/production' }}") })
mutate.call('mismatched-updater-shared-key', workflow_change: ->(text) { text.sub("shared-key: ${{ matrix.os }}-desktop\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: false", "shared-key: ${{ matrix.os }}-updater\n          workspaces: apps/desktop/src-tauri -> target\n          save-if: false") })
puts 'CI topology mutation tests passed'
