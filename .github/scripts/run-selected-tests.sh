#!/usr/bin/env bash
set -euo pipefail

mode=$1
shift
case "$mode" in
  exact|prefix) ;;
  *) echo "unsupported selection mode: $mode" >&2; exit 2 ;;
esac
names=()
while [[ $# -gt 0 && $1 != -- ]]; do
  names+=("$1")
  shift
done
[[ ${#names[@]} -gt 0 && $# -gt 1 ]] || { echo "expected mode, names, -- and command" >&2; exit 2; }
shift

case "$1" in
  cargo)
    has_separator=false
    for arg in "$@"; do
      if [[ $arg == -- ]]; then has_separator=true; break; fi
    done
    if "$has_separator"; then
      listing=$("$@" --list)
    else
      listing=$("$@" -- --list)
    fi
    ;;
  go)
    [[ $mode == exact ]] || { echo "unsupported selection mode for go: $mode" >&2; exit 2; }
    run_pattern=
    previous=
    for arg in "$@"; do
      if [[ $previous == -run || $previous == -test.run ]]; then run_pattern=$arg; fi
      case "$arg" in
        -run=*|-test.run=*) run_pattern=${arg#*=} ;;
      esac
      previous=$arg
    done
    [[ -n $run_pattern ]] || { echo "go test requires a -run pattern" >&2; exit 2; }
    listing=$("$@" -list "$run_pattern")
    ;;
  *) echo "unsupported test command: $1" >&2; exit 2 ;;
esac

for name in "${names[@]}"; do
  case "$mode:$1" in
    exact:cargo) grep -Fxq -- "$name: test" <<< "$listing" ;;
    exact:go) grep -Fxq -- "$name" <<< "$listing" ;;
    prefix:cargo)
      found=false
      while IFS= read -r line; do
        if [[ $line == "$name"*": test" ]]; then found=true; break; fi
      done <<< "$listing"
      "$found"
      ;;
    *) echo "unsupported selection mode: $mode" >&2; exit 2 ;;
  esac || { echo "selected test not found: $name" >&2; exit 1; }
done

exec "$@"
