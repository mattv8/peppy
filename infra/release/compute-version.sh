#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
. "$script_dir/lib.sh"

channel=dev
bump=auto
explicit_version=
allow_auto_patch_fallback=false
component=desktop
while (($#)); do
    case $1 in
        --channel) channel=${2:-}; shift 2 ;;
        --bump) bump=${2:-}; shift 2 ;;
        --version) explicit_version=${2:-}; shift 2 ;;
        --allow-auto-patch-fallback) allow_auto_patch_fallback=true; shift ;;
        --component) component=${2:-}; shift 2 ;;
        *) release_die "usage: $0 [--channel stable|prerelease|dev] [--bump auto|patch|minor|major] [--version X.Y.Z] [--allow-auto-patch-fallback] [--component desktop|android]" 2 ;;
    esac
done
case $channel in stable|prerelease|dev) ;; *) release_die "invalid channel: $channel" 2 ;; esac
case $bump in auto|patch|minor|major) ;; *) release_die "invalid bump: $bump" 2 ;; esac
case $component in desktop|android) ;; *) release_die "invalid component: $component" 2 ;; esac
if [[ -n "$explicit_version" && $channel != stable ]]; then release_die "--version is only valid for stable releases" 2; fi
if [[ -n "$explicit_version" && ! "$explicit_version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then release_die "invalid version: $explicit_version" 2; fi

repo_root=$(git rev-parse --show-toplevel) || release_die "not inside a git repository"
if [[ $(git rev-parse --is-shallow-repository) == true ]]; then release_die "shallow repositories are unsupported" 2; fi
prefix=$(peppy_tag_prefix "$component")
stable_tag_regex=$(peppy_stable_tag_regex "$prefix")
last_stable=$(peppy_last_stable_tag "$component")
if [[ -n "$last_stable" ]]; then commits_since=$(git rev-list --count "$last_stable..HEAD"); else commits_since=$(git rev-list --count HEAD); fi
head_released=false
for tag in $(git tag --points-at HEAD); do
    if [[ $tag =~ $stable_tag_regex ]]; then head_released=true; break; fi
done
if [[ $head_released == true && $channel == stable ]]; then release_die "HEAD already carries a stable release tag" 4; fi
if [[ -n "$explicit_version" ]] && git rev-parse -q --verify "refs/tags/$prefix$explicit_version" >/dev/null; then
    release_die "tag $prefix$explicit_version already exists" 2
fi

last_stable_version=${last_stable#android-v}
last_stable_version=${last_stable_version#v}
android_paths=()
if [[ $component == android ]]; then
    while IFS= read -r path; do android_paths+=("$path"); done < <(peppy_android_paths)
fi
release_range=("$last_stable..HEAD")
if [[ $component == android ]]; then release_range+=(-- "${android_paths[@]}"); fi

if [[ -z "$last_stable" ]]; then
    base_version=0.1.0
    resolved_bump=initial
    if [[ $bump != auto ]]; then printf '%s\n' "ignoring --bump without a stable tag" >&2; fi
elif [[ -n "$explicit_version" ]]; then
    if [[ $(semver_compare "$explicit_version" "$last_stable") != 1 ]]; then release_die "explicit version must exceed $last_stable" 2; fi
    base_version=$explicit_version
    resolved_bump=explicit
elif [[ $bump == auto ]]; then
    command -v git-cliff >/dev/null || release_die "git-cliff is required for automatic bumps"
    cliff_version=$(git-cliff --version | awk '{print $2}')
    if [[ $cliff_version != 2.14.2 ]]; then printf '%s\n' "warning: git-cliff 2.14.2 is expected (found $cliff_version)" >&2; fi
    cliff_args=(--config "$script_dir/cliff.toml" --bumped-version)
    if [[ $component == android ]]; then
        if [[ $last_stable == android-v* ]]; then
            cliff_args+=(--tag-pattern "$stable_tag_regex")
        else
            # Before the first Android tag, only the bootstrap base counts; later v* tags must not move Android.
            cliff_args+=(--tag-pattern "$PEPPY_STABLE_TAG_REGEX" --count-tags "^${last_stable//./\\.}\$")
        fi
        for path in "${android_paths[@]}"; do cliff_args+=(--include-path "$path" --include-path "$path/**"); done
    fi
    base_version=$(cd "$repo_root" && git-cliff "${cliff_args[@]}")
    base_version=${base_version#android-v}
    base_version=${base_version#v}
    if [[ -z $(git log --no-merges -E --grep='BREAKING CHANGE:' --format=%H "${release_range[@]}") ]]; then
        only_non_releasable=true
        # keep in sync with infra/release/cliff.toml
        non_releasable_regex='^((docs|ci|build|refactor|test|chore|style)(\([^)]*\))?:|Merge (pull request|branch|remote-tracking branch) )'
        while IFS= read -r subject; do
            if [[ ! $subject =~ $non_releasable_regex ]]; then
                only_non_releasable=false
                break
            fi
        done < <(git log --no-merges --format=%s "${release_range[@]}")
        if [[ $only_non_releasable == true ]]; then base_version=$last_stable_version; fi
    fi
    if [[ $base_version == "$last_stable_version" ]]; then
        if [[ $channel == stable && $allow_auto_patch_fallback != true ]]; then release_die "no releasable commits since $last_stable; pass --bump or --version" 3; fi
        base_version=$(semver_bump "$last_stable_version" patch)
        resolved_bump=none-patch
    else
        IFS=. read -r old_major old_minor old_patch <<EOF
$last_stable_version
EOF
        IFS=. read -r new_major new_minor new_patch <<EOF
$base_version
EOF
        if [[ $new_major != "$old_major" ]]; then
            resolved_bump=major
        elif [[ $new_minor != "$old_minor" ]]; then
            resolved_bump=minor
        elif [[ $new_patch != "$old_patch" ]]; then
            resolved_bump='patch'
        else
            resolved_bump='patch'
        fi
    fi
else
    base_version=$(semver_bump "$last_stable_version" "$bump")
    resolved_bump=$bump
fi
if [[ -z "$last_stable" && -n "$explicit_version" ]]; then
    base_version=$explicit_version
    resolved_bump=explicit
fi

if [[ $head_released == true ]]; then
    base_version=$(semver_bump "$last_stable_version" patch)
    commits_since=0
fi
case $channel in
    stable) version=$base_version; sequence=999 ;;
    prerelease) version=$base_version-staging.$commits_since; sequence=$commits_since ;;
    dev) version=$base_version-dev.$commits_since; sequence=$commits_since ;;
esac
IFS=. read -r major minor patch <<EOF
$base_version
EOF
if ((10#$major > 209 || 10#$minor > 99 || 10#$patch > 99)) || { [[ $channel != stable ]] && ((10#$sequence > 998)); }; then release_die "version cannot be represented by Android/WiX" 5; fi
if ((10#$major > 255)); then release_die "version cannot be represented by WiX" 5; fi
android_version_code=$((10#$major * 10000000 + 10#$minor * 100000 + 10#$patch * 1000 + 10#$sequence))
printf 'version=%s\ntag=%s%s\nbase_version=%s\nchannel=%s\nbump=%s\nlast_stable=%s\ncommits_since=%s\nhead_released=%s\nandroid_version_code=%s\nwix_version=%s.%s.%s.%s\n' "$version" "$prefix" "$version" "$base_version" "$channel" "$resolved_bump" "$last_stable" "$commits_since" "$head_released" "$android_version_code" "$major" "$minor" "$patch" "$sequence"
if [[ $component == android ]]; then
    changed_base=$last_stable
    if [[ $channel != stable ]]; then
        prerelease_tag_regex=$(peppy_prerelease_tag_regex "$prefix")
        for tag in $(git tag --merged HEAD); do
            if [[ $tag =~ $prerelease_tag_regex ]] && { [[ -z "$changed_base" ]] || [[ $(semver_compare "$tag" "$changed_base") == 1 ]]; }; then
                changed_base=$tag
            fi
        done
    fi
    if [[ -z $changed_base ]] || git log -1 --format=%H "$changed_base..HEAD" -- "${android_paths[@]}" | grep -q .; then changed=true; else changed=false; fi
    if ((commits_since > 500)); then budget_refresh=true; else budget_refresh=false; fi
    printf 'changed=%s\nbudget_refresh=%s\n' "$changed" "$budget_refresh"
fi
