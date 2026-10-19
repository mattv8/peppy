#!/usr/bin/env ruby
# frozen_string_literal: true

require 'yaml'

root = File.expand_path('..', __dir__)
load_workflow = ->(name) { YAML.load_file(File.join(root, 'workflows', name)) }
assert = ->(condition, message) { abort(message) unless condition }

ci = load_workflow.call('ci.yml')
stable = ci.fetch('jobs').fetch('stable-release')
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

trusted_contexts = 'workflow_dispatch:refs/heads/production:stable|push:refs/heads/production:stable|push:refs/heads/staging:staging'
helper = File.read(File.join(__dir__, 'release-context-guard.sh'))
assert.call(helper.include?(trusted_contexts), 'context helper must define the trusted context matrix')
assert.call(desktop_steps.fetch(desktop_guard_index - 2).fetch('run').include?(trusted_contexts), 'artifact pre-check must match the context helper matrix')
assert.call(publisher_steps.fetch(publisher_guard_index - 2).fetch('run').include?(trusted_contexts), 'publish pre-check must match the context helper matrix')

image_needs = release.fetch('jobs').fetch('publish-server-image').fetch('needs')
assert.call(image_needs.sort == %w[artifacts container prepare], 'stable server image promotion must wait for trusted artifacts')
relay_needs = release.fetch('jobs').fetch('publish-relay-image').fetch('needs')
assert.call(relay_needs.sort == %w[artifacts container prepare], 'stable relay image promotion must wait for trusted artifacts')

artifacts = load_workflow.call('build-artifacts.yml').fetch('jobs')
android = artifacts.fetch('android-development-artifacts').fetch('steps').find { |step| step.dig('with', 'name')&.start_with?('android-') }
ios = artifacts.fetch('ios-development-artifacts').fetch('steps').find { |step| step.dig('with', 'name')&.start_with?('ios-') }
assert.call(android.dig('with', 'name').include?("inputs.trusted_update_build && 'release' || 'development'"), 'Android artifacts must separate release and development namespaces')
assert.call(ios.dig('with', 'name').include?("inputs.trusted_update_build && 'release' || 'development'"), 'iOS artifacts must separate release and development namespaces')

puts 'release workflow tests passed'
