#!/usr/bin/env bash
set -euo pipefail

ROOT="${PEPPY_REPOSITORY_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
cd "$ROOT"
recipe="${1:-}"
shift || true

require_actions_installer() {
  command -v python3 >/dev/null || { echo "development action setup requires python3" >&2; exit 1; }
  if [[ ! -f infra/dev/install-actions.py ]]; then
    echo "development action setup needs infra/dev/install-actions.py (provided by the actions workflow lane); rerun after it is available" >&2
    exit 1
  fi
}

require_container_tooling() {
  command -v docker >/dev/null || { echo "container development requires docker" >&2; exit 1; }
  docker compose version >/dev/null 2>&1 || { echo "container development requires docker compose" >&2; exit 1; }
}

select_source_tree() {
  if ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if [[ -n "${PEPPY_SOURCE_TREE+x}" ]]; then
      echo "PEPPY_SOURCE_TREE requires a registered Git worktree" >&2
      exit 1
    fi
    PEPPY_SOURCE_TREE="$ROOT"
    export PEPPY_SOURCE_TREE
    return
  fi
  command -v python3 >/dev/null || { echo "development source selection requires python3" >&2; exit 1; }
  PEPPY_SOURCE_TREE="$(python3 infra/dev/worktree_source.py --root "$ROOT")"
  export PEPPY_SOURCE_TREE
}

case "$recipe" in
  dev-setup)
    require_actions_installer
    if [[ -L .env ]]; then
      echo "refusing to replace symlinked .env" >&2
      exit 1
    fi
    if [[ ! -e .env ]]; then
      python3 infra/dev/dev_port.py write-setup .env
      echo "Created .env with synthetic local credentials (mode 0600)." >&2
    fi
    exec python3 infra/dev/install-actions.py "$@" ;;
  dev-actions)
    require_actions_installer
    exec python3 infra/dev/install-actions.py --install "$@" ;;
  dev-demo)
    require_container_tooling
    exec python3 infra/dev/demo.py "$@" ;;
  dev-up|dev-build)
    select_source_tree
    require_container_tooling
    exec just "$recipe" "$@" ;;
  dev-down|dev-test|ci-test)
    require_container_tooling
    exec just "$recipe" "$@" ;;
  dev-start|android-build|android-run|desktop-dev|desktop-bundle|desktop-run|ios-run)
    select_source_tree
    exec just "$recipe" "$@" ;;
  android-test|android-emulator|android-deploy|android-open|android-smoke|android-sms|desktop-open)
    exec just "$recipe" "$@" ;;
  *)
    echo "unknown development recipe: ${recipe:-<missing>}" >&2
    echo "run 'just --list' for available recipes" >&2
    exit 64 ;;
esac
