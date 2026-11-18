#!/usr/bin/env ruby
# frozen_string_literal: true

require 'json'
require 'minitest/autorun'
require 'open3'
require 'tmpdir'
require 'yaml'

class MergeSerializerTest < Minitest::Test
  WORKFLOW = File.expand_path('../workflows/merge-serializer.yml', __dir__)
  FAKE_GH = <<~'RUBY'
    #!/usr/bin/env ruby
    require 'json'
    fixture = JSON.parse(File.read(ENV.fetch('FIXTURE')))
    args = ARGV
    File.open(ENV.fetch('CALLS'), 'a') { |f| f.puts JSON.generate(args) }
    endpoint = args.find { |arg| arg.start_with?('/') }.to_s
    route = endpoint.split('?').first.to_s
    state_path = ENV.fetch('STATE')
    state = File.exist?(state_path) ? JSON.parse(File.read(state_path)) : {}
    next_value = lambda do |key, values|
      index = state.fetch(key, 0)
      state[key] = index + 1
      File.write(state_path, JSON.generate(state))
      values.empty? ? {} : values[[index, values.length - 1].min]
    end
    if args[0, 2] == %w[api graphql]
      key = 'graphql'
      number = args.find { |arg| arg.start_with?('id=NODE') }.to_s.delete_prefix('id=NODE').to_i
      value = fixture.fetch('mutation', { 'data' => { 'updatePullRequestBranch' => { 'pullRequest' => { 'number' => number } } } })
    elsif route.end_with?('/pulls')
      key, value = 'list', fixture.fetch('pages', [[]])
    elsif route.include?('/pulls/')
      key = 'detail'
      number = route.split('/').last
      value = next_value.call(number, fixture.fetch('details', {}).fetch(number, []))
    elsif route.include?('/branches/')
      key, value = 'branch', fixture.fetch('branch', { 'commit' => { 'sha' => 'base' } })
    elsif route.include?('/compare/')
      key = 'compare'
      sha = route.split('...').last
      value = fixture.fetch('compare', {}).fetch(sha, { 'behind_by' => 1 })
      value = next_value.call("compare:#{sha}", value) if value.is_a?(Array)
    elsif route.include?('/check-runs')
      key = 'checks'
      sha = route.split('/commits/').last.split('/').first
      value = fixture.fetch('checks', {}).fetch(sha, [{ 'check_runs' => [] }])
    else
      warn "unexpected gh call: #{args.inspect}"
      exit 2
    end
    exit 1 if fixture.fetch('fail', []).include?(key)
    puts JSON.generate(value)
  RUBY

  def workflow
    YAML.load_file(WORKFLOW)
  end

  def pr(number, state = 'behind')
    { 'number' => number, 'node_id' => "NODE#{number}", 'auto_merge' => {}, 'draft' => false,
      'created_at' => format('2024-01-%02dT00:00:00Z', number), 'mergeable_state' => state,
      'base' => { 'ref' => 'production' },
      'head' => { 'sha' => "sha#{number}", 'ref' => "feature-#{number}", 'repo' => { 'full_name' => 'org/repo' } } }
  end

  def fixture(*prs)
    { 'pages' => [prs], 'details' => prs.to_h { |item| [item.fetch('number').to_s, [item]] } }
  end

  def check(id: 1, name: 'gate', status: 'completed', conclusion: 'success')
    { 'id' => id, 'name' => name, 'status' => status, 'conclusion' => conclusion }
  end

  def run_workflow(data, token: true, required: 'gate')
    script = workflow.fetch('jobs').fetch('advance-queue').fetch('steps').first.fetch('run')
    Dir.mktmpdir('merge-serializer') do |dir|
      File.write("#{dir}/gh", FAKE_GH)
      File.write("#{dir}/sleep", "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SLEEPS\"\n")
      %w[gh sleep].each { |name| File.chmod(0o755, "#{dir}/#{name}") }
      File.write("#{dir}/fixture", JSON.generate(data))
      env = { 'PATH' => "#{dir}:#{ENV.fetch('PATH')}", 'FIXTURE' => "#{dir}/fixture",
              'CALLS' => "#{dir}/calls", 'STATE' => "#{dir}/state", 'SLEEPS' => "#{dir}/sleeps",
              'REPO' => 'org/repo', 'BASE_BRANCH' => 'production', 'REQUIRED_CHECKS' => required,
              'GH_TOKEN' => token ? 'fake-token' : nil }
      out, err, status = Open3.capture3(env, 'bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', script)
      calls = File.exist?("#{dir}/calls") ? File.readlines("#{dir}/calls").map { |line| JSON.parse(line) } : []
      sleeps = File.exist?("#{dir}/sleeps") ? File.readlines("#{dir}/sleeps", chomp: true) : []
      [out, err, status, calls, sleeps]
    end
  end

  def mutations(calls)
    calls.select { |call| call[0, 2] == %w[api graphql] }
  end

  def assert_success(result)
    assert result[2].success?, "stdout: #{result[0]}\nstderr: #{result[1]}"
  end

  def assert_candidate(result, number)
    assert_success(result)
    calls = mutations(result[3])
    assert_equal 1, calls.length
    assert_includes calls.first.join(' '), 'updateMethod: REBASE'
    assert_includes calls.first, "id=NODE#{number}"
    assert_includes calls.first, "expectedHeadOid=sha#{number}"
  end

  def test_workflow_contract
    text = File.read(WORKFLOW)
    doc = workflow
    events = doc.fetch(true)
    assert_equal %w[pull_request_target push workflow_dispatch workflow_run], events.keys.sort
    assert_equal({ 'branches' => ['production'] }, events.fetch('push'))
    assert_equal({ 'workflows' => ['CI'], 'types' => ['completed'] }, events.fetch('workflow_run'))
    target = events.fetch('pull_request_target')
    assert_equal ['production'], target.fetch('branches')
    assert_equal %w[auto_merge_disabled auto_merge_enabled closed converted_to_draft edited ready_for_review reopened], target.fetch('types').sort
    assert_equal({}, doc.fetch('permissions'))
    assert_equal({ 'group' => 'merge-serializer-production', 'cancel-in-progress' => false }, doc.fetch('concurrency'))
    assert_equal 'production', doc.fetch('env').fetch('BASE_BRANCH')
    assert_equal 'gate', doc.fetch('env').fetch('REQUIRED_CHECKS')
    job = doc.fetch('jobs').fetch('advance-queue')
    assert_equal 'ubuntu-latest', job.fetch('runs-on')
    assert_equal 5, job.fetch('timeout-minutes')
    assert_equal '${{ secrets.PEPPY_AUTOMATION_TOKEN }}', job.fetch('steps').first.fetch('env').fetch('GH_TOKEN')
    refute_includes text, 'github.ref'
    assert_includes text, 'zizmor: ignore[dangerous-triggers]'
    %w[checkout download-artifact actions/cache upload-artifact].each { |item| refute_includes text.downcase, item }
    ['npm install', 'pip install'].each { |item| refute_includes text.downcase, item }
  end

  def test_missing_token_and_empty_queue
    result = run_workflow({}, token: false)
    refute result[2].success?
    assert_includes result[1], 'PEPPY_AUTOMATION_TOKEN'
    assert_empty result[3]
    result = run_workflow({})
    assert_success(result)
    assert_empty mutations(result[3])
    assert_equal ['api', '--paginate', '--slurp', '/repos/org/repo/pulls?base=production&state=open&sort=created&direction=asc&per_page=100'], result[3].first
  end

  def test_filters_pagination_and_oldest_tiebreak
    ignored = (1..9).map { |n| pr(n) }
    ignored[0]['auto_merge'] = nil
    ignored[1]['draft'] = true
    ignored[2]['head']['repo']['full_name'] = 'fork/repo'
    ignored[3]['head']['repo'] = nil
    %w[production staging main beta].each_with_index { |ref, i| ignored[i + 4]['head']['ref'] = ref }
    ignored[8]['base']['ref'] = 'other'
    first, second = pr(10), pr(11)
    second['created_at'] = first['created_at']
    data = fixture(first, second)
    data['pages'] = [ignored + [second], [first]]
    result = run_workflow(data)
    assert_candidate(result, 10)
    assert_empty result[4]
    assert_equal 1, result[3].count { |call| call.any? { |arg| arg.include?('/pulls?') } }
  end

  def test_required_check_hold_and_skip_decisions
    cases = [
      ['missing', [], 'clean', true, 'checks in flight'],
      ['in progress', [check(status: 'in_progress', conclusion: nil)], 'clean', true, 'checks in flight'],
      ['green', [check], 'clean', true, 'ready for auto-merge'],
      ['failed', [check(conclusion: 'failure')], 'clean', false, 'required check failed'],
      ['blocked', [check], 'blocked', false, 'skipping'],
      ['unrelated running', [check(conclusion: 'failure'), check(id: 2, name: 'other', status: 'in_progress', conclusion: nil)], 'clean', false, 'required check failed']
    ]
    cases.each do |label, checks, state, hold, message|
      older, current = pr(1), pr(2, state)
      data = fixture(older, current)
      data['compare'] = { 'sha2' => { 'behind_by' => 0 } }
      data['checks'] = { 'sha2' => [{ 'check_runs' => checks }] }
      result = run_workflow(data)
      assert_success(result)
      assert_includes result[0], message, label
      if hold
        assert_empty mutations(result[3]), label
      else
        assert_candidate(result, 1)
      end
    end
  end

  def test_latest_required_check_wins_across_pages
    [true, false].each do |latest_success|
      data = fixture(pr(1), pr(2, 'clean'))
      data['compare'] = { 'sha2' => { 'behind_by' => 0 } }
      data['checks'] = { 'sha2' => [
        { 'check_runs' => [check(id: 1, conclusion: latest_success ? 'failure' : 'success')] },
        { 'check_runs' => [check(id: 9, conclusion: latest_success ? 'success' : 'failure')] }
      ] }
      result = run_workflow(data)
      assert_success(result)
      latest_success ? assert_empty(mutations(result[3])) : assert_candidate(result, 1)
    end
  end

  def test_newline_separated_required_checks_and_successful_conclusions
    data = nil
    %w[success neutral skipped].each do |conclusion|
      data = fixture(pr(1), pr(2, 'clean'))
      data['compare'] = { 'sha2' => { 'behind_by' => 0 } }
      data['checks'] = { 'sha2' => [{ 'check_runs' => [check(conclusion: conclusion), check(id: 2, name: 'second')] }] }
      result = run_workflow(data, required: "gate\nsecond")
      assert_success(result)
      assert_empty mutations(result[3])
      assert_includes result[0], 'ready for auto-merge'
    end
    data['checks']['sha2'][0]['check_runs'].pop
    result = run_workflow(data, required: "gate\nsecond")
    assert_success(result)
    assert_includes result[0], 'checks in flight'
    assert_empty mutations(result[3])
  end

  def test_dirty_behind_skips_to_next
    result = run_workflow(fixture(pr(1, 'dirty'), pr(2)))
    assert_includes result[0], 'merge conflicts — skipping'
    assert_candidate(result, 2)
  end

  def test_api_failures_and_malformed_data_fail_closed
    %w[list branch compare detail checks].each do |boundary|
      data = fixture(pr(1))
      data['compare'] = { 'sha1' => { 'behind_by' => 0 } } if boundary == 'checks'
      data['fail'] = [boundary]
      result = run_workflow(data)
      refute result[2].success?, boundary
      assert_empty mutations(result[3]), boundary
    end
    [
      ['list', { 'pages' => [[pr(1)], {}] }],
      ['branch', { 'branch' => {} }],
      ['compare', { 'compare' => { 'sha1' => {} } }],
      ['detail', { 'details' => { '1' => [{}] } }],
      ['checks page', { 'compare' => { 'sha1' => { 'behind_by' => 0 } }, 'checks' => { 'sha1' => [{ 'check_runs' => [] }, {}] } }],
      ['check run', { 'compare' => { 'sha1' => { 'behind_by' => 0 } }, 'checks' => { 'sha1' => [{ 'check_runs' => [{}] }] } }]
    ].each do |label, overrides|
      result = run_workflow(fixture(pr(1)).merge(overrides))
      refute result[2].success?, label
      assert_empty mutations(result[3]), label
    end
  end

  def test_invalid_pr_number_and_empty_required_checks_fail_closed
    invalid = pr(1)
    invalid['number'] = 1.5
    result = run_workflow(fixture(invalid))
    refute result[2].success?, 'fractional PR numbers must be rejected'
    assert_empty mutations(result[3])

    data = fixture(pr(1, 'clean'))
    data['compare'] = { 'sha1' => { 'behind_by' => 0 } }
    result = run_workflow(data, required: '')
    refute result[2].success?, 'empty required checks must fail closed'
    assert_empty mutations(result[3])
  end

  def test_candidate_revalidation
    %w[head eligibility ancestry].each do |change|
      original = pr(1)
      changed = pr(1)
      changed['head']['sha'] = 'newsha' if change == 'head'
      changed['auto_merge'] = nil if change == 'eligibility'
      data = fixture(original, pr(2))
      data['details']['1'] = [original, changed]
      data['compare'] = { 'sha1' => [{ 'behind_by' => 1 }, { 'behind_by' => 0 }] } if change == 'ancestry'
      result = run_workflow(data)
      assert_success(result)
      assert_empty mutations(result[3]), change
    end
  end

  def test_graphql_errors_or_http_failure_never_try_another_candidate
    [{ 'mutation' => { 'errors' => [{ 'message' => 'failure' }] } }, { 'fail' => ['graphql'] }].each do |override|
      result = run_workflow(fixture(pr(1), pr(2)).merge(override))
      refute result[2].success?
      assert_equal 1, mutations(result[3]).length
    end
  end

  def test_unknown_retries_are_bounded_and_success_does_not_sleep
    unknown = pr(1, 'unknown')
    data = fixture(unknown)
    data['compare'] = { 'sha1' => { 'behind_by' => 0 } }
    result = run_workflow(data)
    assert_success(result)
    assert_equal %w[2 2], result[4]
    assert_empty mutations(result[3])
    assert_equal 3, result[3].count { |call| call.include?('/repos/org/repo/pulls/1') }
    assert_includes result[1], 'retrying'
    refute_includes result[0], 'retrying'
    result = run_workflow(fixture(pr(1)))
    assert_candidate(result, 1)
    assert_empty result[4]
  end
end
