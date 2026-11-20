#!/usr/bin/env ruby
# frozen_string_literal: true

require 'json'
require 'open3'
require 'optparse'
require 'tempfile'
require 'yaml'

ROOT = File.expand_path('..', __dir__)
VALIDATION_JOBS = %w[changes source-audit workflow-security version release-tooling release-macos-tooling development-tools rust integration-build integration frontend desktop desktop-windows-build desktop-windows-check desktop-windows desktop-updater android google-messages-go swift-bindings container-image development-artifacts].freeze
PUBLICATION_JOBS = %w[publish-prerelease publish-prerelease-android publish-production-server publish-production-relay publish-production-mcp notify-hosted-production stable-release].freeze
FULL_MATRIX = [{ 'os' => 'ubuntu-24.04', 'index' => 1, 'count' => 1 }, { 'os' => 'macos-15', 'index' => 1, 'count' => 1 }].freeze
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

workflow = YAML.safe_load(File.read(options[:workflow]), aliases: true)
jobs = workflow.fetch('jobs')
fail!('nightly schedule is missing') unless workflow.fetch(true).fetch('schedule').include?({ 'cron' => '17 9 * * *' })
fail!('nightly and dispatched runs must use their own concurrency groups, separate from branch pushes') unless workflow.dig('concurrency', 'group') == "ci-${{ github.workflow }}-${{ github.event_name == 'schedule' && 'nightly' || github.event_name == 'workflow_dispatch' && format('dispatch-{0}', github.ref) || github.ref }}"
changes = jobs.fetch('changes')
fail!('changes requires actions read') unless changes.dig('permissions', 'actions') == 'read'
reuse = changes.fetch('steps').find { |step| step['id'] == 'reuse' }
scope = changes.fetch('steps').find { |step| step['id'] == 'scope' }
fail!('reuse must run only on production push before scope') unless reuse && scope && changes.fetch('steps').index(reuse) < changes.fetch('steps').index(scope) && reuse['if'] == "github.event_name == 'push' && github.ref == 'refs/heads/production'"
fail!('scope must use the reuse step output') unless scope.dig('env', 'REUSE_VERIFIED') == '${{ steps.reuse.outputs.reused }}'
fail!('reuse output must only feed scope') unless changes.fetch('steps').count { |step| step.fetch('env', {}).values.include?('${{ steps.reuse.outputs.reused }}') } == 1
fail!('CI job inventory changed without topology classification') unless jobs.keys.sort == (VALIDATION_JOBS + %w[gate timing-report] + PUBLICATION_JOBS).sort
fail!('timing-report must follow gate') unless jobs.fetch('timing-report').fetch('needs') == ['gate']
integration = jobs.fetch('integration')
integration_build = jobs.fetch('integration-build')
fail!('integration-build must require the integration scope') unless integration_build['needs'] == 'changes' && integration_build['if'] == "needs.changes.outputs.integration == 'true'" && integration_build['runs-on'] == 'ubuntu-24.04' && integration_build['timeout-minutes'] == 60
fail!('integration shards must require the build and integration scope') unless integration['needs'].sort == %w[changes integration-build] && integration['if'] == "needs.changes.outputs.integration == 'true'"
build_steps = integration_build.fetch('steps')
fail!('integration-build must archive workspace tests') unless build_steps.any? { |step| step['run'] == 'cargo nextest archive --workspace --locked --profile ci --config-file .config/nextest.toml --archive-file "$RUNNER_TEMP/workspace-tests.tar.zst"' }
fail!('integration-build must run workspace doctests') unless build_steps.any? { |step| step['run'] == 'cargo test --workspace --locked --doc' }
archive_upload = build_steps.find { |step| step['uses']&.start_with?('actions/upload-artifact@') }
fail!('integration-build must upload a short-lived SHA-bound archive') unless archive_upload && archive_upload['with'] == { 'name' => 'workspace-tests-${{ github.sha }}', 'path' => '${{ runner.temp }}/workspace-tests.tar.zst', 'retention-days' => 1, 'overwrite' => true, 'if-no-files-found' => 'error' }
fail!('integration shards must not use rust-cache or install a Rust toolchain') if integration.fetch('steps').any? { |step| step['uses']&.match?(/\A(?:Swatinem\/rust-cache|dtolnay\/rust-toolchain)@/) }
archive_download = integration.fetch('steps').find { |step| step['uses']&.start_with?('actions/download-artifact@') }
fail!('integration shards must download the SHA-bound archive') unless archive_download&.dig('with', 'name') == 'workspace-tests-${{ github.sha }}' && archive_download.dig('with', 'path') == '${{ runner.temp }}'
fail!('integration shards must report archive download failure') unless archive_download['id'] == 'workspace_archive' && integration.fetch('steps').any? { |step| step['if'] == "failure() && steps.workspace_archive.outcome == 'failure'" && step.fetch('run', '').include?('Workspace test archive unavailable; it expires after one day. Re-run all jobs.') }
fail!('root integration must use fail-fast false') unless integration.dig('strategy', 'fail-fast') == false
fail!('root integration matrix must have only shard coverage') unless integration.fetch('strategy').fetch('matrix').keys == ['shard']
fail!('root integration shards must be exactly 1..4') unless integration.dig('strategy', 'matrix', 'shard') == [1, 2, 3, 4]
integration_run = integration.fetch('steps').find { |step| step['name'] == 'Run workspace integration tests' }
fail!('integration shards must run the downloaded archive') unless integration_run&.dig('env', 'PEPPY_NEXTEST_ARCHIVE') == '${{ runner.temp }}/workspace-tests.tar.zst'
run = integration_run.fetch('run')
fail!('root integration runner must use the literal four-way matrix') unless run.include?('run-rust-test-shard.sh workspace ${{ matrix.shard }} 4')
fail!('integration shards must not run doctests') if integration.fetch('steps').any? { |step| step.fetch('run', '').include?('cargo test --workspace --locked --doc') }
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
%w[rust integration-build desktop desktop-windows-build desktop-windows-check desktop-updater swift-bindings].each do |name|
  job_steps = jobs.fetch(name).fetch('steps')
  cache_index = job_steps.index { |step| step['uses']&.start_with?('Swatinem/rust-cache@') }
  cleanup_index = job_steps.index { |step| step['run'] == 'bash .github/scripts/remove-unpinned-rust-toolchains.sh 1.98.1' && step['shell'] == 'bash' }
  toolchain_index = job_steps.index { |step| step['uses']&.start_with?('dtolnay/rust-toolchain@') && step.dig('with', 'toolchain') == '1.98.1' }
  fail!("#{name} must clean unpinned toolchains after installation and before rust-cache") unless toolchain_index && cleanup_index && cache_index && toolchain_index < cleanup_index && cleanup_index < cache_index
