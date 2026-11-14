#!/usr/bin/env ruby
# frozen_string_literal: true

require 'fileutils'
require 'open3'
require 'shellwords'
require 'tmpdir'
require 'yaml'

root = File.expand_path('..', __dir__)
load_workflow = ->(name) { YAML.load_file(File.join(root, 'workflows', name)) }
assert = ->(condition, message) { abort(message) unless condition }
immutable_action_ref = ->(uses, action) { uses.is_a?(String) && uses.match?(/\A#{Regexp.escape(action)}@[0-9a-f]{40}\z/) }

ci = load_workflow.call('ci.yml')
ci_jobs = ci.fetch('jobs')
assert.call(ci_jobs.key?('google-messages-go'), 'public CI must test the Google Messages Go module')
google_messages_go = ci_jobs.fetch('google-messages-go')
assert.call(google_messages_go.fetch('needs') == 'changes' && google_messages_go.fetch('if') == "needs.changes.outputs.android == 'true'", 'Google Messages Go checks must follow the Android path scope')
assert.call(google_messages_go.dig('defaults', 'run', 'working-directory') == 'apps/android/googlemessages', 'Google Messages Go commands must run in the module')
google_messages_steps = google_messages_go.fetch('steps')
google_messages_setup = google_messages_steps.find { |step| step['uses']&.start_with?('actions/setup-go@') }
assert.call(google_messages_setup && google_messages_setup.fetch('uses').match?(/@\h{40}\z/) && google_messages_setup.dig('with', 'go-version') == '1.27.3' && google_messages_setup.dig('with', 'cache') == false, 'Google Messages CI must pin setup-go and the Go toolchain without saving shared caches')
assert.call(google_messages_steps.map { |step| step['run'] }.compact == ['./prepare-libgm.sh', 'go test -race ./...', 'go test -race go.mau.fi/mautrix-gmessages/pkg/libgm', 'go vet ./...', 'go vet go.mau.fi/mautrix-gmessages/pkg/libgm', 'test -z "$(gofmt -l .)"'], 'Google Messages CI must test and vet the patched libgm package explicitly alongside the module-wide checks')
assert.call(ci_jobs.fetch('gate').fetch('needs').include?('google-messages-go'), 'the CI gate must wait for Google Messages Go checks')
gate_environment = ci_jobs.fetch('gate').fetch('steps').find { |step| step['name'] == 'Evaluate CI gate' }.fetch('env')
assert.call(gate_environment.fetch('GOOGLE_MESSAGES_GO_RESULT') == '${{ needs.google-messages-go.result }}' && gate_environment.fetch('GOOGLE_MESSAGES_GO_REQUIRED') == '${{ needs.changes.outputs.android }}', 'the Go result must be evaluated by the common required-job gate')
assert.call(ci_jobs.fetch('desktop').fetch('timeout-minutes') == 60, 'public CI desktop must allow 60 minutes uniformly for native cold-cache builds')
android_ci = load_workflow.call('test-android.yml')
android_ci_jobs = android_ci.fetch('jobs')
android_call = ci_jobs.fetch('android')
assert.call(android_call.fetch('needs') == 'changes' && android_call.fetch('if') == "needs.changes.outputs.android == 'true'", 'public Android CI must retain the Android path scope')
assert.call(android_call.fetch('uses') == './.github/workflows/test-android.yml', 'public Android CI must call the reusable Android validator')
assert.call((android_call.keys & %w[runs-on timeout-minutes steps with secrets]).empty?, 'the Android reusable-workflow call must not declare invalid runner, timeout, step, input, or secret fields')
assert.call(android_ci.fetch(true).fetch('workflow_call').nil?, 'Android validation must expose a no-input, no-secret callable interface')
android_preparation = android_ci_jobs.fetch('android-native-preparation')
android_ci_steps = android_preparation.fetch('steps')
android_ci_go = android_ci_steps.find { |step| step['uses']&.start_with?('actions/setup-go@') }
android_rust_cache = android_ci_steps.find { |step| step['uses']&.start_with?('Swatinem/rust-cache@') }
android_rust_cache_ok = lambda do |workflow|
  step = workflow.fetch('jobs').fetch('android-native-preparation').fetch('steps').find { |candidate| candidate['uses']&.start_with?('Swatinem/rust-cache@') }
  with = step&.fetch('with', nil)
  with.is_a?(Hash) && with['shared-key'] == 'ubuntu-24.04-android-ndk-27.2.12479018-android' && !with.key?('key') && with['save-if'] == "${{ github.event_name == 'push' && github.ref == 'refs/heads/production' }}"
end
assert.call(android_rust_cache_ok.call(android_ci), 'Android producer Rust cache must use the shared key and production-push save guard that match the production Android cache')
android_mismatch = Marshal.load(Marshal.dump(android_ci))
android_mismatch_step = android_mismatch.fetch('jobs').fetch('android-native-preparation').fetch('steps').find { |candidate| candidate['uses']&.start_with?('Swatinem/rust-cache@') }
android_mismatch_step.fetch('with')['shared-key'] = 'ubuntu-24.04-android-ndk-27.2.12479018'
assert.call(!android_rust_cache_ok.call(android_mismatch), 'Android producer Rust cache mismatch in the parsed workflow must be rejected')
android_ci_aar = android_ci_steps.find { |step| step['name'] == 'Build Google Messages AAR and licenses' }
assert.call(android_ci_go && android_ci_go.fetch('uses') == 'actions/setup-go@44694675825211faa026b3c33043df3e48a5fa00' && android_ci_go.fetch('with') == { 'go-version' => '1.27.3', 'cache' => false }, 'Android preparation must pin Go 1.27.3 without a shared cache')
assert.call(android_ci_aar && android_ci_aar.fetch('run').include?('export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"') && android_ci_aar.fetch('run').include?('apps/android/googlemessages/build-aar.sh') && android_ci_aar.fetch('run').include?('apps/android/googlemessages/check-licenses.sh') && android_ci_aar.fetch('run').include?('apps/android/googlemessages/check-licenses-test.sh'), 'Android preparation must build the Google Messages AAR and run its license checks with the pinned NDK')
android_consumers = %w[android-debug-unit-tests android-sideload-unit-tests android-lint-and-assembly].map { |name| android_ci_jobs.fetch(name) }
assert.call(android_consumers.all? { |job| job.fetch('needs') == 'android-native-preparation' }, 'every Android consumer must require native preparation')
assert.call(android_consumers.all? { |job| job.fetch('steps').any? { |step| step.dig('with', 'name') == 'android-native-${{ github.sha }}' } }, 'every Android consumer must download the SHA-bound native artifact')
android_packaging = android_ci_jobs.fetch('android-lint-and-assembly').fetch('steps').find { |step| step['name'] == 'Lint and assemble Android artifacts' }
assert.call(!android_packaging.nil?, 'Android validation must retain the lint and packaging step')
android_packaging_command = Shellwords.shellsplit(android_packaging.fetch('run'))
android_packaging_flags = android_packaging_command.reject { |token| token.start_with?(':') }
expected_android_packaging_flags = ['./gradlew', '--console=plain', '--dependency-verification=strict', '--stacktrace', '-Dorg.gradle.jvmargs=-Xmx4096m -Dfile.encoding=UTF-8', '--max-workers=2', '-PpeppyGoogleMessagesLink=true']
assert.call(android_packaging_flags == expected_android_packaging_flags, 'Android packaging must use strict dependency verification, the 4 GiB JVM override, two-worker bound, and diagnostic flags')
android_packaging_tasks = android_packaging_command.select { |token| token.start_with?(':') }
assert.call(android_packaging_tasks == %w[:app:verifyReleaseManifestSecurity :app:lintDebug :app:lintSideload :app:assembleDebug :app:assembleDebugAndroidTest :app:assembleSideload :app:verifySideloadGoogleMessagesLink], 'Android packaging must verify the merged release manifest, sideload link, linting, and debug artifacts')
android_aggregate = android_ci_jobs.fetch('android-aggregate')
assert.call(android_aggregate.fetch('if') == '${{ always() }}' && android_aggregate.fetch('needs').sort == %w[android-debug-unit-tests android-lint-and-assembly android-native-preparation android-sideload-unit-tests], 'Android aggregate must always require preparation and every consumer')
ci_text = File.read(File.join(root, 'workflows', 'ci.yml'))
assert.call(ci_jobs.key?('notify-hosted-production'), 'public CI must dispatch verified hosted production evidence')
assert.call(!ci_text.include?('create-github-app-token'), 'public CI must not mint an App token')
assert.call(!ci_text.match?(/HOSTED_DISPATCH_APP_|HOSTED_DISPATCH_TOKEN/), 'public CI must not depend on hosted dispatch credentials')
assert.call(ci_text.include?('GH_TOKEN: ${{ secrets.PEPPY_AUTOMATION_TOKEN }}'), 'public CI must map the automation PAT to GH_TOKEN')
assert.call(!ci_text.match?(/PEPPY_PLATFORM_DISPATCH_TOKEN|SOURCE_PIN_PR_TOKEN/), 'public CI must not retain legacy automation token bindings')
handoff = ci_jobs.fetch('notify-hosted-production')
assert.call(handoff.fetch('if') == "!cancelled() && github.event_name == 'push' && github.ref == 'refs/heads/production' && needs.gate.result == 'success' && needs.publish-production-server.result == 'success' && needs.publish-production-relay.result == 'success' && needs.publish-production-mcp.result == 'success'", 'hosted dispatch must require a successful production push gate and all three publishers')
assert.call(handoff.fetch('needs').sort == %w[gate publish-production-mcp publish-production-relay publish-production-server], 'hosted dispatch must wait for the gate and all three publishers, because hosted builds require the MCP image')
publish_job_names = %w[
  publish-prerelease
  publish-prerelease-android
  publish-production-server
  publish-production-relay
  publish-production-mcp
  stable-release
]

condition_allows = lambda do |condition, event:, ref:, cancelled:, implicit_success:, results:, outputs: {}, inputs: {}|
  has_status_guard = condition.match?(/(?:^|\s)(?:!cancelled|always|failure|success)\(\)/)
  next false if !has_status_guard && !implicit_success

  split_terms = lambda do |expression, operator|
    depth = 0
    terms = [String.new]
    index = 0
    while index < expression.length
      character = expression[index]
      depth += 1 if character == '('
      depth -= 1 if character == ')'
      if depth.zero? && expression[index, operator.length] == operator
        terms << String.new
        index += operator.length
      else
        terms.last << character
        index += 1
      end
    end
    terms.map(&:strip)
  end

  evaluate = nil
  evaluate = lambda do |expression|
    expression = expression.strip.sub(/\A\$\{\{\s*/, '').sub(/\s*\}\}\z/, '')
    expression = expression[1...-1].strip while expression.start_with?('(') && expression.end_with?(')')
    and_terms = split_terms.call(expression.dup, ' && ')
    next and_terms.all? { |term| evaluate.call(term) } if and_terms.length > 1

    or_terms = split_terms.call(expression.dup, ' || ')
    next or_terms.any? { |term| evaluate.call(term) } if or_terms.length > 1

    term = expression
    case term
    when '!cancelled()'
      !cancelled
    when '!inputs.dry_run'
      !inputs.fetch('dry_run', false)
    when "github.event_name == 'push'"
      event == 'push'
    when /\Agithub\.ref == '([^']+)'\z/
      ref == Regexp.last_match(1)
    when /\Aneeds\.([a-z0-9-]+)\.result == '([^']+)'\z/
      results.fetch(Regexp.last_match(1)) == Regexp.last_match(2)
    when /\Aneeds\.([a-z0-9-]+)\.result != 'failure'\z/
      results.fetch(Regexp.last_match(1)) != 'failure'
    when /\Aneeds\.([a-z0-9-]+)\.outputs\.([a-z0-9_]+) == 'true'\z/
      outputs.fetch("#{Regexp.last_match(1)}.#{Regexp.last_match(2)}", false)
    when /\Aneeds\.([a-z0-9-]+)\.outputs\.([a-z0-9_]+) != 'true'\z/
      !outputs.fetch("#{Regexp.last_match(1)}.#{Regexp.last_match(2)}", false)
    else
      abort("unsupported CI publish condition term: #{term}")
    end
  end

  evaluate.call(condition)
