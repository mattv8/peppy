#!/usr/bin/env bash
set -euo pipefail

apply=false
if [[ ${1:-} == --apply ]]; then
    apply=true
    shift
fi
[[ $# -eq 0 ]] || { echo "usage: $0 [--apply]" >&2; exit 2; }
command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }

origin=$(git remote get-url origin)
repo=$(gh repo view "$origin" --json nameWithOwner,visibility --jq '.nameWithOwner + " " + .visibility')
name=${repo%% *}
visibility=${repo#* }
[[ $visibility == PUBLIC ]] || { echo "refusing non-public repository: $name" >&2; exit 1; }
for branch in production staging; do
    git ls-remote --exit-code --heads origin "refs/heads/$branch" >/dev/null || {
        echo "missing remote branch: $branch" >&2
        exit 1
    }
done

echo "Validated public repository $name and remote production/staging branches."
echo "Before applying, ensure the branch ruleset requires the CI job named: gate"
if ! $apply; then
    echo "Dry run only. Re-run with --apply to set the remote default branch to production."
    exit 0
fi
gh api --method PATCH "repos/$name" -f default_branch=production >/dev/null
echo "Remote default branch is now production. Review rulesets manually; this script never weakens protection."
