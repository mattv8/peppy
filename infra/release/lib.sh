#!/usr/bin/env bash

# shellcheck disable=SC2034
PEPPY_STABLE_TAG_REGEX='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
# shellcheck disable=SC2034
# `main` remains accepted while the branch migration is rolling out so a
# stable release cannot move backward relative to an already-published tag.
PEPPY_PRERELEASE_TAG_REGEX='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-(main|staging)\.(0|[1-9][0-9]*)$'

: "${PEPPY_ANDROID_BOOTSTRAP_TAG:=v0.17.1}"

peppy_tag_prefix() {
    case $1 in
        desktop) printf 'v\n' ;;
        android) printf 'android-v\n' ;;
        *) return 2 ;;
    esac
}

peppy_stable_tag_regex() {
    case $1 in
        v) printf '%s\n' "$PEPPY_STABLE_TAG_REGEX" ;;
        android-v) printf '^android-v%s\n' "${PEPPY_STABLE_TAG_REGEX:2}" ;;
        *) return 2 ;;
    esac
}

peppy_prerelease_tag_regex() {
    case $1 in
        v) printf '%s\n' "$PEPPY_PRERELEASE_TAG_REGEX" ;;
        android-v) printf '%s\n' '^android-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-staging\.(0|[1-9][0-9]*)$' ;;
        *) return 2 ;;
    esac
}

peppy_android_paths() {
    local script_dir
    script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$script_dir/components/android.paths"
}

peppy_last_stable_tag() {
    local component prefix regex tag last_stable bootstrap_tag
    component=$1
    prefix=$(peppy_tag_prefix "$component") || return
    regex=$(peppy_stable_tag_regex "$prefix") || return
    for tag in $(git tag --merged HEAD); do
        if [[ $tag =~ $regex ]] && { [[ -z "$last_stable" ]] || [[ $(semver_compare "$tag" "$last_stable") == 1 ]]; }; then
            last_stable=$tag
        fi
    done
    if [[ $component == android && -z "$last_stable" ]]; then
        for tag in $(git tag --merged HEAD); do
            if [[ $tag =~ $PEPPY_STABLE_TAG_REGEX ]] && [[ $(semver_compare "$tag" "$PEPPY_ANDROID_BOOTSTRAP_TAG") != 1 ]] && { [[ -z "$bootstrap_tag" ]] || [[ $(semver_compare "$tag" "$bootstrap_tag") == 1 ]]; }; then
                bootstrap_tag=$tag
            fi
        done
        last_stable=$bootstrap_tag
    fi
    printf '%s\n' "$last_stable"
}

semver_compare() {
    local a b a_major a_minor a_patch a_word a_number b_major b_minor b_patch b_word b_number
    a=${1#android-}
    b=${2#android-}
    a=${a#v}
    b=${b#v}
    [[ "$a" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-([[:alnum:]_]+)\.([0-9]+))?$ ]] || return 2
    a_major=${BASH_REMATCH[1]}; a_minor=${BASH_REMATCH[2]}; a_patch=${BASH_REMATCH[3]}; a_word=${BASH_REMATCH[5]:-}; a_number=${BASH_REMATCH[6]:-}
    [[ "$b" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-([[:alnum:]_]+)\.([0-9]+))?$ ]] || return 2
    b_major=${BASH_REMATCH[1]}; b_minor=${BASH_REMATCH[2]}; b_patch=${BASH_REMATCH[3]}; b_word=${BASH_REMATCH[5]:-}; b_number=${BASH_REMATCH[6]:-}
    if ((10#$a_major < 10#$b_major)); then echo -1; return; fi
    if ((10#$a_major > 10#$b_major)); then echo 1; return; fi
    if ((10#$a_minor < 10#$b_minor)); then echo -1; return; fi
    if ((10#$a_minor > 10#$b_minor)); then echo 1; return; fi
    if ((10#$a_patch < 10#$b_patch)); then echo -1; return; fi
    if ((10#$a_patch > 10#$b_patch)); then echo 1; return; fi
    if [[ -z "$a_word" && -n "$b_word" ]]; then echo 1; return; fi
    if [[ -n "$a_word" && -z "$b_word" ]]; then echo -1; return; fi
    if [[ -z "$a_word" ]]; then echo 0; return; fi
    if [[ "$a_word" < "$b_word" ]]; then echo -1; return; fi
    if [[ "$a_word" > "$b_word" ]]; then echo 1; return; fi
    if ((10#$a_number < 10#$b_number)); then echo -1; elif ((10#$a_number > 10#$b_number)); then echo 1; else echo 0; fi
}

semver_bump() {
    local version major minor patch
    version=${1#v}
    case $2 in patch|minor|major) ;; *) return 2 ;; esac
    if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then return 2; fi
    major=${BASH_REMATCH[1]}; minor=${BASH_REMATCH[2]}; patch=${BASH_REMATCH[3]}
    case $2 in
        patch) patch=$((patch + 1)) ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        major) major=$((major + 1)); minor=0; patch=0 ;;
    esac
    printf '%s.%s.%s\n' "$major" "$minor" "$patch"
}

release_die() {
    printf '%s\n' "$1" >&2
    exit "${2:-1}"
}

release_update_targets() {
    local version=$1
    cat <<EOF
linux-x86_64-appimage|peppy-$version-linux-x86_64.AppImage
linux-x86_64-deb|peppy-$version-linux-x86_64.deb
linux-x86_64-rpm|peppy-$version-linux-x86_64.rpm
darwin-aarch64-app|peppy-$version-darwin-aarch64.app.tar.gz
windows-x86_64-nsis|peppy-$version-windows-x86_64-setup.exe
windows-x86_64-msi|peppy-$version-windows-x86_64.msi
EOF
}

release_linux_package_version() {
    local version=$1
    case "$version" in
        *-staging.*)
            [[ "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)-staging\.([0-9]+)$ ]] || return 2
            printf '%s~staging.%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
            ;;
        *)
            [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 2
            printf '%s\n' "$version"
            ;;
    esac
}

release_linux_rpm_release() {
    local version=$1
    release_linux_package_version "$version" >/dev/null || return
    printf '1\n'
}