end

publish_conditions = publish_job_names.to_h do |job_name|
  [job_name, ci_jobs.fetch(job_name).fetch('if')]
end
publish_conditions.each do |job_name, condition|
  assert.call(condition.include?('!cancelled()'), "#{job_name} must override implicit success without running after cancellation")
  assert.call(!condition.include?('always()'), "#{job_name} must not use always() for publication")
end

successful_results = {
  'gate' => 'success',
  'version' => 'success',
  'development-artifacts' => 'success',
  'publish-prerelease' => 'success',
  'publish-production-server' => 'success',
  'publish-production-relay' => 'success',
  'publish-production-mcp' => 'success'
}.freeze
staging_context = {
  event: 'push', ref: 'refs/heads/staging', cancelled: false,
  implicit_success: false, results: successful_results,
  outputs: { 'version.android_changed' => true, 'version.android_budget_refresh' => false }
}.freeze
production_context = {
  event: 'push', ref: 'refs/heads/production', cancelled: false,
  implicit_success: false, results: successful_results,
  outputs: { 'version.android_changed' => true, 'version.android_budget_refresh' => false }
}.freeze

staging_publishers = %w[publish-prerelease publish-prerelease-android]
production_publishers = %w[publish-production-server publish-production-relay publish-production-mcp stable-release]
staging_publishers.each do |job_name|
  condition = publish_conditions.fetch(job_name)
  assert.call(condition_allows.call(condition, **staging_context.merge(implicit_success: true)), "#{job_name} must run for an ordinary successful staging push")
  assert.call(condition_allows.call(condition, **staging_context), "#{job_name} must remain eligible after an optional prerequisite skips")
end
production_publishers.each do |job_name|
  condition = publish_conditions.fetch(job_name)
  assert.call(condition_allows.call(condition, **production_context.merge(implicit_success: true)), "#{job_name} must run for an ordinary successful production push")
  assert.call(condition_allows.call(condition, **production_context), "#{job_name} must remain eligible after an optional prerequisite skips")
end
pull_request_context = staging_context.merge(event: 'pull_request', ref: 'refs/pull/1/merge', implicit_success: true)
publish_conditions.each do |job_name, condition|
  assert.call(!condition_allows.call(condition, **pull_request_context), "#{job_name} must not run for a pull request")
end
%w[staging production].each do |branch|
  dispatch_context = production_context.merge(event: 'workflow_dispatch', ref: "refs/heads/#{branch}", implicit_success: true)
  publish_conditions.each do |job_name, condition|
    assert.call(!condition_allows.call(condition, **dispatch_context), "#{job_name} must not run for a manual #{branch} dispatch")
  end
end
staging_publishers.each do |job_name|
  assert.call(!condition_allows.call(publish_conditions.fetch(job_name), **production_context), "#{job_name} must not run on production")
end
production_publishers.each do |job_name|
  assert.call(!condition_allows.call(publish_conditions.fetch(job_name), **staging_context), "#{job_name} must not run on staging")
end
%w[publish-prerelease publish-prerelease-android].each do |job_name|
  context = staging_publishers.include?(job_name) ? staging_context : production_context
  output = job_name == 'publish-prerelease-android' ? 'android_head_released' : 'head_released'
  assert.call(!condition_allows.call(publish_conditions.fetch(job_name), **context.merge(outputs: context.fetch(:outputs).merge("version.#{output}" => true))), "#{job_name} must not republish an already released head")
end
publish_conditions.each do |job_name, condition|
  context = staging_publishers.include?(job_name) ? staging_context : production_context
  assert.call(!condition_allows.call(condition, **context.merge(cancelled: true)), "#{job_name} must not run after cancellation")
  %w[failure skipped cancelled].each do |gate_result|
    results = successful_results.merge('gate' => gate_result)
    assert.call(!condition_allows.call(condition, **context.merge(results: results)), "#{job_name} must require a successful gate, not #{gate_result}")
  end
end
%w[publish-prerelease publish-prerelease-android stable-release].each do |job_name|
  context = job_name == 'publish-prerelease' ? staging_context : production_context
  %w[failure skipped cancelled].each do |version_result|
    results = successful_results.merge('version' => version_result)
    assert.call(!condition_allows.call(publish_conditions.fetch(job_name), **context.merge(results: results)), "#{job_name} must require a successful version job, not #{version_result}")
  end
end
%w[publish-production-server publish-production-relay publish-production-mcp].each do |publisher|
  %w[failure skipped cancelled].each do |publisher_result|
    results = successful_results.merge(publisher => publisher_result)
    assert.call(!condition_allows.call(publish_conditions.fetch('stable-release'), **production_context.merge(results: results)), "stable release must require #{publisher} success, not #{publisher_result}")
  end
end

stable = ci_jobs.fetch('stable-release')
assert.call(stable.fetch('needs').sort == %w[gate publish-production-mcp publish-production-relay publish-production-server version], 'stable release must wait for the gate and all three edge publishers')
assert.call(ci_jobs.fetch('notify-hosted-production').fetch('needs').sort == %w[gate publish-production-mcp publish-production-relay publish-production-server], 'hosted dispatch must keep its server and relay payload contract and wait for the MCP image')
assert.call(stable.fetch('permissions') == { 'contents' => 'write', 'actions' => 'read', 'attestations' => 'write', 'id-token' => 'write' }, 'stable release must grant its nested publisher provenance permissions')
assert.call(stable.fetch('concurrency').fetch('group') == 'stable-release', 'automatic stable release must serialize with manual release')
assert.call(stable.fetch('with') == { 'bump' => 'auto', 'version' => '', 'dry_run' => false }, 'automatic release must use the safe automatic inputs')