end
cache_jobs = jobs.select { |_name, job| job.fetch('steps', []).any? { |step| step['uses']&.start_with?('Swatinem/rust-cache@') } }.keys
fail!('every rust-cache job must be covered by toolchain cleanup checks') unless cache_jobs.sort == %w[rust integration-build desktop desktop-windows-build desktop-windows-check desktop-updater swift-bindings].sort
integration_cache = rust_cache_with.call(integration_build, 'integration-build')
fail!('integration-build cache must save on production even on failure') unless integration_cache['key'] == 'ubuntu-24.04' && integration_cache['save-if'] == "${{ github.ref == 'refs/heads/production' }}" && integration_cache['cache-on-failure'] == true
desktop_cache = rust_cache_with.call(desktop, 'desktop')
updater_cache = rust_cache_with.call(updater, 'desktop-updater')
fail!('desktop and desktop-updater must share the desktop Rust cache key') unless desktop_cache['shared-key'] == '${{ matrix.os }}-desktop' && updater_cache['shared-key'] == '${{ matrix.os }}-desktop'
fail!('desktop Rust caches must not set key alongside shared-key') if desktop_cache.key?('key') || updater_cache.key?('key')
fail!('desktop-updater Rust cache must be restore-only') unless updater_cache['save-if'] == false
updater_cargo_runs = updater.fetch('steps').map { |step| step['run'] }.compact.select { |run| run.include?('cargo ') }
fail!('desktop-updater must run cargo check before cargo test covering the library, binaries and updates:: tests') unless updater_cargo_runs == [
  'cargo check --locked --no-default-features --manifest-path apps/desktop/src-tauri/Cargo.toml',
  'cargo test --locked --no-default-features --manifest-path apps/desktop/src-tauri/Cargo.toml --lib --bins updates::'
]
fail!('desktop Rust cache must save only from production') unless desktop_cache['save-if'] == "${{ github.ref == 'refs/heads/production' }}"
full_scope = scope_output(options[:scope], { 'FILTER_FORCE_FULL' => 'true' })
linux_scope = scope_output(options[:scope], { 'FILTER_WEB' => 'true' })
fail!('full desktop matrix has missing, duplicate, or invalid legs') unless JSON.parse(full_scope.fetch('desktop_matrix')) == FULL_MATRIX
fail!('Linux-only desktop matrix must remain a singleton') unless JSON.parse(linux_scope.fetch('desktop_matrix')) == LINUX_MATRIX
fail!('full updater matrix must run once per desktop OS') unless JSON.parse(full_scope.fetch('desktop_os_matrix')) == FULL_OS_MATRIX
fail!('Linux-only updater matrix must remain a singleton') unless JSON.parse(linux_scope.fetch('desktop_os_matrix')) == LINUX_OS_MATRIX
fail!('Windows scope must match full desktop scope') unless full_scope['desktop_windows'] == 'true' && linux_scope['desktop_windows'] == 'false' && changes.dig('outputs', 'desktop_windows') == '${{ steps.scope.outputs.desktop_windows }}'
frontend = jobs.fetch('frontend')
fail!('frontend must run whenever desktop validation is required') unless frontend['if'] == "needs.changes.outputs.desktop == 'true'"
upload = frontend.fetch('steps').find { |step| step['uses']&.start_with?('actions/upload-artifact@') }
fail!('frontend artifact must be SHA-bound and overwrite on reruns') unless upload&.dig('with', 'name') == 'desktop-dist-${{ github.sha }}' && upload.dig('with', 'overwrite') == true
%w[desktop desktop-windows-build desktop-windows-check desktop-updater].each do |name|
  download = jobs.fetch(name).fetch('steps').find { |step| step['uses']&.start_with?('actions/download-artifact@') }
  fail!("#{name} must consume the frontend SHA-bound artifact") unless download&.dig('with', 'name') == 'desktop-dist-${{ github.sha }}' && download.dig('with', 'path') == 'apps/desktop/dist'
