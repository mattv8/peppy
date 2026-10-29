# Contribution policy

## Policy reference

`.github/CODEOWNERS` assigns every repository path to `@mattv8`. The contribution-review ruleset applies to `production` and `staging`. It allows merge and rebase methods, requires one approval from a code owner, dismisses stale approvals after a push, requires an approver other than the last pusher, and requires an extra approval for unattributed changes.

Repository administrators (GitHub repository role `5`) have a pull-request-only bypass for the review ruleset. This is a role-based exception, not an author-specific exception: every current or future administrator receives it. Pull request authors never self-approve. `@mattv8` uses the explicit review bypass for an owner-authored pull request; if `@mattv8` pushes a fixup to a contributor pull request, the last-push requirement similarly requires that bypass to merge it.

The required-CI ruleset applies to the same full branch refs and requires the GitHub Actions `gate` check from app `15368` in strict mode. It has no bypass actors. A missing or failed `gate` check freezes merges, including during an outage, unless an administrator makes an explicit, reviewed ruleset change. Do not replace this requirement with path-filtered job contexts.

The production-integrity payload is the in-place replacement for existing ruleset `24377145` (`Default`). It protects `refs/heads/production` with non-fast-forward and linear-history rules and has no bypass. The current classic production setting allows force pushes; preserve that setting in this task because the new no-bypass history ruleset forbids force pushes. Existing deletion protection and classic CI/admin restrictions remain separate and must not be removed.

## CODEOWNERS bootstrap

GitHub evaluates CODEOWNERS from the pull request's base branch. Code-owner enforcement cannot use this file for a target branch until `.github/CODEOWNERS` exists on that branch. Merge this bootstrap policy into each target base before treating code-owner review as active there.

## Apply and verify

An administrator activates these payloads in GitHub repository settings or with the GitHub rulesets API. Apply them in this order so required CI remains in force:

1. Create or update `required-ci.json` for both branches.
2. Create `contribution-review.json` for both branches.
3. Last, update ruleset `24377145` in place with `production-integrity.json` using `PUT`. Do not delete and recreate `Default`.

For API application, use the repository rulesets endpoint with each payload as its request body. The final update targets `repos/mattv8/peppy/rulesets/24377145`; creation uses `repos/mattv8/peppy/rulesets`. The payload files do not change repository settings until an administrator applies them. Do not add a PR workflow or a personal-access-token workaround to apply them.

After activation, inspect the GitHub settings and API responses to confirm the exact refs, rule parameters, bypass actors, `gate` integration ID, and active enforcement. Also verify that the separate deletion and classic restrictions still exist. Test a pull request on each base branch before relying on the policy.