release = load_workflow.call('release.yml')
assert.call(release.fetch(true).key?('workflow_call'), 'release must be reusable')
assert.call(release.fetch('concurrency').fetch('cancel-in-progress') == false, 'release serialization must not cancel publication')
prepare = release.fetch('jobs').fetch('prepare')
assert.call(prepare.fetch('outputs').key?('skip'), 'duplicate and stale automatic releases must produce a no-op output')
assert.call(%w[android_existing desktop_existing].all? { |name| prepare.fetch('outputs').key?(name) }, 'prepare must keep candidate component state separate')
prepare_steps = prepare.fetch('steps')
checkout_index = prepare_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
guard_index = prepare_steps.index { |step| step['run']&.include?('release-context-guard.sh') }
assert.call(!checkout_index.nil? && !guard_index.nil? && checkout_index < guard_index, 'release context helper must run after checkout')
assert.call(prepare_steps.fetch(guard_index).fetch('run').include?('git rev-parse HEAD'), 'release context helper must compare the checked-out HEAD to GitHub SHA')
desktop_compute = prepare_steps.find { |step| step['id'] == 'compute' }
android_compute = prepare_steps.find { |step| step['id'] == 'android-compute' }
assert.call(desktop_compute.fetch('run').include?('release-component-state.sh discover desktop "$GITHUB_SHA" "$VERSION_INPUT"'), 'desktop compute collision must discover a candidate for both push and manual releases')
assert.call(android_compute.fetch('run').include?("release-component-state.sh discover android \"$GITHUB_SHA\" ''"), 'Android compute collision must discover a candidate independently')
Dir.mktmpdir('peppy-release-prepare') do |fixture|
  FileUtils.mkdir_p(File.join(fixture, '.github', 'scripts'))
  FileUtils.mkdir_p(File.join(fixture, 'infra', 'release'))
  FileUtils.cp(File.join(__dir__, 'release-component-state.sh'), File.join(fixture, '.github', 'scripts'))
  FileUtils.cp(File.join(root, '..', 'infra', 'release', 'lib.sh'), File.join(fixture, 'infra', 'release'))
  bin = File.join(fixture, 'bin')
  FileUtils.mkdir_p(bin)
  compute = File.join(fixture, 'infra', 'release', 'compute-version.sh')
  File.write(compute, "#!/usr/bin/env bash\nexit 4\n")
  File.chmod(0o755, compute)
  %w[user.email fixture@example.invalid user.name fixture].each_slice(2) do |key, value|
    _, stderr, status = Open3.capture3('git', '-C', fixture, 'init', '--quiet') if key == 'user.email'
    assert.call(status.success?, "prepare fixture git init failed: #{stderr}") if key == 'user.email'
    _, stderr, status = Open3.capture3('git', '-C', fixture, 'config', key, value)
    assert.call(status.success?, "prepare fixture git config failed: #{stderr}")
  end
  File.write(File.join(fixture, 'fixture'), "candidate\n")
  _, stderr, status = Open3.capture3('git', '-C', fixture, 'add', 'fixture')
  assert.call(status.success?, "prepare fixture git add failed: #{stderr}")
  _, stderr, status = Open3.capture3('git', '-C', fixture, 'commit', '--quiet', '-m', 'candidate')
  assert.call(status.success?, "prepare fixture git commit failed: #{stderr}")
  sha, stderr, status = Open3.capture3('git', '-C', fixture, 'rev-parse', 'HEAD')
  assert.call(status.success?, "prepare fixture SHA lookup failed: #{stderr}")
  sha = sha.strip
  _, stderr, status = Open3.capture3('git', '-C', fixture, 'tag', 'v1.2.3', sha)
  assert.call(status.success?, "prepare fixture tag failed: #{stderr}")
  output = File.join(fixture, 'github-output')
  environment = { 'PATH' => "#{bin}:#{ENV.fetch('PATH')}", 'GITHUB_OUTPUT' => output, 'GITHUB_SHA' => sha, 'GITHUB_EVENT_NAME' => 'workflow_dispatch', 'VERSION_INPUT' => '', 'BUMP' => 'auto' }
  _, stderr, status = Open3.capture3(environment, 'bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', desktop_compute.fetch('run'), chdir: fixture)
  assert.call(status.success? && File.read(output).include?("version=1.2.3\nexisting=true\nskip=false\n"), "prepare collision recovery must execute the extracted manual-dispatch shell: #{stderr}")
end

build = load_workflow.call('build-artifacts.yml')
desktop_steps = build.fetch('jobs').fetch('desktop-development-artifacts').fetch('steps')
desktop_checkout_index = desktop_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
desktop_guard_index = desktop_steps.index { |step| step['run']&.include?('release-context-guard.sh') }
assert.call(!desktop_checkout_index.nil? && !desktop_guard_index.nil? && desktop_checkout_index < desktop_guard_index, 'artifact context helper must run after checkout')

publisher = load_workflow.call('publish-release.yml')
publisher_steps = publisher.fetch('jobs').fetch('publish').fetch('steps')
publisher_permissions = publisher.fetch('jobs').fetch('publish').fetch('permissions')
assert.call(publisher_permissions == { 'contents' => 'write', 'attestations' => 'write', 'id-token' => 'write' }, 'publisher must hold only release and provenance permissions')
publisher_checkout_index = publisher_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
publisher_guard_index = publisher_steps.index { |step| step['run'] == 'bash .github/scripts/release-context-guard.sh' }
assert.call(!publisher_checkout_index.nil? && !publisher_guard_index.nil? && publisher_checkout_index < publisher_guard_index, 'publish context helper must run after checkout')
attestation_index = publisher_steps.index { |step| step['uses']&.start_with?('actions/attest-build-provenance@') }
assert.call(!attestation_index.nil? && publisher_guard_index < attestation_index, 'release artifacts must be attested only after the trusted context guard')
assert.call(publisher_steps.fetch(attestation_index).fetch('if') == "steps.guard.outputs.skip != 'true'", 'skipped releases must not mint provenance credentials')
sbom_index = publisher_steps.index { |step| step['uses']&.start_with?('anchore/sbom-action@') }
sbom_attestation_index = publisher_steps.index { |step| step['uses']&.start_with?('actions/attest-sbom@') }
assert.call(!sbom_index.nil? && publisher_guard_index < sbom_index, 'SBOM generation must run only after the trusted context guard')
assert.call(!sbom_attestation_index.nil? && sbom_index < sbom_attestation_index, 'staged release artifacts must receive the generated SBOM attestation')
assert.call(publisher_steps.fetch(sbom_attestation_index).fetch('with').fetch('subject-checksums') == 'dist/STAGED-ARTIFACT-SHA256SUMS', 'SBOM attestation must bind the immutable staged-artifact digest list')
sbom_generator = publisher_steps.fetch(sbom_index)
assert.call(sbom_generator.fetch('with').fetch('path') == 'dist/release', 'SBOM generation must scan only staged release artifacts')
assert.call(sbom_generator.fetch('with').fetch('upload-artifact') == false && sbom_generator.fetch('with').fetch('upload-release-assets') == false, 'SBOM generation must not alter release assets or upload an unaudited artifact')
publisher_steps.each_with_index.select { |step, _index| step['uses']&.start_with?('anchore/sbom-action@') }.each do |step, index|
  output_file = step.fetch('with').fetch('output-file')
  output_directory_step = publisher_steps.fetch(index - 1)
  assert.call(output_directory_step.fetch('name') == 'Create staged release artifact SBOM directory', 'SBOM generation must create its output directory immediately before running')
  assert.call(output_directory_step.fetch('if') == step.fetch('if'), 'SBOM output directory creation must use the SBOM guard')
  assert.call(Shellwords.shellsplit(output_directory_step.fetch('run')) == ['mkdir', '-p', File.dirname(output_file)], 'SBOM output directory creation must make the parent of the configured output file')
end
desktop_download = publisher_steps.find { |step| step.dig('with', 'pattern') == 'desktop-*-release-artifacts' }
android_download = publisher_steps.find { |step| step.dig('with', 'name') == 'android-release-artifacts' }
assert.call(!desktop_download.nil? && desktop_download.fetch('if').include?("inputs.component == 'desktop'"), 'desktop publisher must download only desktop release artifacts')
assert.call(!desktop_download.dig('with', 'pattern').include?('android-release-artifacts'), 'desktop download pattern must never match Android release artifacts')
assert.call(!android_download.nil? && android_download.fetch('if').include?("inputs.component == 'android'"), 'Android publisher must download exactly Android release artifacts')
# A by-name download extracts straight into path; only pattern downloads add a per-artifact directory.
assert.call(android_download.dig('with', 'path') == 'dist/download/android-release-artifacts', 'Android download must land where stage-assets.sh reads android-release-artifacts')

publisher_call_permissions = { 'contents' => 'write', 'attestations' => 'write', 'id-token' => 'write' }
%w[publish-prerelease publish-prerelease-android].each do |job_name|
  assert.call(ci_jobs.fetch(job_name).fetch('permissions') == publisher_call_permissions, "#{job_name} must grant the reusable publisher provenance permissions")
end
%w[publish publish-android].each do |job_name|
  assert.call(release.fetch('jobs').fetch(job_name).fetch('permissions') == publisher_call_permissions, "#{job_name} must grant the nested publisher provenance permissions")
end

trusted_contexts = 'workflow_dispatch:refs/heads/production:stable|push:refs/heads/production:stable|push:refs/heads/staging:staging'
helper = File.read(File.join(__dir__, 'release-context-guard.sh'))
assert.call(helper.include?(trusted_contexts), 'context helper must define the trusted context matrix')
assert.call(helper.include?('actual_sha=$(git rev-parse HEAD)'), 'context helper must verify the checked-out revision before protected repository code runs')
assert.call(desktop_steps.fetch(desktop_guard_index - 2).fetch('run').include?(trusted_contexts), 'artifact pre-check must match the context helper matrix')
assert.call(publisher_steps.fetch(publisher_guard_index - 2).fetch('run').include?(trusted_contexts), 'publish pre-check must match the context helper matrix')
Dir.mktmpdir('peppy-release-context') do |fixture|
  Open3.capture3('git', 'init', '--quiet', fixture).then { |_, stderr, status| assert.call(status.success?, "release context fixture git init failed: #{stderr}") }
  File.write(File.join(fixture, 'fixture'), "trusted\n")
  %w[user.email fixture@example.invalid user.name fixture].each_slice(2) do |key, value|
    _, stderr, status = Open3.capture3('git', '-C', fixture, 'config', key, value)
    assert.call(status.success?, "release context fixture git config failed: #{stderr}")
  end
  _, stderr, status = Open3.capture3('git', '-C', fixture, 'add', 'fixture')
  assert.call(status.success?, "release context fixture git add failed: #{stderr}")
  _, stderr, status = Open3.capture3('git', '-C', fixture, 'commit', '--quiet', '-m', 'fixture')
  assert.call(status.success?, "release context fixture git commit failed: #{stderr}")
  trusted_sha, stderr, status = Open3.capture3('git', '-C', fixture, 'rev-parse', 'HEAD')
  assert.call(status.success?, "release context fixture SHA lookup failed: #{stderr}")
  trusted_env = {
    'CHANNEL' => 'stable', 'REQUESTED_SHA' => trusted_sha.strip, 'GITHUB_SHA' => trusted_sha.strip,
    'GITHUB_EVENT_NAME' => 'workflow_dispatch', 'GITHUB_REF' => 'refs/heads/production'
  }
  _, stderr, status = Open3.capture3(trusted_env, 'bash', File.join(__dir__, 'release-context-guard.sh'), chdir: fixture)
  assert.call(status.success?, "trusted release context must pass: #{stderr}")
  _, _, status = Open3.capture3(trusted_env.merge('REQUESTED_SHA' => 'arbitrary-ref'), 'bash', File.join(__dir__, 'release-context-guard.sh'), chdir: fixture)
  assert.call(!status.success?, 'arbitrary release refs must fail before protected checkout work')