end
steps = desktop.fetch('steps')
posix_installer = steps.find { |step| step['name'] == 'Install cargo-nextest' }
fail!('desktop Linux/macOS legs must install verified cargo-nextest') unless posix_installer && !posix_installer.key?('if') && posix_installer.fetch('run').include?('install-cargo-nextest.sh') && posix_installer.fetch('run').include?('GITHUB_PATH')
deny_steps = steps.select { |step| step.fetch('run', '').match?(/cargo (?:install cargo-deny|deny )/) }
fail!('desktop cargo-deny install and check must run only on Linux') unless deny_steps.length == 2 && deny_steps.all? { |step| step['if'] == "runner.os == 'Linux'" }
windows_build = jobs.fetch('desktop-windows-build')
windows_check = jobs.fetch('desktop-windows-check')
windows = jobs.fetch('desktop-windows')
fail!('Windows build scope and dependencies are incorrect') unless windows_build['needs'].sort == %w[changes frontend] && windows_build['if'] == "needs.changes.outputs.desktop_windows == 'true'" && windows_build['runs-on'] == 'windows-2025' && windows_build['timeout-minutes'] == 60
fail!('Windows check scope and dependencies are incorrect') unless windows_check['needs'].sort == %w[changes frontend] && windows_check['if'] == "needs.changes.outputs.desktop_windows == 'true'" && windows_check['runs-on'] == 'windows-2025' && windows_check['timeout-minutes'] == 60
fail!('Windows shard scope and dependencies are incorrect') unless windows['needs'].sort == %w[changes desktop-windows-build] && windows['if'] == "needs.changes.outputs.desktop_windows == 'true'" && windows['runs-on'] == 'windows-2025' && windows['timeout-minutes'] == 60
fail!('Windows shards must cover four partitions') unless windows.dig('strategy', 'fail-fast') == false && windows.dig('strategy', 'matrix') == { 'index' => [1, 2, 3, 4] }
windows_build_steps = windows_build.fetch('steps')
windows_steps = windows.fetch('steps')
dist_index = windows_build_steps.index { |step| step['uses']&.start_with?('actions/download-artifact@') }
fail!('Windows build must download dist immediately after checkout') unless dist_index == 1
fail!('Windows build must archive desktop tests and run doctests') unless windows_build_steps.any? { |step| step['run'] == 'cargo nextest archive --manifest-path apps/desktop/src-tauri/Cargo.toml --locked --profile ci --config-file .config/nextest.toml --archive-file "$RUNNER_TEMP/desktop-tests.tar.zst"' } && windows_build_steps.any? { |step| step['run'] == 'cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --doc' }
check_command = 'cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml'
fail!('Windows check must run cargo check instead of the build job') unless windows_check.fetch('steps').any? { |step| step['run'] == check_command } && windows_build_steps.none? { |step| step['run'] == check_command }
fail!('Windows check must download dist immediately after checkout') unless windows_check.fetch('steps').index { |step| step['uses']&.start_with?('actions/download-artifact@') } == 1
%w[tests/dev/windows-desktop.Tests.ps1 .github/scripts/windows-loader-diagnostics.Tests.ps1].each do |script|
  fail!("Windows build must test #{script}") unless windows_build_steps.any? { |step| step['shell'] == 'pwsh' && step.fetch('run', '').include?(script) }
