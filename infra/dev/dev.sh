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
  dev-up|dev-down|dev-build|dev-test|ci-test)
    require_container_tooling
    exec just "$recipe" "$@" ;;
  dev-start|android-build|android-test|android-emulator|android-deploy|android-open|android-run|android-smoke|android-sms|desktop-dev|desktop-bundle|desktop-run|desktop-open|ios-run)
    exec just "$recipe" "$@" ;;
  *)
    echo "unknown development recipe: ${recipe:-<missing>}" >&2
    echo "run 'just --list' for available recipes" >&2
    exit 64 ;;
esac
