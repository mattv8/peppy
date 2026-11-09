#!/bin/sh
set -eu

validate_password() {
  case "$1" in
    ''|*[!A-Za-z0-9._~-]*)
      echo "database passwords must use only URI-unreserved characters: A-Z a-z 0-9 . _ ~ -" >&2
      exit 1
      ;;
  esac
}

: "${POSTGRES_PASSWORD:?POSTGRES_PASSWORD is required}"
: "${PEPPY_RUNTIME_DB_PASSWORD:?PEPPY_RUNTIME_DB_PASSWORD is required}"
: "${PEPPY_MAINTENANCE_DB_PASSWORD:?PEPPY_MAINTENANCE_DB_PASSWORD is required}"

for password in "$POSTGRES_PASSWORD" "$PEPPY_RUNTIME_DB_PASSWORD" "$PEPPY_MAINTENANCE_DB_PASSWORD"; do
  validate_password "$password"
done