end

image_needs = release.fetch('jobs').fetch('publish-server-image').fetch('needs')
assert.call(!release.fetch('jobs').key?('container'), 'release must not rebuild container images after the CI gate')
assert.call(image_needs.sort == %w[artifacts prepare signing], 'stable server image promotion must wait for trusted artifacts')
relay_needs = release.fetch('jobs').fetch('publish-relay-image').fetch('needs')
assert.call(relay_needs.sort == %w[artifacts prepare signing], 'stable relay image promotion must wait for trusted artifacts')
mcp_needs = release.fetch('jobs').fetch('publish-mcp-image').fetch('needs')
assert.call(mcp_needs.sort == %w[artifacts prepare signing], 'stable MCP image promotion must wait for trusted artifacts')
mcp_image = release.fetch('jobs').fetch('publish-mcp-image').fetch('with')
assert.call(mcp_image.fetch('image') == 'hub.docker.visnovsky.us/library/peppy-mcp' && mcp_image.fetch('dockerfile') == 'infra/docker/mcp.Dockerfile' && mcp_image.fetch('context') == '.', 'stable MCP image must publish peppy-mcp from its Dockerfile at the repository root')

signing = release.fetch('jobs').fetch('signing')
assert.call(signing.fetch('environment') == 'updates-stable', 'signing readiness must resolve protected stable environment inputs')
signing_steps = signing.fetch('steps')
signing_checkout_index = signing_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
signing_helper_index = signing_steps.index { |step| step['run']&.include?('release-signing-readiness.sh') }
assert.call(!signing_checkout_index.nil? && !signing_helper_index.nil? && signing_checkout_index < signing_helper_index, 'signing readiness helper must run after checkout')
signing_env = signing_steps.fetch(signing_helper_index).fetch('env')
assert.call(signing_env.fetch('PEPPY_MACOS_SIGNING_MODE').include?("vars.PEPPY_MACOS_SIGNING_MODE || 'developer-id'"), 'stable readiness must default an absent macOS mode to strict')
assert.call(signing_env.fetch('PEPPY_WINDOWS_SIGNING_MODE').include?("vars.PEPPY_WINDOWS_SIGNING_MODE || 'authenticode'"), 'stable readiness must default an absent Windows mode to strict')
assert.call(signing.fetch('outputs').keys.sort == %w[android_verified desktop_verified ready], 'protected signing must expose only readiness and verified existing component state')
signing_checkout = signing_steps.fetch(signing_checkout_index)
assert.call(signing_checkout.dig('with', 'ref') == '${{ needs.prepare.outputs.sha }}', 'protected signing must pin checkout to the prepared SHA')
signing_context_guard_index = signing_steps.index { |step| step['name'] == 'Verify checked-out trusted signing context' }
signing_manifest_index = signing_steps.index { |step| step['run'] == 'cargo build --quiet --locked --manifest-path infra/release/manifest-verifier/Cargo.toml' }
signing_android_tools_index = signing_steps.index { |step| step['name'] == 'Prepare Android verification tools' }
assert.call(!signing_context_guard_index.nil? && signing_checkout_index < signing_context_guard_index && signing_context_guard_index < signing_manifest_index, 'signing must guard its pinned checkout before protected inputs')
assert.call(signing_steps.fetch(signing_context_guard_index).dig('env', 'CHANNEL') == 'stable' && signing_steps.fetch(signing_context_guard_index).dig('env', 'REQUESTED_SHA') == '${{ needs.prepare.outputs.sha }}', 'signing context guard must validate the prepared stable SHA')
assert.call(!signing_manifest_index.nil? && !signing_android_tools_index.nil? && signing_manifest_index < signing_helper_index && signing_android_tools_index < signing_helper_index, 'signing must prepare read-only desktop and Android verifiers before private key mapping')
existing_verification = signing_steps.find { |step| step['id'] == 'existing' }
assert.call(existing_verification&.fetch('run')&.include?('release-component-state.sh verify desktop') && existing_verification.fetch('run').include?('release-component-state.sh verify android'), 'signing must verify existing component candidates with the maintained helper')
assert.call(existing_verification.fetch('if').include?("steps.readiness.outputs.ready == 'true'") && existing_verification.dig('env', 'GH_TOKEN') == '${{ github.token }}', 'existing verification must wait for readiness and receive only its read-only GitHub token')

release_artifacts = release.fetch('jobs').fetch('artifacts')
assert.call(release_artifacts.fetch('needs').sort == %w[prepare signing], 'trusted artifacts must wait for signing readiness')
assert.call(release_artifacts.fetch('if').include?("needs.signing.outputs.ready == 'true'"), 'trusted artifacts must require ready signing inputs')
assert.call(release_artifacts.fetch('if').include?("needs.signing.outputs.desktop_verified == 'true'") && release_artifacts.fetch('if').include?("needs.signing.outputs.android_verified == 'true'"), 'artifact work must require a positive verification for every candidate')
assert.call(release_artifacts.fetch('if').include?("needs.prepare.outputs.desktop_existing != 'true' || (needs.prepare.outputs.android_skip != 'true' && needs.prepare.outputs.android_existing != 'true')"), 'both complete or Android-ineligible desktop recovery must not call an all-skipped artifact workflow')
assert.call(release_artifacts.fetch('if').include?('!cancelled()'), 'trusted artifacts must not begin after cancellation')
assert.call(release_artifacts.fetch('with').fetch('build_desktop') == "${{ needs.prepare.outputs.desktop_existing != 'true' }}", 'stable recovery must skip desktop artifacts only after a candidate is separately verified')
assert.call(release_artifacts.fetch('with').fetch('build_ios') == "${{ needs.prepare.outputs.desktop_existing != 'true' }}", 'stable recovery must skip iOS artifacts with verified desktop reuse')
assert.call(release_artifacts.fetch('with').fetch('build_android') == "${{ needs.prepare.outputs.android_skip != 'true' && needs.prepare.outputs.android_existing != 'true' }}", 'stable recovery must build only missing eligible Android artifacts')
assert.call(release.fetch('jobs').fetch('publish').fetch('if').include?("needs.prepare.outputs.desktop_existing != 'true'"), 'desktop publisher must skip verified existing desktop releases')
%w[publish-server-image publish-relay-image publish-mcp-image].each do |job_name|
  assert.call(release.fetch('jobs').fetch(job_name).fetch('if').include?("needs.prepare.outputs.desktop_existing != 'true'"), "#{job_name} must skip during desktop recovery")
end
assert.call(!ci_jobs.fetch('stable-release').fetch('if').include?('head_released'), 'outer CI must not suppress a same-SHA component recovery')

publish_needs = release.fetch('jobs').fetch('publish').fetch('needs')
assert.call(publish_needs.sort == %w[artifacts prepare publish-mcp-image publish-relay-image publish-server-image signing], 'stable publish must wait for trusted artifacts and all image publishers')
assert.call(release.fetch('jobs').fetch('publish').fetch('if').include?('!cancelled()'), 'release publishing must not begin after cancellation')
%w[publish-server-image publish-relay-image publish-mcp-image].each do |job_name|
  assert.call(release.fetch('jobs').fetch(job_name).fetch('if').include?('!cancelled()'), "#{job_name} must not begin after cancellation")
end

artifacts = load_workflow.call('build-artifacts.yml').fetch('jobs')
artifact_workflow_text = File.read(File.join(root, 'workflows', 'build-artifacts.yml'))
assert.call(!artifact_workflow_text.match?(%r{uses:\s*(actions/cache(/\w+)?|Swatinem/rust-cache)@}), 'build-artifacts must not restore or save Actions caches: the 10 GB repository cache limit evicts gate-critical caches')
%w[android desktop ios].each do |component|
  job = artifacts.fetch("#{component}-development-artifacts")
  assert.call(job.fetch('if') == "inputs.build_#{component} != false", "#{component} artifact job must run unless explicitly disabled")
