#!/usr/bin/env ruby
# frozen_string_literal: true

require 'shellwords'
require 'yaml'

root = File.expand_path('..', __dir__)
load_workflow = ->(name) { YAML.load_file(File.join(root, 'workflows', name)) }
assert = ->(condition, message) { abort(message) unless condition }

ci = load_workflow.call('ci.yml')
ci_jobs = ci.fetch('jobs')
assert.call(ci_jobs.fetch('desktop').fetch('timeout-minutes') == 60, 'public CI desktop must allow 60 minutes uniformly for native cold-cache builds')
ci_text = File.read(File.join(root, 'workflows', 'ci.yml'))
assert.call(ci_jobs.key?('notify-hosted-production'), 'public CI must dispatch verified hosted production evidence')
assert.call(!ci_text.include?('create-github-app-token'), 'public CI must not mint an App token')
assert.call(!ci_text.match?(/HOSTED_DISPATCH_APP_|HOSTED_DISPATCH_TOKEN/), 'public CI must not depend on hosted dispatch credentials')
assert.call(ci_text.include?('PEPPY_PLATFORM_DISPATCH_TOKEN'), 'public CI must use the production-scoped dispatch token')
android_ci_steps = ci_jobs.fetch('android').fetch('steps')
android_packaging = android_ci_steps.find { |step| step['name'] == 'Lint and assemble Android artifacts' }
assert.call(!android_packaging.nil?, 'public CI must retain the Android lint and packaging step')
android_packaging_command = Shellwords.shellsplit(android_packaging.fetch('run'))
android_packaging_flags = android_packaging_command.reject { |token| token.start_with?(':') }
expected_android_packaging_flags = ['./gradlew', '--console=plain', '--stacktrace', '-Dorg.gradle.jvmargs=-Xmx4096m -Dfile.encoding=UTF-8', '--max-workers=2']
assert.call(android_packaging_flags == expected_android_packaging_flags, 'public CI Android packaging must use the 4 GiB JVM override, two-worker bound, and diagnostic flags')
android_packaging_tasks = android_packaging_command.select { |token| token.start_with?(':') }
assert.call(android_packaging_tasks == %w[:app:lintDebug :app:assembleDebug :app:assembleDebugAndroidTest], 'public CI Android packaging must retain lint, debug APK, and debug test APK tasks')
handoff = ci_jobs.fetch('notify-hosted-production')
assert.call(handoff.fetch('if') == "!cancelled() && github.event_name == 'push' && github.ref == 'refs/heads/production' && needs.gate.result == 'success' && needs.publish-production-server.result == 'success' && needs.publish-production-relay.result == 'success'", 'hosted dispatch must require a successful production push gate and both publishers')
assert.call(handoff.fetch('needs').sort == %w[gate publish-production-relay publish-production-server], 'hosted dispatch must wait for the gate and both publishers')
publish_job_names = %w[
  publish-prerelease
  publish-prerelease-android
  publish-staging-server
  publish-staging-relay
  publish-production-server
  publish-production-relay
  stable-release
]

condition_allows = lambda do |condition, event:, ref:, cancelled:, implicit_success:, results:, outputs: {}|
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
    expression = expression.strip
    expression = expression[1...-1].strip while expression.start_with?('(') && expression.end_with?(')')
    and_terms = split_terms.call(expression.dup, ' && ')
    next and_terms.all? { |term| evaluate.call(term) } if and_terms.length > 1

    or_terms = split_terms.call(expression.dup, ' || ')
    next or_terms.any? { |term| evaluate.call(term) } if or_terms.length > 1

    term = expression
    case term
    when '!cancelled()'
      !cancelled
    when "github.event_name == 'push'"
      event == 'push'
    when /\Agithub\.ref == '([^']+)'\z/
      ref == Regexp.last_match(1)
    when /\Aneeds\.([a-z0-9-]+)\.result == 'success'\z/
      results.fetch(Regexp.last_match(1)) == 'success'
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
  'publish-production-relay' => 'success'
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

staging_publishers = %w[publish-prerelease publish-staging-server publish-staging-relay]
staging_publishers << 'publish-prerelease-android'
production_publishers = %w[publish-production-server publish-production-relay stable-release]
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
%w[publish-prerelease publish-prerelease-android stable-release].each do |job_name|
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
%w[publish-production-server publish-production-relay].each do |publisher|
  %w[failure skipped cancelled].each do |publisher_result|
    results = successful_results.merge(publisher => publisher_result)
    assert.call(!condition_allows.call(publish_conditions.fetch('stable-release'), **production_context.merge(results: results)), "stable release must require #{publisher} success, not #{publisher_result}")
  end
