#!/usr/bin/env ruby
# frozen_string_literal: true

require 'json'
require 'open3'
require 'optparse'
require 'tempfile'
require 'yaml'

ROOT = File.expand_path('..', __dir__)
VALIDATION_JOBS = %w[changes source-audit workflow-security version release-tooling development-tools rust integration frontend desktop desktop-updater android google-messages-go swift-bindings container-image development-artifacts].freeze
PUBLICATION_JOBS = %w[publish-prerelease publish-prerelease-android publish-staging-server publish-staging-relay publish-staging-mcp publish-production-server publish-production-relay publish-production-mcp notify-hosted-production stable-release].freeze
FULL_MATRIX = [{ 'os' => 'ubuntu-24.04', 'index' => 1, 'count' => 1 }, { 'os' => 'macos-15', 'index' => 1, 'count' => 1 }, { 'os' => 'windows-2025', 'index' => 1, 'count' => 4 }, { 'os' => 'windows-2025', 'index' => 2, 'count' => 4 }, { 'os' => 'windows-2025', 'index' => 3, 'count' => 4 }, { 'os' => 'windows-2025', 'index' => 4, 'count' => 4 }].freeze
LINUX_MATRIX = [{ 'os' => 'ubuntu-24.04', 'index' => 1, 'count' => 1 }].freeze
FULL_OS_MATRIX = %w[ubuntu-24.04 macos-15 windows-2025].freeze
LINUX_OS_MATRIX = ['ubuntu-24.04'].freeze

options = { workflow: File.join(ROOT, 'workflows', 'ci.yml'), scope: File.join(ROOT, 'scripts', 'ci-scope.sh'), gate: File.join(ROOT, 'scripts', 'ci-gate.sh') }
OptionParser.new { |parser| parser.on('--workflow PATH') { |path| options[:workflow] = path }; parser.on('--scope PATH') { |path| options[:scope] = path }; parser.on('--gate PATH') { |path| options[:gate] = path } }.parse!
def fail!(message)
  abort("check-ci-shards: #{message}")
end
def scope_output(scope, environment)
  Tempfile.create('ci-scope-output') do |output|
    output.close
    _, stderr, status = Open3.capture3(environment.merge('GITHUB_OUTPUT' => output.path), 'bash', scope)
    fail!("ci-scope failed: #{stderr}") unless status.success?
    return File.readlines(output.path, chomp: true).to_h { |line| line.split('=', 2) }
  end
end

jobs = YAML.safe_load(File.read(options[:workflow]), aliases: true).fetch('jobs')
fail!('CI job inventory changed without topology classification') unless jobs.keys.sort == (VALIDATION_JOBS + ['gate'] + PUBLICATION_JOBS).sort
integration = jobs.fetch('integration')
fail!('root integration must use fail-fast false') unless integration.dig('strategy', 'fail-fast') == false
fail!('root integration matrix must have only shard coverage') unless integration.fetch('strategy').fetch('matrix').keys == ['shard']
fail!('root integration shards must be exactly 1..4') unless integration.dig('strategy', 'matrix', 'shard') == [1, 2, 3, 4]
run = integration.fetch('steps').find { |step| step['name'] == 'Run workspace integration tests' }.fetch('run')
fail!('root integration runner must use the literal four-way matrix') unless run.include?('run-rust-test-shard.sh workspace ${{ matrix.shard }} 4')
desktop = jobs.fetch('desktop')
fail!('desktop must use fail-fast false') unless desktop.dig('strategy', 'fail-fast') == false
fail!('desktop must use the scoped partition matrix') unless desktop.dig('strategy', 'matrix', 'include') == '${{ fromJSON(needs.changes.outputs.desktop_matrix) }}'
fail!('desktop must require frontend') unless desktop.fetch('needs').sort == %w[changes frontend]
desktop_run = desktop.fetch('steps').find { |step| step['run']&.include?('run-rust-test-shard.sh desktop') }.fetch('run')
fail!('desktop runner must bind each matrix leg index and count') unless desktop_run.strip == 'bash .github/scripts/run-rust-test-shard.sh desktop ${{ matrix.index }} ${{ matrix.count }}'
updater = jobs.fetch('desktop-updater')
fail!('desktop-updater must use fail-fast false') unless updater.dig('strategy', 'fail-fast') == false
fail!('desktop-updater must use the scoped OS matrix once per platform') unless updater.dig('strategy', 'matrix', 'os') == '${{ fromJSON(needs.changes.outputs.desktop_os_matrix) }}'
fail!('desktop-updater must require frontend') unless updater.fetch('needs').sort == %w[changes frontend]
rust_cache_with = lambda do |job, name|
  step = job.fetch('steps').find { |candidate| candidate['uses']&.start_with?('Swatinem/rust-cache@') }
  fail!("#{name} must use Swatinem/rust-cache") unless step
  step.fetch('with')
