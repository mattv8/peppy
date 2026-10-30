# Contribution policy

## Policy reference

`.github/CODEOWNERS` assigns every repository path to `@mattv8`. The contribution-review ruleset applies to `production` and `staging`. It allows merge and rebase methods, requires one approval from a code owner, dismisses stale approvals after a push, requires an approver other than the last pusher, and requires an extra approval for unattributed changes. Its sole bypass actor is GitHub user `mattv8` (ID `9312603`), with `exempt` mode; it is not a repository-role bypass.

The active **Pull request required** baseline also applies to `production` and `staging`. It requires pull requests with merge or rebase methods, has zero required approvals, does not require code-owner or last-push approval, and has no bypass actors. This mandatory baseline prevents direct pushes even when `mattv8` is exempt from Contribution review.

The required-CI ruleset applies to the same full branch refs and requires the GitHub Actions `gate` check from app `15368` in strict mode. It has no bypass actors. A missing or failed `gate` check freezes merges, including during an outage, unless an administrator makes an explicit, reviewed ruleset change. Do not replace this requirement with path-filtered job contexts.

Existing history and deletion protections remain separate rulesets and must not be removed or weakened when applying the pull-request policy.

## CODEOWNERS bootstrap

GitHub evaluates CODEOWNERS from the pull request's base branch. Code-owner enforcement cannot use this file for a target branch until `.github/CODEOWNERS` exists on that branch. Merge this bootstrap policy into each target base before treating code-owner review as active there.

## Apply and verify

An administrator activates these payloads in GitHub repository settings or with the GitHub rulesets API. Apply them in this order so the mandatory no-bypass pull-request gate exists before the review exemption:

1. Create `pull-request-required.json` for both branches.
2. Update existing Contribution review ruleset `26088116` with `contribution-review.json`, replacing only its old repository-role bypass with the `mattv8` exemption.
3. Confirm Required CI and the separate history/deletion rulesets remain unchanged.

For API application, use the repository rulesets endpoint with each payload as its request body. Creation uses `repos/mattv8/peppy/rulesets`; the review update targets `repos/mattv8/peppy/rulesets/26088116`. The payload files do not change repository settings until an administrator applies them. Do not add a PR workflow or a personal-access-token workaround to apply them.

After activation, inspect the GitHub settings and API responses to confirm the exact refs, rule parameters, bypass actors, `gate` integration ID, and active enforcement. Also verify that the separate history and deletion restrictions still exist. Test a pull request on each base branch before relying on the policy.
