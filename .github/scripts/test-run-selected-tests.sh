#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT
cat > "$temp_dir/cargo" <<'EOF'
#!/usr/bin/env bash
if [[ ${!#} == --list ]]; then
  printf '%s\n' "$*" > "$LIST_LOG"
  [[ ${FAIL_LIST:-0} == 0 ]] || exit 23
  printf '%s\n' 'module::present: test' 'updates::present: test'
else
  printf '%s\n' "$*" > "$RUN_LOG"
  exit "${FAIL_RUN:-0}"
fi
EOF
cat > "$temp_dir/go" <<'EOF'
#!/usr/bin/env bash
if [[ ${*: -2:1} == -list ]]; then
  pattern=${!#}
  for name in TestPresent TestOther; do
    if [[ $name =~ $pattern ]]; then echo "$name"; fi
  done
else
  printf '%s\n' "$*" > "$RUN_LOG"
fi
EOF
chmod +x "$temp_dir/cargo" "$temp_dir/go"
export PATH="$temp_dir:$PATH" RUN_LOG="$temp_dir/run" LIST_LOG="$temp_dir/list"
helper="$script_dir/run-selected-tests.sh"

bash "$helper" exact module::present -- cargo test module::present -- --exact --ignored
grep -Fxq 'test module::present -- --exact --ignored' "$RUN_LOG"
rm "$RUN_LOG"
if bash "$helper" exact module::missing -- cargo test module::missing -- --exact; then exit 1; fi
[[ ! -e $RUN_LOG ]]
bash "$helper" exact module::present -- cargo test module::present
grep -Fxq 'test module::present -- --list' "$LIST_LOG"
rm "$RUN_LOG"
bash "$helper" prefix updates:: -- cargo test updates::
grep -Fxq 'test updates:: -- --list' "$LIST_LOG"
grep -Fxq 'test updates::' "$RUN_LOG"
rm "$RUN_LOG"
if bash "$helper" prefix absent:: -- cargo test absent::; then exit 1; fi
[[ ! -e $RUN_LOG ]]
if env FAIL_LIST=23 bash "$helper" exact module::present -- cargo test module::present; then exit 1; else result=$?; fi
[[ $result == 23 && ! -e $RUN_LOG ]]
if env FAIL_RUN=24 bash "$helper" exact module::present -- cargo test module::present; then exit 1; else result=$?; fi
[[ $result == 24 ]]
rm "$RUN_LOG"
rm "$LIST_LOG"
if bash "$helper" invalid module::present -- cargo test module::present; then exit 1; else result=$?; fi
[[ $result == 2 && ! -e $LIST_LOG ]]
bash "$helper" exact TestPresent TestOther -- go test ./pkg -run '^(TestPresent|TestOther)$'
grep -Fxq 'test ./pkg -run ^(TestPresent|TestOther)$' "$RUN_LOG"
rm "$RUN_LOG"
if bash "$helper" exact TestPresent TestOther -- go test ./pkg -run '^TestPresent$'; then exit 1; fi
[[ ! -e $RUN_LOG ]]
if bash "$helper" exact TestPresent TestMissing -- go test ./pkg -run '^TestPresent$'; then exit 1; fi
[[ ! -e $RUN_LOG ]]

workflow="$script_dir/../workflows/test-android.yml"
for name in \
  rcs_receive_tests::go_exported_receive_record_ingests_through_mobile_boundary \
  rcs_receive_tests::go_exported_media_receive_records_ingest_through_mobile_boundary \
  rcs_native::rcs_native_tests::rcs_native_group_receive_tests::go_exported_group_receive_records_ingest_into_a_live_group; do
  grep -F "run-selected-tests.sh exact $name -- cargo test" "$workflow" | grep -Fq "$name -- --exact --ignored"
done
for name in \
  same_live_go_ticket_uses_real_rust_facts_and_commit \
  same_live_go_group_ticket_uses_real_rust_facts_and_commit \
  carrier_live_go_ticket_uses_real_rust_facts_and_commit \
  same_live_go_ticket_uses_real_rust_media_commit \
  same_live_go_group_ticket_uses_real_rust_media_commit \
  same_live_go_ticket_accepts_media_without_a_caption \
  same_live_go_group_media_change_refuses_every_remaining_index \
  same_live_go_group_ticket_refuses_a_changed_roster_before_dispatch; do
  full_name="rcs_native::rcs_native_tests::rcs_native_live_ticket_tests::$name"
  grep -F "run-selected-tests.sh exact $full_name -- cargo test" "$workflow" | grep -Fq "$full_name -- --exact --ignored"
done
grep -Fq 'run-selected-tests.sh exact TestNativeRcsExportObservationFixture TestNativeRcsExportReceiveRecord -- go test' "$workflow"
grep -Fq 'run-selected-tests.sh exact TestNativeRcsContractRustFixture -- go test' "$workflow"
grep -Fq 'run-selected-tests.sh exact TestNativePrepareAcceptsKotlinCarrierBindings -- go test' "$workflow"
grep -Fq 'run-selected-tests.sh exact rcs_send::native_handoff::actual_go_observations_verify_classify_and_begin -- cargo test' "$workflow"
ci="$script_dir/../workflows/ci.yml"
for generator in generate-account-access-contracts generate-rcs-status-contracts generate-launch-progress-contracts generate-notification-activation-contracts; do
  grep -Fq "cargo run --locked -p peppy-desktop-api --bin $generator -- --check" "$ci"
done
grep -Fq 'python3 infra/audit/test-check-advisory-exceptions.py' "$ci"
ruby -ryaml -e 'abort "android filter omits contracts" unless YAML.load_file(ARGV.fetch(0)).fetch("android").include?("packages/contracts/**")' "$script_dir/../ci-paths-filter.yml"
echo 'selected-test guard tests passed'