end
desktop_cache = rust_cache_with.call(desktop, 'desktop')
updater_cache = rust_cache_with.call(updater, 'desktop-updater')
fail!('desktop and desktop-updater must share the desktop Rust cache key') unless desktop_cache['shared-key'] == '${{ matrix.os }}-desktop' && updater_cache['shared-key'] == '${{ matrix.os }}-desktop'
fail!('desktop Rust caches must not set key alongside shared-key') if desktop_cache.key?('key') || updater_cache.key?('key')
fail!('desktop-updater Rust cache must be restore-only') unless updater_cache['save-if'] == false
fail!('desktop Rust cache must save only from production shard 1') unless desktop_cache['save-if'] == "${{ github.ref == 'refs/heads/production' && matrix.index == 1 }}"
full_scope = scope_output(options[:scope], { 'FILTER_FORCE_FULL' => 'true' })
linux_scope = scope_output(options[:scope], { 'FILTER_WEB' => 'true' })
fail!('full desktop matrix has missing, duplicate, or invalid legs') unless JSON.parse(full_scope.fetch('desktop_matrix')) == FULL_MATRIX
fail!('Linux-only desktop matrix must remain a singleton') unless JSON.parse(linux_scope.fetch('desktop_matrix')) == LINUX_MATRIX
fail!('full updater matrix must run once per desktop OS') unless JSON.parse(full_scope.fetch('desktop_os_matrix')) == FULL_OS_MATRIX
fail!('Linux-only updater matrix must remain a singleton') unless JSON.parse(linux_scope.fetch('desktop_os_matrix')) == LINUX_OS_MATRIX
frontend = jobs.fetch('frontend')
fail!('frontend must run whenever desktop validation is required') unless frontend['if'] == "needs.changes.outputs.desktop == 'true'"
upload = frontend.fetch('steps').find { |step| step['uses']&.start_with?('actions/upload-artifact@') }
fail!('frontend artifact must be SHA-bound and overwrite on reruns') unless upload&.dig('with', 'name') == 'desktop-dist-${{ github.sha }}' && upload.dig('with', 'overwrite') == true
%w[desktop desktop-updater].each do |name|
  download = jobs.fetch(name).fetch('steps').find { |step| step['uses']&.start_with?('actions/download-artifact@') }
  fail!("#{name} must consume the frontend SHA-bound artifact") unless download&.dig('with', 'name') == 'desktop-dist-${{ github.sha }}' && download.dig('with', 'path') == 'apps/desktop/dist'
end
steps = desktop.fetch('steps')
posix_installer = steps.find { |step| step['name'] == 'Install cargo-nextest' }
windows_installer = steps.find { |step| step['name'] == 'Install cargo-nextest on Windows' }
windows_test = steps.find { |step| step['name'] == 'Test cargo-nextest installer on Windows' }
fail!('desktop Linux/macOS legs must install verified cargo-nextest') unless posix_installer&.fetch('if') == "runner.os != 'Windows'" && posix_installer.fetch('run').include?('install-cargo-nextest.sh') && posix_installer.fetch('run').include?('GITHUB_PATH')
fail!('desktop Windows legs must install verified cargo-nextest') unless windows_installer&.fetch('if') == "runner.os == 'Windows'" && windows_installer.fetch('shell') == 'pwsh' && windows_installer.fetch('run').include?('install-cargo-nextest.ps1') && windows_installer.fetch('run').include?('GITHUB_PATH')
fail!('desktop Windows shard one must test the PowerShell installer') unless windows_test&.fetch('if') == "runner.os == 'Windows' && matrix.index == 1" && windows_test.fetch('shell') == 'pwsh' && windows_test.fetch('run').include?('install-cargo-nextest.Tests.ps1')
gate = jobs.fetch('gate')
fail!('gate must need every validation job exactly once') unless gate.fetch('needs').sort == VALIDATION_JOBS.sort
environment = gate.fetch('steps').find { |step| step['name'] == 'Evaluate CI gate' }.fetch('env')
VALIDATION_JOBS.each { |name| prefix = name.upcase.tr('-', '_'); fail!("gate result is missing for #{name}") unless environment["#{prefix}_RESULT"] == "${{ needs.#{name}.result }}" }
required_scopes = {
  'frontend' => 'desktop',
  'desktop' => 'desktop',
  'desktop-updater' => 'desktop',
  'google-messages-go' => 'android'
}.freeze
required_scopes.each do |name, scope|
  prefix = name.upcase.tr('-', '_')
  fail!("gate required flag is missing for #{name}") unless environment["#{prefix}_REQUIRED"] == "${{ needs.changes.outputs.#{scope} }}"
end
gate_jobs = File.read(options[:gate])[/JOBS=\(\n(.*?)\n\)/m, 1].split
fail!('ci-gate JOBS must match gate validation jobs') unless gate_jobs.sort == VALIDATION_JOBS.sort
puts 'CI shard topology checks passed'