end

stable = ci_jobs.fetch('stable-release')
assert.call(stable.fetch('needs').sort == %w[gate publish-production-relay publish-production-server version], 'stable release must wait for the gate and both edge publishers')
assert.call(stable.fetch('permissions') == { 'contents' => 'write', 'actions' => 'read' }, 'stable release needs only contents write and actions read')
assert.call(stable.fetch('concurrency').fetch('group') == 'stable-release', 'automatic stable release must serialize with manual release')
assert.call(stable.fetch('with') == { 'bump' => 'auto', 'version' => '', 'dry_run' => false }, 'automatic release must use the safe automatic inputs')

release = load_workflow.call('release.yml')
assert.call(release.fetch(true).key?('workflow_call'), 'release must be reusable')
assert.call(release.fetch('concurrency').fetch('cancel-in-progress') == false, 'release serialization must not cancel publication')
prepare = release.fetch('jobs').fetch('prepare')
assert.call(prepare.fetch('outputs').key?('skip'), 'duplicate and stale automatic releases must produce a no-op output')
prepare_steps = prepare.fetch('steps')
checkout_index = prepare_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
guard_index = prepare_steps.index { |step| step['run']&.include?('release-context-guard.sh') }
assert.call(!checkout_index.nil? && !guard_index.nil? && checkout_index < guard_index, 'release context helper must run after checkout')
assert.call(prepare_steps.fetch(guard_index).fetch('run').include?('git rev-parse HEAD'), 'release context helper must compare the checked-out HEAD to GitHub SHA')

build = load_workflow.call('build-artifacts.yml')
desktop_steps = build.fetch('jobs').fetch('desktop-development-artifacts').fetch('steps')
desktop_checkout_index = desktop_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
desktop_guard_index = desktop_steps.index { |step| step['run']&.include?('release-context-guard.sh') }
assert.call(!desktop_checkout_index.nil? && !desktop_guard_index.nil? && desktop_checkout_index < desktop_guard_index, 'artifact context helper must run after checkout')

publisher = load_workflow.call('publish-release.yml')
publisher_steps = publisher.fetch('jobs').fetch('publish').fetch('steps')
publisher_checkout_index = publisher_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
publisher_guard_index = publisher_steps.index { |step| step['run'] == 'bash .github/scripts/release-context-guard.sh' }
assert.call(!publisher_checkout_index.nil? && !publisher_guard_index.nil? && publisher_checkout_index < publisher_guard_index, 'publish context helper must run after checkout')
desktop_download = publisher_steps.find { |step| step.dig('with', 'pattern') == 'desktop-*-release-artifacts' }
android_download = publisher_steps.find { |step| step.dig('with', 'name') == 'android-release-artifacts' }
assert.call(!desktop_download.nil? && desktop_download.fetch('if').include?("inputs.component == 'desktop'"), 'desktop publisher must download only desktop release artifacts')
assert.call(!desktop_download.dig('with', 'pattern').include?('android-release-artifacts'), 'desktop download pattern must never match Android release artifacts')
assert.call(!android_download.nil? && android_download.fetch('if').include?("inputs.component == 'android'"), 'Android publisher must download exactly Android release artifacts')

trusted_contexts = 'workflow_dispatch:refs/heads/production:stable|push:refs/heads/production:stable|push:refs/heads/staging:staging'
helper = File.read(File.join(__dir__, 'release-context-guard.sh'))
assert.call(helper.include?(trusted_contexts), 'context helper must define the trusted context matrix')
assert.call(desktop_steps.fetch(desktop_guard_index - 2).fetch('run').include?(trusted_contexts), 'artifact pre-check must match the context helper matrix')
assert.call(publisher_steps.fetch(publisher_guard_index - 2).fetch('run').include?(trusted_contexts), 'publish pre-check must match the context helper matrix')

image_needs = release.fetch('jobs').fetch('publish-server-image').fetch('needs')
assert.call(!release.fetch('jobs').key?('container'), 'release must not rebuild container images after the CI gate')
assert.call(image_needs.sort == %w[artifacts prepare signing], 'stable server image promotion must wait for trusted artifacts')
relay_needs = release.fetch('jobs').fetch('publish-relay-image').fetch('needs')
assert.call(relay_needs.sort == %w[artifacts prepare signing], 'stable relay image promotion must wait for trusted artifacts')