end
windows_upload = windows_build_steps.find { |step| step['uses']&.start_with?('actions/upload-artifact@') }
fail!('Windows archive must be short-lived and SHA-bound') unless windows_upload && windows_upload['with'] == { 'name' => 'desktop-tests-windows-${{ github.sha }}', 'path' => '${{ runner.temp }}/desktop-tests.tar.zst', 'retention-days' => 1, 'overwrite' => true, 'if-no-files-found' => 'error' }
windows_cache = rust_cache_with.call(windows_build, 'desktop-windows-build')
fail!('Windows build must save its shared cache on production') unless windows_cache['shared-key'] == 'windows-2025-desktop' && windows_cache['workspaces'] == 'apps/desktop/src-tauri -> target' && windows_cache['save-if'] == "${{ github.ref == 'refs/heads/production' }}" && windows_cache['cache-on-failure'] == true
check_cache = rust_cache_with.call(windows_check, 'desktop-windows-check')
fail!('Windows check cache must be restore-only and match the build cache') unless check_cache['shared-key'] == 'windows-2025-desktop' && check_cache['workspaces'] == 'apps/desktop/src-tauri -> target' && check_cache['save-if'] == false && check_cache['cache-on-failure'] == true
fail!('Windows shards must not build or restore dependencies') if windows_steps.any? { |step| step['uses']&.match?(/\A(?:Swatinem\/rust-cache|dtolnay\/rust-toolchain|actions\/setup-node)@/) || step.fetch('run', '').include?('pnpm ') }
windows_download = windows_steps.find { |step| step['uses']&.start_with?('actions/download-artifact@') }
fail!('Windows shards must download their SHA-bound archive') unless windows_download&.dig('with', 'name') == 'desktop-tests-windows-${{ github.sha }}' && windows_download.dig('with', 'path') == '${{ runner.temp }}'
fail!('Windows shards must report archive download failure') unless windows_download['id'] == 'desktop_archive' && windows_steps.any? { |step| step['if'] == "failure() && steps.desktop_archive.outcome == 'failure'" && step.fetch('run', '').include?('Desktop test archive unavailable; it expires after one day. Re-run all jobs.') }
windows_run = windows_steps.find { |step| step['run']&.include?('run-rust-test-shard.sh desktop') }
fail!('Windows shards must run archive partitions') unless windows_run&.fetch('run') == 'bash .github/scripts/run-rust-test-shard.sh desktop ${{ matrix.index }} 4' && windows_run.dig('env', 'PEPPY_NEXTEST_ARCHIVE') == '${{ runner.temp }}/desktop-tests.tar.zst'
fail!('Windows loader diagnostics must remain available') unless windows_steps.any? { |step| step['name'] == 'Diagnose Windows loader failure' && step['if'] == 'failure()' && step.fetch('run').include?("peppy_desktop_lib-*.exe") }
%w[desktop-windows-build desktop-windows].each do |name|
  installer = jobs.fetch(name).fetch('steps').find { |step| step['name'] == 'Install cargo-nextest on Windows' }
  fail!("#{name} must install verified nextest") unless installer && installer['shell'] == 'pwsh' && installer.fetch('run').include?('install-cargo-nextest.ps1') && installer.fetch('run').include?('GITHUB_PATH')
end
fail!('Windows build must test nextest installer') unless windows_build_steps.any? { |step| step['shell'] == 'pwsh' && step.fetch('run', '').include?('install-cargo-nextest.Tests.ps1') }
gate = jobs.fetch('gate')
fail!('gate must need every validation job exactly once') unless gate.fetch('needs').sort == VALIDATION_JOBS.sort
environment = gate.fetch('steps').find { |step| step['name'] == 'Evaluate CI gate' }.fetch('env')
VALIDATION_JOBS.each { |name| prefix = name.upcase.tr('-', '_'); fail!("gate result is missing for #{name}") unless environment["#{prefix}_RESULT"] == "${{ needs.#{name}.result }}" }
required_scopes = {
  'rust' => 'rust',
  'integration-build' => 'integration',
  'integration' => 'integration',
  'frontend' => 'desktop',
  'desktop' => 'desktop',
  'desktop-windows-build' => 'desktop_windows',
  'desktop-windows-check' => 'desktop_windows',
  'desktop-windows' => 'desktop_windows',
  'desktop-updater' => 'desktop',
  'android' => 'android',
  'google-messages-go' => 'android',
  'swift-bindings' => 'swift_bindings',
  'container-image' => 'container_image',
  'development-artifacts' => 'development_artifacts'
}.freeze
required_scopes.each do |name, scope|
  prefix = name.upcase.tr('-', '_')
  fail!("gate required flag is missing for #{name}") unless environment["#{prefix}_REQUIRED"] == "${{ needs.changes.outputs.#{scope} }}"
end
gate_jobs = File.read(options[:gate])[/JOBS=\(\n(.*?)\n\)/m, 1].split
fail!('ci-gate JOBS must match gate validation jobs') unless gate_jobs.sort == VALIDATION_JOBS.sort
puts 'CI shard topology checks passed'
