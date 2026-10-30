#!/usr/bin/env ruby
# frozen_string_literal: true

require 'json'
require 'minitest/autorun'

class ContributionPolicyTest < Minitest::Test
  ROOT = File.expand_path('..', __dir__)
  RULESETS = File.join(ROOT, 'rulesets')
  PROTECTED_REFS = %w[refs/heads/production refs/heads/staging].freeze

  def payload(name)
    JSON.parse(File.read(File.join(RULESETS, name)))
  end

  def rule(payload, type)
    payload.fetch('rules').find { |entry| entry['type'] == type }
  end

  def assert_branch_scope(payload, refs = PROTECTED_REFS)
    assert_equal %w[bypass_actors conditions enforcement name rules target], payload.keys.sort
    assert_equal 'branch', payload['target']
    assert_equal 'active', payload['enforcement']
    assert_equal ['ref_name'], payload.fetch('conditions').keys
    assert_equal refs, payload.dig('conditions', 'ref_name', 'include')
    assert_equal [], payload.dig('conditions', 'ref_name', 'exclude')
  end

  def test_single_codeowners_file_assigns_every_path_to_the_owner
    assert_equal "* @mattv8\n", File.read(File.join(ROOT, 'CODEOWNERS'))
    refute File.exist?(File.join(File.dirname(ROOT), 'CODEOWNERS'))
    refute File.exist?(File.join(File.dirname(ROOT), 'docs', 'CODEOWNERS'))
  end

  def test_contribution_review_requires_a_current_code_owner_approval
    review = payload('contribution-review.json')
    assert_branch_scope(review)
    assert_equal ['pull_request'], review.fetch('rules').map { |entry| entry['type'] }
    parameters = rule(review, 'pull_request').fetch('parameters')

    assert_equal %w[allowed_merge_methods dismiss_stale_reviews_on_push require_code_owner_review require_extra_approval_for_unattributed_changes require_last_push_approval required_approving_review_count required_review_thread_resolution required_reviewers], parameters.keys.sort
    assert_equal [], parameters['required_reviewers']
    assert_equal %w[merge rebase], parameters['allowed_merge_methods']
    assert_equal 1, parameters['required_approving_review_count']
    assert_equal false, parameters['required_review_thread_resolution']
    assert_equal true, parameters['require_code_owner_review']
    assert_equal true, parameters['require_extra_approval_for_unattributed_changes']
    assert_equal true, parameters['dismiss_stale_reviews_on_push']
    assert_equal true, parameters['require_last_push_approval']
  end

  def test_contribution_review_exempts_only_the_named_owner_user
    review = payload('contribution-review.json')
    assert_equal [{ 'actor_id' => 9_312_603, 'actor_type' => 'User', 'bypass_mode' => 'exempt' }], review['bypass_actors']
  end

  def test_pull_request_baseline_applies_even_to_the_review_exempt_owner
    baseline = payload('pull-request-required.json')
    assert_branch_scope(baseline)
    assert_equal ['pull_request'], baseline.fetch('rules').map { |entry| entry['type'] }
    assert_equal [], baseline['bypass_actors']
    assert_equal({
      'allowed_merge_methods' => %w[merge rebase],
      'dismiss_stale_reviews_on_push' => false,
      'required_reviewers' => [],
      'require_code_owner_review' => false,
      'require_extra_approval_for_unattributed_changes' => false,
      'require_last_push_approval' => false,
      'required_approving_review_count' => 0,
      'required_review_thread_resolution' => false
    }, rule(baseline, 'pull_request').fetch('parameters'))
  end

  def test_required_ci_uses_the_exact_gate_check_in_strict_mode_without_bypass
    ci = payload('required-ci.json')
    assert_branch_scope(ci)
    assert_equal ['required_status_checks'], ci.fetch('rules').map { |entry| entry['type'] }
    parameters = rule(ci, 'required_status_checks').fetch('parameters')

    assert_equal %w[required_status_checks strict_required_status_checks_policy], parameters.keys.sort
    assert_equal true, parameters['strict_required_status_checks_policy']
    assert_equal [{ 'context' => 'gate', 'integration_id' => 15_368 }], parameters['required_status_checks']
    assert_equal [], ci['bypass_actors']
  end

  def test_production_integrity_forbids_force_pushes_and_requires_linear_history
    integrity = payload('production-integrity.json')
    assert_branch_scope(integrity, ['refs/heads/production'])

    assert_equal 'Default', integrity['name']
    assert_equal %w[non_fast_forward required_linear_history], integrity.fetch('rules').map { |entry| entry['type'] }
    assert_equal [], integrity['bypass_actors']
  end

  def test_ci_script_runs_the_contribution_policy_contract
    ci_script = File.read(File.join(ROOT, 'scripts', 'test-ci-scripts.sh'))
    assert_includes ci_script, 'ruby "$SCRIPT_DIR/test-contribution-policy.rb"'
  end
end