signing = release.fetch('jobs').fetch('signing')
assert.call(signing.fetch('environment') == 'updates-stable', 'signing readiness must resolve protected stable environment inputs')
signing_steps = signing.fetch('steps')
signing_checkout_index = signing_steps.index { |step| step['uses']&.start_with?('actions/checkout@') }
signing_helper_index = signing_steps.index { |step| step['run']&.include?('release-signing-readiness.sh') }
assert.call(!signing_checkout_index.nil? && !signing_helper_index.nil? && signing_checkout_index < signing_helper_index, 'signing readiness helper must run after checkout')
signing_env = signing_steps.fetch(signing_helper_index).fetch('env')
assert.call(signing_env.fetch('PEPPY_MACOS_SIGNING_MODE').include?("vars.PEPPY_MACOS_SIGNING_MODE || 'developer-id'"), 'stable readiness must default an absent macOS mode to strict')
assert.call(signing_env.fetch('PEPPY_WINDOWS_SIGNING_MODE').include?("vars.PEPPY_WINDOWS_SIGNING_MODE || 'authenticode'"), 'stable readiness must default an absent Windows mode to strict')

release_artifacts = release.fetch('jobs').fetch('artifacts')
assert.call(release_artifacts.fetch('needs').sort == %w[prepare signing], 'trusted artifacts must wait for signing readiness')
assert.call(release_artifacts.fetch('if').include?("needs.signing.outputs.ready == 'true'"), 'trusted artifacts must require ready signing inputs')
assert.call(release_artifacts.fetch('if').include?('!cancelled()'), 'trusted artifacts must not begin after cancellation')
assert.call(release_artifacts.fetch('with').fetch('build_desktop') == true, 'stable release artifacts must always include desktop')

publish_needs = release.fetch('jobs').fetch('publish').fetch('needs')
assert.call(publish_needs.sort == %w[artifacts prepare publish-relay-image publish-server-image signing], 'stable publish must wait for trusted artifacts and both image publishers')
assert.call(release.fetch('jobs').fetch('publish').fetch('if').include?('!cancelled()'), 'release publishing must not begin after cancellation')
%w[publish-server-image publish-relay-image].each do |job_name|
  assert.call(release.fetch('jobs').fetch(job_name).fetch('if').include?('!cancelled()'), "#{job_name} must not begin after cancellation")
end

artifacts = load_workflow.call('build-artifacts.yml').fetch('jobs')
%w[android desktop ios].each do |component|
  job = artifacts.fetch("#{component}-development-artifacts")
  assert.call(job.fetch('if') == "inputs.build_#{component} != false", "#{component} artifact job must run unless explicitly disabled")
end
desktop_steps = artifacts.fetch('desktop-development-artifacts').fetch('steps')
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
  APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_ID APPLE_PASSWORD APPLE_SIGNING_IDENTITY
  APPLE_TEAM_ID PEPPY_ANDROID_KEYSTORE_BASE64 PEPPY_ANDROID_KEYSTORE_PASSWORD PEPPY_ANDROID_KEY_ALIAS PEPPY_ANDROID_KEY_PASSWORD
  PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET PEPPY_RELEASE_RPM_GPG_KEY PEPPY_UPDATER_PRIVATE_KEY PEPPY_UPDATER_PRIVATE_KEY_PASSWORD
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

assert.call(bundle_env.fetch('PEPPY_DESKTOP_GOOGLE_CLIENT_ID') == "${{ inputs.trusted_update_build && vars.PEPPY_DESKTOP_GOOGLE_CLIENT_ID || '' }}", 'desktop bundling must receive the public desktop Google client ID only for trusted builds')
assert.call(bundle_env.fetch('PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET') == "${{ inputs.trusted_update_build && secrets.PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET || '' }}", 'desktop bundling must receive the installed-client field only for trusted builds')

