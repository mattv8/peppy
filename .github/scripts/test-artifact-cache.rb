#!/usr/bin/env ruby
# frozen_string_literal: true

require 'yaml'

root = File.expand_path('..', __dir__)
workflow = YAML.load_file(File.join(root, 'workflows', 'build-artifacts.yml'))
jobs = workflow.fetch('jobs')

assert = ->(condition, message) { abort(message) unless condition }
production_save_guard = "github.event_name == 'push' && github.ref == 'refs/heads/production'"

cache_steps = lambda do |job|
  job.fetch('steps').select { |step| step['uses']&.start_with?('actions/cache/') }
end

assert_cache_scope = lambda do |job_name, expected_scopes|
  steps = cache_steps.call(jobs.fetch(job_name))
  restores = steps.select { |step| step['uses'].start_with?('actions/cache/restore@') }
  saves = steps.select { |step| step['uses'].start_with?('actions/cache/save@') }

  assert.call(restores.map { |step| step.dig('with', 'key') }.sort == expected_scopes.sort, "#{job_name} must restore each cache scope")
  assert.call(saves.map { |step| step.dig('with', 'key') }.sort == expected_scopes.sort, "#{job_name} must nominate one save per cache scope")
  saves.each do |step|
    assert.call(step['if'] == "${{ success() && #{production_save_guard} && steps.#{step.fetch('id').sub('-save', '')}.outputs.cache-hit != 'true' }}", "#{job_name} cache save must be production-push-only and skip cache hits")
  end
end

android_scopes = [
  "rust-android-artifacts-${{ runner.os }}-${{ runner.arch }}-1.98.1-host-debug-cross-release-mobile-bindings-default-ndk-27.2.12479018-${{ hashFiles('Cargo.lock', 'Cargo.toml', 'crates/mobile-bindings/Cargo.toml') }}",
  "gradle-android-artifacts-${{ runner.os }}-${{ hashFiles('apps/android/**/*.gradle.kts', 'apps/android/gradle.properties', 'apps/android/gradle/wrapper/gradle-wrapper.properties', 'apps/android/gradle/verification-metadata.xml') }}"
].freeze
desktop_scope = "rust-desktop-artifacts-${{ matrix.name }}-${{ runner.arch }}-1.98.1-release-default-self-update-${{ hashFiles('apps/desktop/src-tauri/Cargo.lock', 'apps/desktop/src-tauri/Cargo.toml') }}"
ios_scope = "rust-ios-artifacts-${{ runner.os }}-${{ runner.arch }}-1.98.1-release-mobile-bindings-default-aarch64-apple-ios-aarch64-apple-ios-sim-${{ hashFiles('Cargo.lock', 'Cargo.toml', 'crates/mobile-bindings/Cargo.toml') }}"

assert_cache_scope.call('android-development-artifacts', android_scopes)
assert_cache_scope.call('desktop-development-artifacts', [desktop_scope])
assert_cache_scope.call('ios-development-artifacts', [ios_scope])

android_steps = cache_steps.call(jobs.fetch('android-development-artifacts'))
android_rust = android_steps.find { |step| step.dig('with', 'key') == android_scopes.first }
android_gradle = android_steps.find { |step| step.dig('with', 'key') == android_scopes.last }
assert.call(android_rust.dig('with', 'path').include?('target/aarch64-linux-android/release/deps'), 'Android Rust cache must retain Android release compiled dependencies')
assert.call(android_rust.dig('with', 'path').include?('target/x86_64-linux-android/release/deps'), 'Android Rust cache must retain both Android ABI dependency outputs')
assert.call(android_rust.dig('with', 'path').include?('target/debug/deps'), 'Android Rust cache must retain the host-debug bindgen dependency outputs')
assert.call(!android_rust.dig('with', 'path').include?('apps/android/app/src/main/jniLibs'), 'Android Rust cache must not contain generated JNI release inputs')
assert.call(android_gradle.dig('with', 'path') == "~/.gradle/caches\n~/.gradle/wrapper\n", 'Android Gradle cache must contain only Gradle user-home caches')

android_workflow_steps = jobs.fetch('android-development-artifacts').fetch('steps')
trusted_build = android_workflow_steps.index { |step| step['name'] == 'Build trusted Android release artifacts' }
development_build = android_workflow_steps.index { |step| step['name'] == 'Build development Android debug APK' }
cache_stop = android_workflow_steps.index { |step| step['name'] == 'Stop Android Gradle daemon before caching' }
rust_save = android_workflow_steps.index { |step| step['id'] == 'android-rust-cache-save' }
gradle_save = android_workflow_steps.index { |step| step['id'] == 'android-gradle-cache-save' }
first_upload = android_workflow_steps.index { |step| step['uses']&.start_with?('actions/upload-artifact@') }
assert.call(trusted_build < cache_stop && development_build < cache_stop, 'Android cache shutdown must follow trusted and development build paths')
assert.call(cache_stop < rust_save && rust_save < gradle_save && gradle_save < first_upload, 'Android cache saves must follow shutdown and precede uploads')

desktop_steps = cache_steps.call(jobs.fetch('desktop-development-artifacts'))
desktop_cache = desktop_steps.find { |step| step.dig('with', 'key') == desktop_scope }
assert.call(desktop_cache.dig('with', 'path').include?('apps/desktop/src-tauri/target/release/deps'), 'desktop cache must use the standalone release dependency directory')
assert.call(!desktop_cache.dig('with', 'path').include?('/bundle'), 'desktop cache must not contain final bundles')

ios_steps = cache_steps.call(jobs.fetch('ios-development-artifacts'))
ios_cache = ios_steps.find { |step| step.dig('with', 'key') == ios_scope }
assert.call(ios_cache.dig('with', 'path').include?('target/aarch64-apple-ios/release/deps'), 'iOS cache must preserve device cross-target dependencies')
assert.call(ios_cache.dig('with', 'path').include?('target/aarch64-apple-ios-sim/release/deps'), 'iOS cache must preserve simulator cross-target dependencies')
assert.call(!ios_cache.dig('with', 'path').include?('dist/ios'), 'iOS cache must not contain final artifacts')

puts 'Artifact cache workflow tests passed'