end
desktop_steps = artifacts.fetch('desktop-development-artifacts').fetch('steps')
desktop_precheck = desktop_steps.find { |step| step['name'] == 'Reject untrusted updater build context' }
desktop_checkout = desktop_steps.find { |step| step['uses']&.start_with?('actions/checkout@') }
desktop_guard = desktop_steps.find { |step| step['name'] == 'Verify checked-out trusted updater context' }
assert.call(desktop_precheck.fetch('if') == "inputs.trusted_update_build || inputs.macos_distribution == 'app-store'", 'App Store builds must validate trusted ref context before checkout')
assert.call(desktop_guard.fetch('if') == "inputs.trusted_update_build || inputs.macos_distribution == 'app-store'", 'App Store builds must verify checked-out HEAD before repository code receives secrets')
assert.call(desktop_steps.index(desktop_precheck) < desktop_steps.index(desktop_checkout) && desktop_steps.index(desktop_checkout) < desktop_steps.index(desktop_guard), 'trusted desktop context checks must bracket checkout')
desktop_matrix = artifacts.fetch('desktop-development-artifacts').dig('strategy', 'matrix')
assert.call(desktop_matrix.include?('fromJSON') && desktop_matrix.include?('"bundles":"app"') && desktop_matrix.include?('"path":"apps/desktop/src-tauri/target/release/bundle/macos/*.pkg"'), 'App Store builds must run only one macOS app/package matrix entry')
mode_validation = desktop_steps.find { |step| step['run'] == 'bash .github/scripts/release-signing-readiness.sh --validate-modes' }
assert.call(!mode_validation.nil?, 'trusted builds must validate both signing modes before platform work')
mode_env = mode_validation.fetch('env')
assert.call(mode_env.fetch('PEPPY_MACOS_SIGNING_MODE').include?("vars.PEPPY_MACOS_SIGNING_MODE || 'developer-id'"), 'trusted build policy validation must default an absent macOS mode to strict')
assert.call(mode_env.fetch('PEPPY_WINDOWS_SIGNING_MODE').include?("vars.PEPPY_WINDOWS_SIGNING_MODE || 'authenticode'"), 'trusted build policy validation must default an absent Windows mode to strict')
bundle_step = desktop_steps.find { |step| step['run'] == 'infra/release/build-desktop-bundles.sh' }
bundle_env = bundle_step.fetch('env')
assert.call(bundle_env.fetch('PEPPY_MACOS_SIGNING_MODE').include?('vars.PEPPY_MACOS_SIGNING_MODE'), 'desktop bundling must receive the macOS mode')
assert.call(bundle_env.fetch('PEPPY_WINDOWS_SIGNING_MODE').include?('vars.PEPPY_WINDOWS_SIGNING_MODE'), 'desktop bundling must receive the Windows mode')
windows_import = desktop_steps.find { |step| step['name'] == 'Import protected Authenticode certificate' }
windows_cleanup = desktop_steps.find { |step| step['name'] == 'Remove protected Authenticode certificate' }
assert.call(windows_import.fetch('if').include?("vars.PEPPY_WINDOWS_SIGNING_MODE != 'none'"), 'Windows none mode must skip certificate import')
assert.call(windows_cleanup.fetch('if').include?("vars.PEPPY_WINDOWS_SIGNING_MODE != 'none'"), 'Windows none mode must skip certificate cleanup')
mode_validation_index = desktop_steps.index(mode_validation)
windows_import_index = desktop_steps.index(windows_import)
bundle_step_index = desktop_steps.index(bundle_step)
assert.call(mode_validation_index < windows_import_index && mode_validation_index < bundle_step_index, 'policy validation must precede Windows certificate import and desktop bundling')
android = artifacts.fetch('android-development-artifacts').fetch('steps').find { |step| step.dig('with', 'name') == 'android-release-artifacts' }
ios = artifacts.fetch('ios-development-artifacts').fetch('steps').find { |step| step.dig('with', 'name')&.start_with?('ios-') }
assert.call(android.dig('with', 'name') == 'android-release-artifacts', 'trusted Android artifacts must use the release namespace')
assert.call(ios.dig('with', 'name').include?("inputs.trusted_update_build && 'release' || 'development'"), 'iOS artifacts must separate release and development namespaces')

notes_step = publisher_steps.find { |step| step['run'] == 'infra/release/release-notes.sh "$VERSION" > dist/notes.md' }
notes_env = notes_step.fetch('env')
assert.call(notes_env.fetch('TRUSTED_UPDATE_BUILD') == '${{ inputs.trusted_update_build }}', 'release notes must receive trusted-build status')
assert.call(notes_env.fetch('PEPPY_MACOS_SIGNING_MODE').include?("vars.PEPPY_MACOS_SIGNING_MODE || 'developer-id'"), 'release notes must receive the strict-default macOS mode')
assert.call(notes_env.fetch('PEPPY_WINDOWS_SIGNING_MODE').include?("vars.PEPPY_WINDOWS_SIGNING_MODE || 'authenticode'"), 'release notes must receive the strict-default Windows mode')
android_notes_step = publisher_steps.find { |step| step['run'] == 'infra/release/release-notes.sh "$VERSION" --component android > dist/notes.md' }
assert.call(android_notes_step&.dig('env', 'TRUSTED_UPDATE_BUILD') == '${{ inputs.trusted_update_build }}', 'Android release notes must receive trusted-build status')
publisher_group = publisher.fetch('jobs').fetch('publish').fetch('concurrency').fetch('group')
assert.call(!publisher.fetch('jobs').fetch('publish').key?('needs'), 'attestation must run in the protected publisher job without a second environment approval')
assert.call(publisher_group == "${{ inputs.component == 'android' && 'release-publish-android' || 'release-publish' }}", 'desktop publishing must keep the release-publish group while Android queues separately')

dev_artifacts_with = ci_jobs.fetch('development-artifacts').fetch('with')
assert.call(dev_artifacts_with.fetch('build_android') == "${{ (github.event_name == 'push' && (needs.version.outputs.android_changed == 'true' || needs.version.outputs.android_budget_refresh == 'true')) || (github.event_name != 'push' && needs.changes.outputs.artifacts_android == 'true') }}", 'pushes must build Android only when its release inputs changed or its budget needs a refresh')
android_publish_secrets = [ci_jobs.fetch('publish-prerelease-android'), release.fetch('jobs').fetch('publish-android')].map { |job| job.fetch('secrets').keys.sort }
assert.call(android_publish_secrets.all? { |keys| keys == %w[PEPPY_RELEASE_GPG_KEY PEPPY_RELEASE_GPG_PASSPHRASE] }, 'Android publishers must receive only the checksum GPG secrets')

read_workflow = ->(name) { File.read(File.join(root, 'workflows', name)) }
declared_secrets = lambda do |workflow, name|
  declarations = workflow.fetch(true).fetch('workflow_call').fetch('secrets', {})
  assert.call(declarations.values.all? { |spec| spec == { 'required' => false } }, "#{name} secrets must be declared optional")
  declarations.keys.sort
end
referenced_secrets = ->(text) { text.scan(/secrets\.([A-Za-z0-9_]+)/).flatten.uniq.sort }
artifact_secret_names = %w[
  APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_ID APPLE_MAS_CERTIFICATE_P12 APPLE_MAS_CERTIFICATE_PASSWORD APPLE_MAS_INSTALLER_CERTIFICATE
  APPLE_MAS_INSTALLER_IDENTITY APPLE_MAS_PROVISIONING_PROFILE_BASE64 APPLE_MAS_SIGNING_IDENTITY APPLE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_TEAM_ID
  PEPPY_ANDROID_KEYSTORE_BASE64 PEPPY_ANDROID_KEYSTORE_PASSWORD PEPPY_ANDROID_KEY_ALIAS PEPPY_ANDROID_KEY_PASSWORD
  PEPPY_RELEASE_RPM_GPG_KEY PEPPY_UPDATER_PRIVATE_KEY PEPPY_UPDATER_PRIVATE_KEY_PASSWORD
  WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD
].freeze
publish_secret_names = %w[
  PEPPY_RELEASE_GPG_KEY PEPPY_RELEASE_GPG_PASSPHRASE PEPPY_UPDATER_PRIVATE_KEY PEPPY_UPDATER_PRIVATE_KEY_PASSWORD
].freeze
named_secret_map = lambda do |job, names, caller_name|
  secrets = job['secrets']
  assert.call(secrets.is_a?(Hash), "#{caller_name} must pass secrets by explicit name, not inherit")
  assert.call(secrets.keys.sort == names, "#{caller_name} must map exactly the callee's declared secrets")
  names.each do |name|
    assert.call(secrets.fetch(name) == "${{ secrets.#{name} }}", "#{caller_name} must map #{name} by identity")
  end
end

build_text = read_workflow.call('build-artifacts.yml')
publish_text = read_workflow.call('publish-release.yml')
build_workflow = YAML.load(build_text)
publish_workflow = YAML.load(publish_text)
assert.call(declared_secrets.call(build_workflow, 'build-artifacts') == artifact_secret_names, 'build-artifacts must declare exactly its signing secrets')
assert.call(referenced_secrets.call(build_text) == artifact_secret_names, 'build-artifacts must not reference undeclared secrets')
assert.call(declared_secrets.call(publish_workflow, 'publish-release') == publish_secret_names, 'publish-release must declare exactly its signing secrets')
assert.call(referenced_secrets.call(publish_text) == publish_secret_names, 'publish-release must not reference undeclared secrets')

named_secret_map.call(release.fetch('jobs').fetch('artifacts'), artifact_secret_names, 'release artifacts')
named_secret_map.call(ci_jobs.fetch('development-artifacts'), artifact_secret_names, 'CI development artifacts')
named_secret_map.call(release.fetch('jobs').fetch('publish'), publish_secret_names, 'release publish')
named_secret_map.call(ci_jobs.fetch('publish-prerelease'), publish_secret_names, 'CI prerelease publish')

assert.call(build_workflow.fetch('jobs').fetch('desktop-development-artifacts').fetch('environment') == "${{ inputs.macos_distribution == 'app-store' && 'mac-app-store' || inputs.trusted_update_build && format('updates-{0}', inputs.update_channel) || 'development-artifacts' }}", 'App Store artifacts must use their protected environment without changing direct builds')
assert.call(publish_workflow.fetch('jobs').fetch('publish').fetch('environment') == "${{ inputs.channel == 'stable' && 'updates-stable' || inputs.channel == 'staging' && 'updates-staging' || 'invalid-update-channel' }}", 'publish environment selection must stay unchanged')
assert.call(ci_jobs.fetch('development-artifacts').fetch('with').fetch('trusted_update_build') == "${{ github.event_name == 'push' && github.ref == 'refs/heads/staging' }}", 'CI artifact trust predicate must stay unchanged')
ci_artifacts_with = ci_jobs.fetch('development-artifacts').fetch('with')
assert.call(ci_artifacts_with.fetch('build_desktop').include?("github.event_name == 'push' && github.ref == 'refs/heads/staging'"), 'trusted staging artifacts must include desktop')

