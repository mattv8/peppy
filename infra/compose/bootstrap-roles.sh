#!/bin/sh
set -eu

: "${PEPPY_RUNTIME_DB_USER:?PEPPY_RUNTIME_DB_USER is required}"
: "${PEPPY_RUNTIME_DB_PASSWORD:?PEPPY_RUNTIME_DB_PASSWORD is required}"
: "${PEPPY_MAINTENANCE_DB_USER:?PEPPY_MAINTENANCE_DB_USER is required}"
: "${PEPPY_MAINTENANCE_DB_PASSWORD:?PEPPY_MAINTENANCE_DB_PASSWORD is required}"

sh /usr/local/bin/validate-db-passwords.sh

if [ "$PEPPY_RUNTIME_DB_USER" = "$POSTGRES_USER" ] || [ "$PEPPY_MAINTENANCE_DB_USER" = "$POSTGRES_USER" ] || [ "$PEPPY_RUNTIME_DB_USER" = "$PEPPY_MAINTENANCE_DB_USER" ]; then
  echo "database runtime and maintenance roles must be distinct from the owner and each other" >&2
  exit 1
fi

psql --set=ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  --set=runtime_role="$PEPPY_RUNTIME_DB_USER" \
  --set=runtime_password="$PEPPY_RUNTIME_DB_PASSWORD" \
  --set=maintenance_role="$PEPPY_MAINTENANCE_DB_USER" \
  --set=maintenance_password="$PEPPY_MAINTENANCE_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS PASSWORD %L', :'runtime_role', :'runtime_password')
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'runtime_role')
\gexec
SELECT format('ALTER ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS PASSWORD %L', :'runtime_role', :'runtime_password')
\gexec
SELECT format('CREATE ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT BYPASSRLS PASSWORD %L', :'maintenance_role', :'maintenance_password')
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'maintenance_role')
\gexec
SELECT format('ALTER ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT BYPASSRLS PASSWORD %L', :'maintenance_role', :'maintenance_password')
\gexec
SQL