assert.call(build_workflow.fetch('jobs').fetch('desktop-development-artifacts').fetch('environment') == "${{ inputs.trusted_update_build && format('updates-{0}', inputs.update_channel) || 'development-artifacts' }}", 'artifact environment selection must stay unchanged')
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
assert.call(android_precheck.fetch('run').include?('[ "$BUILD_DESKTOP" != false ]'), 'trusted Android builds must reject requests without desktop artifacts')
android_sign = android_steps.find { |step| step['run'] == 'bash infra/release/sign-android-artifacts.sh dist/android-release/peppy-android-unsigned.apk dist/android-release/peppy-android-unsigned.aab dist/android-release' }
assert.call(!android_sign.nil?, 'trusted Android builds must sign release artifacts after building')
assert.call(!android_validation.nil? && !android_stop_gradle.nil?, 'trusted Android builds must validate inputs and stop Gradle before signing')
assert.call(android_stop_gradle.fetch('working-directory') == 'apps/android' && android_stop_gradle.fetch('run') == './gradlew --stop', 'trusted Android builds must stop Gradle in the Android project before signing')
assert.call(android_steps.index(android_precheck) < android_steps.index(android_checkout) && android_steps.index(android_checkout) < android_steps.index(android_guard) && android_steps.index(android_guard) < android_steps.index(android_validation) && android_steps.index(android_validation) < android_steps.index(android_stop_gradle) && android_steps.index(android_stop_gradle) < android_steps.index(android_sign), 'Android trust validation and Gradle shutdown must precede secret-scoped signing')
android_sign_env = android_sign.fetch('env')
%w[PEPPY_ANDROID_KEYSTORE_BASE64 PEPPY_ANDROID_KEYSTORE_PASSWORD PEPPY_ANDROID_KEY_ALIAS PEPPY_ANDROID_KEY_PASSWORD].each do |name|
  assert.call(android_sign_env.fetch(name) == "${{ inputs.trusted_update_build && secrets.#{name} || '' }}", "Android signer must receive #{name} only for trusted builds")
end
assert.call(android_sign_env.fetch('PEPPY_ANDROID_SIGNING_CERT_SHA256') == "${{ inputs.trusted_update_build && vars.PEPPY_ANDROID_SIGNING_CERT_SHA256 || '' }}", 'Android signer must receive the protected certificate fingerprint only for trusted builds')
assert.call(android_steps.any? { |step| step.dig('with', 'path') == 'dist/android-release/peppy-android-debug.apk' }, 'development Android artifact must contain only the debug APK')
assert.call(android_steps.any? { |step| step.dig('with', 'path') == "dist/android-release/peppy-android.apk\ndist/android-release/peppy-android.aab\n" }, 'release Android artifact must contain only signed assets')
android_development_build = android_steps.find { |step| step['name'] == 'Build development Android debug APK' }
assert.call(android_development_build.fetch('env').fetch('PEPPY_ANDROID_NATIVE_PROFILE') == 'release', 'development Android builds must retain release native compilation')
assert.call(android_development_build.fetch('run').include?(':app:assembleDebug :app:assembleRelease :app:bundleRelease'), 'development Android builds must retain release variant compilation')

stage_script = File.read(File.join(root, '..', 'infra', 'release', 'stage-assets.sh'))
assert.call(stage_script.include?("peppy-android.apk") && stage_script.include?("peppy-android.aab"), 'staging must require signed Android artifact names')
assert.call(!stage_script.include?('peppy-android-unsigned'), 'staging must reject old unsigned Android artifact names')
verify_step = publisher_steps.find { |step| step['run'] == 'bash infra/release/verify-android-artifacts.sh "dist/release/peppy-$VERSION-android.apk" "dist/release/peppy-$VERSION-android.aab"' }
assert.call(!verify_step.nil?, 'publisher must independently verify staged Android signatures')
assert.call(verify_step.fetch('env').fetch('PEPPY_ANDROID_SIGNING_CERT_SHA256') == "${{ inputs.trusted_update_build && vars.PEPPY_ANDROID_SIGNING_CERT_SHA256 || '' }}", 'publisher must use the protected certificate fingerprint')
assert.call(verify_step.fetch('env').fetch('VERSION') == '${{ inputs.version }}', 'publisher Android verification must receive the release version')
publisher_java = publisher_steps.find { |step| step['uses'] == 'actions/setup-java@cf277c60eb25467037889841efdb72551f06f6c3' }
assert.call(!publisher_java.nil?, 'publisher must select JDK 17 before Android signature verification')
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
assert.call(!publisher_stage.nil? && !publisher_desktop_stage.nil? && !publisher_updater_sign.nil? && !publisher_checksums.nil? && !publisher_android_create.nil? && !publisher_desktop_create.nil?, 'publisher must retain component staging, desktop updater signing, checksums, and release creation')
assert.call(publisher_stage.fetch('if').include?("inputs.component == 'android'") && publisher_desktop_stage.fetch('if').include?("inputs.component == 'desktop'"), 'publisher must stage the selected component only')
assert.call(publisher_steps.index(publisher_context_guard) < publisher_steps.index(publisher_stage) && publisher_steps.index(publisher_stage) < publisher_steps.index(verify_step) && publisher_steps.index(verify_step) < publisher_steps.index(publisher_checksums) && publisher_steps.index(verify_step) < publisher_steps.index(publisher_android_create), 'publisher must verify Android assets after Android staging and before checksums or Android release creation')
assert.call(publisher_steps.index(publisher_desktop_stage) < publisher_steps.index(publisher_updater_sign) && publisher_steps.index(publisher_updater_sign) < publisher_steps.index(publisher_desktop_create), 'desktop publisher must sign update assets before desktop release creation')
assert.call(publisher_android_create.fetch('run').include?('--latest=false') && publisher_android_edit.fetch('run').scan('--latest=false').length == 2, 'Android release creation and edits must never mark Android as latest')
assert.call(publisher_android_create.fetch('run').include?('"${TAG_PREFIX}$VERSION"') && publisher_android_edit.fetch('run').include?('"${TAG_PREFIX}$VERSION"'), 'Android release commands must use the android-v tag prefix')
assert.call(publisher.fetch('jobs').fetch('publish').dig('env', 'TAG_PREFIX') == "${{ inputs.component == 'android' && 'android-v' || 'v' }}", 'Android release commands must resolve to android-v tags')
assert.call(publisher_desktop_create.fetch('run').include?('"v$VERSION"') && publisher_desktop_edit.fetch('run').include?('--latest'), 'desktop release create/edit commands must retain the desktop tag and stable latest flag')
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
release_publish_android_allows = lambda do |context, dry_run:, release_results:, release_outputs:|
  next false unless condition_allows.call(publish_conditions.fetch('stable-release'), **context)
  next false if dry_run || release_outputs.fetch('skip') || release_outputs.fetch('android_skip')

  release_results.fetch('publish') == 'success'