android_job = artifacts.fetch('android-development-artifacts')
assert.call(android_job.fetch('environment') == "${{ inputs.trusted_update_build && format('updates-{0}', inputs.update_channel) || 'development-artifacts' }}", 'Android artifacts must use the trusted update environment')
android_steps = android_job.fetch('steps')
android_precheck = android_steps.find { |step| step['name'] == 'Reject untrusted Android build context' }
android_guard = android_steps.find { |step| step['name'] == 'Verify checked-out trusted Android context' }
android_validation = android_steps.find { |step| step['name'] == 'Validate protected Android signing inputs' }
android_stop_gradle = android_steps.find { |step| step['name'] == 'Stop Android Gradle daemon before signing' }
android_checkout = android_steps.find { |step| step['uses']&.start_with?('actions/checkout@') }
assert.call(!android_precheck.nil? && !android_guard.nil?, 'trusted Android builds must check context before and after checkout')
assert.call(android_precheck.fetch('run').include?(trusted_contexts), 'Android pre-check must match the trusted context matrix')
assert.call(!android_precheck.fetch('run').include?('BUILD_DESKTOP'), 'trusted Android builds must not couple protected Android work to desktop artifacts')
android_sign = android_steps.find { |step| step['run'] == 'bash infra/release/sign-android-artifacts.sh dist/android-release/peppy-android-unsigned.apk dist/android-release/peppy-android-unsigned.aab dist/android-release' }
assert.call(!android_sign.nil?, 'trusted Android builds must sign release artifacts after building')
trusted_android_build = android_steps.find { |step| step['name'] == 'Build trusted Android release artifacts' }
android_artifacts_go = android_steps.find { |step| step['uses']&.start_with?('actions/setup-go@') }
android_artifacts_aar = android_steps.find { |step| step['name'] == 'Build Google Messages AAR' }
assert.call(android_artifacts_go && android_artifacts_go.fetch('uses') == 'actions/setup-go@44694675825211faa026b3c33043df3e48a5fa00' && android_artifacts_go.fetch('with') == { 'go-version' => '1.27.3', 'cache' => false }, 'Android artifact builds must pin Go 1.27.3 without a shared cache')
assert.call(android_artifacts_aar && android_artifacts_aar.fetch('if') == 'inputs.trusted_update_build' && android_artifacts_aar.fetch('run').include?('export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"') && android_artifacts_aar.fetch('run').include?('apps/android/googlemessages/build-aar.sh') && android_artifacts_aar.fetch('run').include?('apps/android/googlemessages/check-licenses.sh'), 'trusted Android artifact builds must build and check the Google Messages AAR')
android_artifacts_sideload_steps = android_steps.select { |step| step['run']&.match?(/:app:\w*Sideload/) }
assert.call(!android_artifacts_sideload_steps.empty? && android_steps.index(android_artifacts_go) < android_steps.index(android_artifacts_aar) && android_artifacts_sideload_steps.all? { |step| android_steps.index(android_artifacts_aar) < android_steps.index(step) }, 'Android artifact builds must provision Go and build the AAR before every sideload Gradle task')
assert.call(trusted_android_build.fetch('run').include?(':app:assembleSideload') && trusted_android_build.fetch('run').include?(':app:verifySideloadGoogleMessagesLink') && trusted_android_build.fetch('run').include?(':app:bundleRelease'), 'trusted Android builds must assemble and verify the sideload APK and clean release AAB')
assert.call(!trusted_android_build.fetch('env').key?('PEPPY_GOOGLE_MESSAGES_LICENSE_SIGNOFF') && !trusted_android_build.fetch('run').include?('PEPPY_GOOGLE_MESSAGES_LICENSE_SIGNOFF') && !trusted_android_build.fetch('run').include?('google_messages_link_args') && trusted_android_build.fetch('run').include?('-PpeppyGoogleMessagesLink=true'), 'trusted Android builds must enable the Google Messages link directly without a sign-off variable or conditional argument branch')
assert.call(trusted_android_build.fetch('run').include?('app/build/outputs/apk/sideload/app-sideload-unsigned.apk'), 'trusted Android builds must stage the unsigned sideload APK')
assert.call(trusted_android_build.fetch('run').include?(':app:bundleRelease :app:mergeReleaseNativeDebugMetadata -PpeppyDistribution=play'), 'trusted Android builds must produce release native debug symbols with the Play bundle')
assert.call(!android_validation.nil? && !android_stop_gradle.nil?, 'trusted Android builds must validate inputs and stop Gradle before signing')
assert.call(android_stop_gradle.fetch('working-directory') == 'apps/android' && android_stop_gradle.fetch('run') == './gradlew --stop', 'trusted Android builds must stop Gradle in the Android project before signing')
assert.call(android_steps.index(android_precheck) < android_steps.index(android_checkout) && android_steps.index(android_checkout) < android_steps.index(android_guard) && android_steps.index(android_guard) < android_steps.index(android_validation) && android_steps.index(android_validation) < android_steps.index(android_stop_gradle) && android_steps.index(android_stop_gradle) < android_steps.index(android_sign), 'Android trust validation and Gradle shutdown must precede secret-scoped signing')
android_distribution_verification = android_steps.find { |step| step['name'] == 'Verify trusted Android distribution metadata and symbols' }
assert.call(android_distribution_verification && android_distribution_verification.fetch('env').fetch('PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED') == 'true' && android_distribution_verification.fetch('run').include?('verify-android-distribution.sh') && android_steps.index(android_sign) < android_steps.index(android_distribution_verification), 'trusted Android builds must expect the Google Messages link during final distribution verification after signing')
android_sign_env = android_sign.fetch('env')
%w[PEPPY_ANDROID_KEYSTORE_BASE64 PEPPY_ANDROID_KEYSTORE_PASSWORD PEPPY_ANDROID_KEY_ALIAS PEPPY_ANDROID_KEY_PASSWORD].each do |name|
  assert.call(android_sign_env.fetch(name) == "${{ inputs.trusted_update_build && secrets.#{name} || '' }}", "Android signer must receive #{name} only for trusted builds")
end
assert.call(android_sign_env.fetch('PEPPY_ANDROID_SIGNING_CERT_SHA256') == "${{ inputs.trusted_update_build && vars.PEPPY_ANDROID_SIGNING_CERT_SHA256 || '' }}", 'Android signer must receive the protected certificate fingerprint only for trusted builds')
assert.call(android_steps.any? { |step| step.dig('with', 'path') == 'dist/android-release/peppy-android-debug.apk' }, 'development Android artifact must contain only the debug APK')
assert.call(android_steps.any? { |step| step.dig('with', 'path') == "dist/android-release/peppy-android.apk\ndist/android-release/peppy-android.aab\ndist/android-release/peppy-android-native-debug-symbols.zip\n" }, 'release Android artifact must contain only signed assets and native debug symbols')
android_development_build = android_steps.find { |step| step['name'] == 'Build development Android debug APK' }
assert.call(android_development_build.fetch('env').fetch('PEPPY_ANDROID_NATIVE_PROFILE') == 'release', 'development Android builds must retain release native compilation')
assert.call(android_development_build.fetch('run').include?(':app:verifyReleaseManifestSecurity :app:assembleDebug :app:assembleSideload :app:verifySideloadGoogleMessagesLink -PpeppyDistribution=direct'), 'development Android builds must verify the direct sideload link before producing artifacts')
assert.call(android_development_build.fetch('run').include?(':app:bundleRelease -PpeppyDistribution=play'), 'development Android builds must build the Play bundle only under the Play distribution')

stage_script = File.read(File.join(root, '..', 'infra', 'release', 'stage-assets.sh'))
assert.call(stage_script.include?("peppy-android.apk") && stage_script.include?("peppy-android.aab"), 'staging must require signed Android artifact names')
assert.call(!stage_script.include?('peppy-android-unsigned'), 'staging must reject old unsigned Android artifact names')
verify_step = publisher_steps.find { |step| step['run'] == 'bash infra/release/verify-android-artifacts.sh "dist/release/peppy-$VERSION-android.apk" "dist/release/peppy-$VERSION-android.aab"' }
assert.call(!verify_step.nil?, 'publisher must independently verify staged Android signatures')
assert.call(verify_step.fetch('env').fetch('PEPPY_ANDROID_SIGNING_CERT_SHA256') == "${{ inputs.trusted_update_build && vars.PEPPY_ANDROID_SIGNING_CERT_SHA256 || '' }}", 'publisher must use the protected certificate fingerprint')
assert.call(verify_step.fetch('env').fetch('VERSION') == '${{ inputs.version }}', 'publisher Android verification must receive the release version')
publisher_java = publisher_steps.find { |step| immutable_action_ref.call(step['uses'], 'actions/setup-java') }
assert.call(!publisher_java.nil?, 'publisher must select JDK 17 via an immutable setup-java commit SHA before Android signature verification')
assert.call(publisher_java.fetch('with') == { 'distribution' => 'temurin', 'java-version' => '17' }, 'publisher must use pinned Temurin JDK 17')
assert.call(publisher_steps.index(publisher_java) < publisher_steps.index(verify_step), 'publisher JDK setup must precede Android signature verification')
publisher_stage = publisher_steps.find { |step| step['run'] == 'infra/release/stage-assets.sh --group android "$VERSION" "$CHANNEL" dist/download dist/release' }
publisher_desktop_stage = publisher_steps.find { |step| step['run'] == 'infra/release/stage-assets.sh --group desktop "$VERSION" "$CHANNEL" dist/download dist/release' }
publisher_updater_sign = publisher_steps.find { |step| step['run'] == 'infra/release/sign-update-assets.sh "$VERSION" "$CHANNEL" dist/release' }
publisher_checksums = publisher_steps.find { |step| step['run'] == 'infra/release/write-checksums.sh dist/release' }
publisher_android_create = publisher_steps.find { |step| step['run']&.include?('gh release create "${TAG_PREFIX}$VERSION"') }
publisher_android_edit = publisher_steps.find { |step| step['run']&.include?('gh release edit "${TAG_PREFIX}$VERSION"') }
publisher_desktop_create = publisher_steps.find { |step| step['run']&.include?('gh release create "v$VERSION"') }
publisher_desktop_edit = publisher_steps.find { |step| step['run']&.include?('gh release edit "v$VERSION" --draft=false --latest') }
publisher_context_guard = publisher_steps.find { |step| step['run'] == 'bash .github/scripts/release-context-guard.sh' }
publisher_existing_guard = publisher_steps.find { |step| step['id'] == 'guard' }
assert.call(!publisher_stage.nil? && !publisher_desktop_stage.nil? && !publisher_updater_sign.nil? && !publisher_checksums.nil? && !publisher_android_create.nil? && !publisher_desktop_create.nil?, 'publisher must retain component staging, desktop updater signing, checksums, and release creation')
assert.call(publisher_stage.fetch('if').include?("inputs.component == 'android'") && publisher_desktop_stage.fetch('if').include?("inputs.component == 'desktop'"), 'publisher must stage the selected component only')
assert.call(publisher_steps.index(publisher_context_guard) < publisher_steps.index(publisher_stage) && publisher_steps.index(publisher_stage) < publisher_steps.index(verify_step) && publisher_steps.index(verify_step) < publisher_steps.index(publisher_checksums) && publisher_steps.index(verify_step) < publisher_steps.index(publisher_android_create), 'publisher must verify Android assets after Android staging and before checksums or Android release creation')
assert.call(publisher_steps.index(publisher_desktop_stage) < publisher_steps.index(publisher_updater_sign) && publisher_steps.index(publisher_updater_sign) < publisher_steps.index(publisher_desktop_create), 'desktop publisher must sign update assets before desktop release creation')
assert.call(publisher_android_create.fetch('run').include?('--latest=false') && publisher_android_edit.fetch('run').scan('--latest=false').length == 2, 'Android release creation and edits must never mark Android as latest')
assert.call(publisher_android_create.fetch('run').include?('"${TAG_PREFIX}$VERSION"') && publisher_android_edit.fetch('run').include?('"${TAG_PREFIX}$VERSION"'), 'Android release commands must use the android-v tag prefix')
assert.call(publisher.fetch('jobs').fetch('publish').dig('env', 'TAG_PREFIX') == "${{ inputs.component == 'android' && 'android-v' || 'v' }}", 'Android release commands must resolve to android-v tags')
assert.call(publisher_desktop_create.fetch('run').include?('"v$VERSION"') && publisher_desktop_edit.fetch('run').include?('--latest'), 'desktop release create/edit commands must retain the desktop tag and stable latest flag')
assert.call(publisher_existing_guard.fetch('run').include?('release-component-state.sh verify "$COMPONENT" "$VERSION" "$SHA"'), 'serialized stable publisher must revalidate an existing component before skipping writes')
assert.call(publisher_existing_guard.fetch('run').include?('remote_tags=$(git ls-remote --tags origin') && publisher_existing_guard.fetch('run').include?('releases=$(gh api --paginate'), 'serialized stable publisher must distinguish absent remote state from an existing invocation before verification')
assert.call(publisher_existing_guard.fetch('run').include?('if [ "$CHANNEL" = staging ]; then') && publisher_existing_guard.fetch('run').include?('gh api -X DELETE'), 'staging draft recovery must remain isolated from stable existing-release handling')
stable_cleanup = publisher_steps.find { |step| step['if'] == "failure() && steps.guard.outputs.skip != 'true' && inputs.channel == 'staging'" }
assert.call(!stable_cleanup.nil?, 'only staging may retain draft cleanup after a failed publish')
Dir.mktmpdir('peppy-stable-publish-guard') do |fixture|
  bin = File.join(fixture, 'bin')
  FileUtils.mkdir_p(bin)
  File.write(File.join(bin, 'git'), <<~'SH')
    #!/usr/bin/env bash
    set -euo pipefail
    [ "$1" = ls-remote ] || exit 97
    printf '%s\n' "$*" >> "$GIT_LOG"
  SH
  File.write(File.join(bin, 'gh'), <<~'SH')
    #!/usr/bin/env bash
    set -euo pipefail
    printf '%s\n' "$*" >> "$GH_LOG"
    [ "$1" = api ] && [ "$2" = --paginate ] && [ "$3" = "repos/example/peppy/releases?per_page=100" ] && [ "$#" -eq 3 ] || exit 98
    [ "${GH_MODE:-empty}" = empty ] || exit 99
    printf '[]\n'
  SH
  [File.join(bin, 'git'), File.join(bin, 'gh')].each { |path| File.chmod(0o755, path) }
  output = File.join(fixture, 'github-output')
  environment = {
    'PATH' => "#{bin}:#{ENV.fetch('PATH')}", 'GIT_LOG' => File.join(fixture, 'git.log'), 'GH_LOG' => File.join(fixture, 'gh.log'),
    'GITHUB_OUTPUT' => output, 'GITHUB_REPOSITORY' => 'example/peppy', 'CHANNEL' => 'stable', 'VERSION' => '1.2.3',
    'COMPONENT' => 'desktop', 'SHA' => 'a' * 40, 'GH_TOKEN' => 'fixture-token'
  }
  _, stderr, status = Open3.capture3(environment, 'bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', publisher_existing_guard.fetch('run'), chdir: root)
  assert.call(status.success? && File.read(output) == "skip=false\n", "fresh stable publisher guard must use the strict GH CLI contract: #{stderr}")
  assert.call(!File.read(File.join(fixture, 'gh.log')).include?('--arg'), 'stable publisher guard must pass jq arguments only to standalone jq')
  _, _, failed_status = Open3.capture3(environment.merge('GH_MODE' => 'api-error'), 'bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', publisher_existing_guard.fetch('run'), chdir: root)
  assert.call(!failed_status.success?, 'stable publisher guard must fail closed on release API errors')
end
publisher_prune = publisher_steps.find { |step| step['run']&.include?('prune-prereleases.sh') }
assert.call(publisher_prune.fetch('run').include?('--prefix "$TAG_PREFIX"'), 'prerelease pruning must stay scoped to the component tag prefix')

android_prerelease = ci_jobs.fetch('publish-prerelease-android')
assert.call(android_prerelease.fetch('needs').sort == %w[development-artifacts gate publish-prerelease version], 'Android prerelease publishing must wait for desktop prerelease publication')
assert.call(android_prerelease.fetch('with').fetch('component') == 'android', 'Android prerelease publishing must select the Android component')
assert.call(android_prerelease.fetch('if').include?("needs.gate.result == 'success'"), 'Android prerelease publishing must require a successful CI gate')
android_stable = release.fetch('jobs').fetch('publish-android')
assert.call(android_stable.fetch('needs').sort == %w[artifacts prepare publish signing], 'Android stable publishing must wait for desktop stable publication')
assert.call(android_stable.fetch('with').fetch('component') == 'android', 'Android stable publishing must select the Android component')
assert.call(prepare_steps.any? { |step| step['run']&.include?(".name == \"gate\" and .conclusion == \"success\"") }, 'stable Android publishing must inherit the successful CI gate requirement')
release_publish_android_condition = android_stable.fetch('if')
release_android_context = {
  event: 'workflow_dispatch', ref: 'refs/heads/production', cancelled: false, implicit_success: false,
  results: { 'prepare' => 'success', 'signing' => 'success', 'artifacts' => 'success', 'publish' => 'success' },
  outputs: {
    'prepare.skip' => false, 'prepare.android_skip' => false, 'prepare.android_existing' => false,
    'prepare.desktop_existing' => false, 'signing.ready' => true, 'signing.desktop_verified' => false
  }, inputs: { 'dry_run' => false }
}.freeze
assert.call(condition_allows.call(release_publish_android_condition, **release_android_context), 'publish-android must run for an ordinary successful stable release')
recovery_context = release_android_context.merge(results: release_android_context.fetch(:results).merge('publish' => 'skipped'), outputs: release_android_context.fetch(:outputs).merge('prepare.desktop_existing' => true, 'signing.desktop_verified' => true))
assert.call(condition_allows.call(release_publish_android_condition, **recovery_context), 'publish-android must follow a positively verified skipped desktop publisher')
%w[failure cancelled skipped].each do |result|
  assert.call(!condition_allows.call(release_publish_android_condition, **release_android_context.merge(results: release_android_context.fetch(:results).merge('prepare' => result))), "publish-android must require successful preparation, not #{result}")
  assert.call(!condition_allows.call(release_publish_android_condition, **release_android_context.merge(results: release_android_context.fetch(:results).merge('signing' => result))), "publish-android must require successful signing, not #{result}")
  assert.call(!condition_allows.call(release_publish_android_condition, **release_android_context.merge(results: release_android_context.fetch(:results).merge('artifacts' => result))), "publish-android must require successful artifacts, not #{result}")