end
successful_release_outputs = { 'skip' => false, 'android_skip' => false }.freeze
successful_release_results = { 'publish' => 'success' }.freeze
assert.call(release_publish_android_allows.call(production_context.merge(implicit_success: true), dry_run: false, release_results: successful_release_results, release_outputs: successful_release_outputs), 'publish-android must run for a successful stable release')
assert.call(!release_publish_android_allows.call(pull_request_context, dry_run: false, release_results: successful_release_results, release_outputs: successful_release_outputs), 'publish-android must not run for a pull request')
assert.call(!release_publish_android_allows.call(staging_context.merge(implicit_success: true), dry_run: false, release_results: successful_release_results, release_outputs: successful_release_outputs), 'publish-android must not run on staging')
%w[failure skipped cancelled].each do |gate_result|
  assert.call(!release_publish_android_allows.call(production_context.merge(implicit_success: true, results: successful_results.merge('gate' => gate_result)), dry_run: false, release_results: successful_release_results, release_outputs: successful_release_outputs), "publish-android must require a successful gate, not #{gate_result}")
end

workflow_text = Dir[File.join(root, 'workflows', '*.{yml,yaml}')].sort.map { |path| File.read(path) }.join("\n")
assert.call(!workflow_text.match?(/PEPPY_ANDROID_SKIP|skip_unchanged|SKIP_UNCHANGED/i), 'workflows must not expose an Android skip override')
workflow_documents = Dir[File.join(root, 'workflows', '*.{yml,yaml}')].map { |path| YAML.load_file(path) }
workflow_documents.each do |workflow|
  dispatch_inputs = workflow.dig(true, 'workflow_dispatch', 'inputs') || {}
  assert.call(dispatch_inputs.keys.none? { |name| name.match?(/(?:force|skip).*android|android.*(?:force|skip)/i) }, 'workflows must not expose a force/skip Android dispatch input')
end

release_tooling = ci_jobs.fetch('release-tooling')
assert.call(release_tooling.fetch('timeout-minutes') == 20, 'release tooling needs Android fixture installation headroom')
release_tooling_steps = release_tooling.fetch('steps')
release_tooling_java = release_tooling_steps.find { |step| step['uses'] == 'actions/setup-java@cf277c60eb25467037889841efdb72551f06f6c3' }
release_tooling_sdk = release_tooling_steps.find { |step| step['name'] == 'Install Android signing test SDK tools' }
assert.call(!release_tooling_java.nil? && release_tooling_java.fetch('with') == { 'distribution' => 'temurin', 'java-version' => '17' }, 'release tooling must install Temurin JDK 17')
assert.call(!release_tooling_sdk.nil? && release_tooling_sdk.fetch('run').include?('platforms;android-35') && release_tooling_sdk.fetch('run').include?('build-tools;35.0.0'), 'release tooling must install the Android signing test SDK platform and build tools')
assert.call(release_tooling_steps.index(release_tooling_java) < release_tooling_steps.index(release_tooling_sdk) && release_tooling_steps.index(release_tooling_sdk) < release_tooling_steps.index(release_tooling_steps.find { |step| step['run'] == 'infra/release/test-android-signing.sh' }), 'release tooling must set up JDK and SDK before real Android signing tests')

puts 'release workflow tests passed'