end
assert.call(!condition_allows.call(release_publish_android_condition, **release_android_context.merge(inputs: { 'dry_run' => true })), 'publish-android must not run during dry runs')
assert.call(!condition_allows.call(release_publish_android_condition, **release_android_context.merge(outputs: release_android_context.fetch(:outputs).merge('prepare.android_skip' => true))), 'publish-android must not run when Android is ineligible')
assert.call(!condition_allows.call(release_publish_android_condition, **release_android_context.merge(results: release_android_context.fetch(:results).merge('publish' => 'skipped'))), 'publish-android must not accept an unverified skipped desktop publisher')

workflow_text = Dir[File.join(root, 'workflows', '*.{yml,yaml}')].sort.map { |path| File.read(path) }.join("\n")
assert.call(!workflow_text.include?('peppyGoogleMessagesLink=false'), 'workflows must not disable the Google Messages link with a default-off flag')
assert.call(!workflow_text.include?('PEPPY_GOOGLE_MESSAGES_LICENSE_SIGNOFF'), 'workflows must not retain the obsolete Google Messages licensing sign-off variable')
android_build_script = File.read(File.join(root, '..', 'apps', 'android', 'app', 'build.gradle.kts'))
assert.call(android_build_script.include?('tasks.register("verifyReleaseGoogleMessagesLink")'), 'release builds must verify Google Messages link presence or absence')
workflow_text.each_line.select { |line| line.include?(':app:bundleRelease') }.each do |line|
  assert.call(line.include?('-PpeppyDistribution=play'), 'every Play bundle invocation must explicitly select the Play distribution')
end
assert.call(!workflow_text.match?(/PEPPY_ANDROID_SKIP|skip_unchanged|SKIP_UNCHANGED/i), 'workflows must not expose an Android skip override')
workflow_documents = Dir[File.join(root, 'workflows', '*.{yml,yaml}')].map { |path| YAML.load_file(path) }
workflow_documents.each do |workflow|
  dispatch_inputs = workflow.dig(true, 'workflow_dispatch', 'inputs') || {}
  assert.call(dispatch_inputs.keys.none? { |name| name.match?(/(?:force|skip).*android|android.*(?:force|skip)/i) }, 'workflows must not expose a force/skip Android dispatch input')
end

release_tooling = ci_jobs.fetch('release-tooling')
assert.call(release_tooling.fetch('timeout-minutes') == 20, 'release tooling needs Android fixture installation headroom')
release_tooling_steps = release_tooling.fetch('steps')
release_tooling_java = release_tooling_steps.find { |step| immutable_action_ref.call(step['uses'], 'actions/setup-java') }
release_tooling_sdk = release_tooling_steps.find { |step| step['name'] == 'Install Android signing test SDK tools' }
assert.call(!release_tooling_java.nil? && release_tooling_java.fetch('with') == { 'distribution' => 'temurin', 'java-version' => '17' }, 'release tooling must install Temurin JDK 17')
assert.call(!release_tooling_sdk.nil? && release_tooling_sdk.fetch('run').include?('platforms;android-35') && release_tooling_sdk.fetch('run').include?('build-tools;35.0.0'), 'release tooling must install the Android signing test SDK platform and build tools')
assert.call(release_tooling_steps.index(release_tooling_java) < release_tooling_steps.index(release_tooling_sdk) && release_tooling_steps.index(release_tooling_sdk) < release_tooling_steps.index(release_tooling_steps.find { |step| step['run'] == 'infra/release/test-android-signing.sh' }), 'release tooling must set up JDK and SDK before real Android signing tests')
assert.call(release_tooling_steps.none? { |step| step['run'] == 'infra/release/test-desktop-bundles.sh' }, 'Ubuntu release tooling must not run the native macOS plutil fixture')
release_macos_tooling = ci_jobs.fetch('release-macos-tooling')
assert.call(release_macos_tooling.fetch('runs-on') == 'macos-15' && release_macos_tooling.fetch('timeout-minutes') == 10, 'native release tooling must run on macOS')
release_macos_steps = release_macos_tooling.fetch('steps')
release_macos_node = release_macos_steps.find { |step| immutable_action_ref.call(step['uses'], 'actions/setup-node') }
assert.call(release_macos_node && release_macos_node.fetch('with') == { 'node-version-file' => '.node-version' }, 'native release tooling must provision the existing Node prerequisite without installing dependencies')
assert.call(release_macos_steps.any? { |step| step['run'] == 'infra/release/test-desktop-bundles.sh' }, 'native release tooling must run the real plutil desktop-bundle fixture')
gate = ci_jobs.fetch('gate')
assert.call(gate.fetch('needs').include?('release-macos-tooling'), 'the required CI gate must wait for native macOS release tooling')
gate_env = gate.fetch('steps').find { |step| step['name'] == 'Evaluate CI gate' }.fetch('env')
assert.call(gate_env == gate_env.merge('RELEASE_MACOS_TOOLING_RESULT' => '${{ needs.release-macos-tooling.result }}', 'RELEASE_MACOS_TOOLING_REQUIRED' => 'true'), 'the CI gate must require macOS native release tooling for every change, including infra/release paths')
trusted_android_build_steps = android_steps.select { |step| step['name'] == 'Build trusted Android release artifacts' }
assert.call(trusted_android_build_steps.length == 1, 'trusted Android builds must have exactly one release build step')
Dir.mktmpdir('peppy-android-release-test') do |fixture|
  FileUtils.mkdir_p(File.join(fixture, 'infra', 'compose'))
  FileUtils.mkdir_p(File.join(fixture, 'apps', 'android'))
  verifier = File.join(fixture, 'infra', 'compose', 'verify-android-native.sh')
  gradlew = File.join(fixture, 'apps', 'android', 'gradlew')
  capture = File.join(fixture, 'gradle-arguments.bin')
  File.write(verifier, "#!/usr/bin/env bash\nexit 0\n")
  File.write(gradlew, <<~'SH')
    #!/usr/bin/env bash
    set -euo pipefail
    { printf '%s\0' "$0"; for argument in "$@"; do printf '%s\0' "$argument"; done; } >> "$PEPPY_TEST_GRADLE_CAPTURE"
    printf 'END\0' >> "$PEPPY_TEST_GRADLE_CAPTURE"
    mkdir -p app/build/outputs/apk/sideload app/build/outputs/bundle/release app/build/outputs/native-debug-symbols/release
    : > app/build/outputs/apk/sideload/app-sideload-unsigned.apk
    : > app/build/outputs/bundle/release/app-release.aab
    : > app/build/outputs/native-debug-symbols/release/native-debug-symbols.zip
  SH
  [verifier, gradlew].each { |path| File.chmod(0o755, path) }

  step_env = ENV.to_h.merge(
    'ANDROID_HOME' => File.join(fixture, 'sdk'),
    'VERSION' => '1.2.3',
    'ANDROID_VERSION_CODE' => '45',
    'PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID' => 'test-server-client-id',
    'PEPPY_ANDROID_NATIVE_PROFILE' => 'release',
    'PEPPY_TEST_GRADLE_CAPTURE' => capture
  )
  stdout, stderr, status = Open3.capture3(
    step_env, 'bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', trusted_android_build_steps.first.fetch('run'),
    chdir: fixture
  )
  assert.call(status.success?, "trusted Android release step must succeed against the fake checkout: #{stdout}#{stderr}")

  tokens = File.binread(capture).force_encoding(Encoding::UTF_8).split("\0")
  invocations = [[]]
  tokens.each { |token| token == 'END' ? invocations << [] : invocations.last << token }
  invocations.pop
  assert.call(invocations.length == 2, 'trusted Android release build must invoke Gradle exactly twice')

  expected_options = [
    '--console=plain', '--dependency-verification=strict', '--stacktrace',
    '-Dorg.gradle.jvmargs=-Xmx4096m -Dfile.encoding=UTF-8', '--max-workers=2'
  ].sort
  direct_invocation, play_invocation = invocations
  [
    [direct_invocation, 'direct', %w[:app:verifyReleaseManifestSecurity :app:lintSideload :app:assembleSideload :app:verifySideloadGoogleMessagesLink]],
    [play_invocation, 'play', %w[:app:verifyReleaseManifestSecurity :app:lintRelease :app:bundleRelease :app:mergeReleaseNativeDebugMetadata :app:verifyReleaseGoogleMessagesLink]]
  ].each do |arguments, distribution, expected_tasks|
    assert.call(arguments.first == './gradlew', "trusted Android #{distribution} build must run the project gradlew")
    options = arguments.drop(1).reject { |token| token.start_with?(':', '-P') }
    assert.call(options.sort == expected_options, "trusted Android #{distribution} build must use strict verification, one 4 GiB UTF-8 JVM argument, two workers, and stacktraces")
    assert.call(arguments.select { |token| token.start_with?(':') } == expected_tasks, "trusted Android #{distribution} build must run exactly its release tasks")
    assert.call(arguments.count { |token| token.start_with?('-PpeppyDistribution=') } == 1 && arguments.include?("-PpeppyDistribution=#{distribution}"), "trusted Android #{distribution} build must select the #{distribution} distribution")
    assert.call(arguments.include?('-PpeppyVersionName=1.2.3') && arguments.include?('-PpeppyVersionCode=45'), "trusted Android #{distribution} build must receive the release version arguments")
  end
  assert.call(direct_invocation.count('-PpeppyGoogleMessagesLink=true') == 1, 'trusted direct Android build must enable the Google Messages link exactly once without a sign-off variable')
  assert.call(play_invocation.count('-PpeppyGoogleMessagesLink=true') == 1, 'trusted Play Android build must enable the Google Messages link exactly once')
end

component_state_test = File.join(__dir__, 'test-release-component-state.sh')
_, stderr, status = Open3.capture3('bash', component_state_test)
assert.call(status.success?, "component-state fixtures must pass through the maintained helper: #{stderr}")

puts 'release workflow tests passed'
