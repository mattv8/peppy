#!/usr/bin/env bash
# Restore only into a fresh, distinct Compose project. Existing volumes are never changed.
set -euo pipefail
umask 077

usage() { echo "usage: $0 BACKUP_DIRECTORY NEW_COMPOSE_PROJECT" >&2; exit 64; }
test "$#" = 2 || usage
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 64; }
root=$(pwd -P)
archive_arg=$1
archive=$(cd "$archive_arg" && pwd -P) || { echo "backup directory must exist" >&2; exit 64; }
target_project_arg=$2
target_project=$target_project_arg
case "$target_project" in *[!a-z0-9_-]*|''|[-_]* ) echo "project name must start with lowercase letter/digit and use lowercase letters, digits, _ or -" >&2; exit 64;; esac
test -f "$root/.env" || { echo ".env is required" >&2; exit 64; }
test -f "$archive/manifest.json" && test -f "$archive/manifest.sha256" || { echo "invalid backup directory" >&2; exit 64; }

python3 - "$archive" <<'PY'
import json, os, re, sys
d=sys.argv[1]; expected={"manifest.json", "manifest.sha256", "postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz"}
m=json.load(open(os.path.join(d,"manifest.json")))
artifacts=set(m.get("artifacts",[]))
baseline=expected-{ "manifest.json", "manifest.sha256"}
allowed=baseline|{"mcp-state.tar.gz"}
if m.get("format_version") != 2 or artifacts not in (baseline, allowed): raise SystemExit("unsupported backup manifest")
if set(os.listdir(d)) != artifacts|{"manifest.json", "manifest.sha256"}: raise SystemExit("backup has an unexpected or missing artifact")
checksum_entries=[]
with open(os.path.join(d, "manifest.sha256"), encoding="ascii") as checksums:
    for line in checksums.read().splitlines():
        match=re.fullmatch(r"[0-9a-fA-F]{64}  (manifest\.json|postgres\.dump|seaweed-data\.tar\.gz|seaweed-home\.tar\.gz|mcp-state\.tar\.gz)", line)
        if not match: raise SystemExit("manifest.sha256 has an invalid entry")
        checksum_entries.append(match.group(1))
required=artifacts|{"manifest.json"}
if len(checksum_entries) != len(required) or set(checksum_entries) != required:
    raise SystemExit("manifest.sha256 must list each required artifact exactly once")
PY
( cd "$archive" && shasum -a 256 -c manifest.sha256 )
postgres_image=$(python3 - "$archive/manifest.json" <<'PY'
import json, re, sys
image = json.load(open(sys.argv[1]))["images"]["running_ids"]["postgres"]
if not isinstance(image, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
    raise SystemExit("backup PostgreSQL image ID must be a pinned sha256 digest")
print(image)
PY
)
source_project=$(python3 - "$archive/manifest.json" <<'PY'
import json,sys
print(json.load(open(sys.argv[1]))["source_project"])
PY
)
test "$target_project" != "$source_project" || { echo "restore target must differ from source project" >&2; exit 64; }
compose=(docker compose --project-name "$target_project" --project-directory "$root" --env-file "$root/.env")
config=$(mktemp); volumes_file=$(mktemp); trap 'rm -f "$config" "$volumes_file"' EXIT
"${compose[@]}" config --format json > "$config"
target_postgres_image=$(python3 - "$config" <<'PY'
import json,sys
print(json.load(open(sys.argv[1]))["services"]["postgres"].get("image", ""))
PY
)
if ! local_postgres_id=$(docker image inspect --format '{{.Id}}' "$postgres_image") || test "$local_postgres_id" != "$postgres_image"; then
  echo "backup PostgreSQL image is not available locally: $postgres_image" >&2
  exit 65
fi
if test -n "$target_postgres_image"; then
  if ! target_postgres_id=$(docker image inspect --format '{{.Id}}' "$target_postgres_image") || test "$target_postgres_id" != "$postgres_image"; then
    echo "target PostgreSQL image does not match backup: $target_postgres_image" >&2
    exit 65
  fi
fi
python3 - "$config" "$archive/manifest.json" <<'PY'
import json,sys
c=json.load(open(sys.argv[1])); m=json.load(open(sys.argv[2]))
for service in ("postgres", "seaweedfs"):
    if c["services"][service].get("image", "") != m["images"]["configured"][service]: raise SystemExit(f"target {service} image does not match backup")
has_mcp_artifact = "mcp-state.tar.gz" in m["artifacts"]
has_mcp_overlay = "mcp" in c["services"]
if has_mcp_artifact != has_mcp_overlay: raise SystemExit("MCP overlay and backup artifact must be selected together")
if has_mcp_artifact:
    if c["services"]["mcp"].get("image", "") != m["images"]["configured"].get("mcp", ""):
        raise SystemExit("target mcp image does not match backup")
    mounts = {v.get("target"): v for v in c["services"]["mcp"].get("volumes", []) if v.get("type") == "volume"}
    if not {"/var/lib/peppy-mcp", "/run/peppy-mcp"} <= set(mounts): raise SystemExit("target MCP storage layout mismatch")
PY
python3 - "$config" > "$volumes_file" <<'PY'
import json,sys
c=json.load(open(sys.argv[1]))
for service, target in (("postgres","/var/lib/postgresql"),("seaweedfs","/data"),("seaweedfs","/root/.seaweedfs")):
  m=next((v for v in c["services"][service]["volumes"] if v.get("target")==target and v.get("type")=="volume"),None)
  if not m: raise SystemExit("target storage layout mismatch")
  print(c["volumes"][m["source"]]["name"])
if "mcp" in c["services"]:
   mounts={v.get("target"): v for v in c["services"]["mcp"].get("volumes", []) if v.get("type")=="volume"}
   for target in ("/var/lib/peppy-mcp", "/run/peppy-mcp"):
      print(c["volumes"][mounts[target]["source"]]["name"])
PY
volumes=()
while IFS= read -r volume; do volumes[${#volumes[@]}]="$volume"; done < "$volumes_file"
case "${#volumes[@]}" in 3) mcp_enabled=0 ;; 5) mcp_enabled=1 ;; *) echo "unexpected target storage layout" >&2; exit 65 ;; esac
for volume in "${volumes[@]}"; do
  docker volume inspect "$volume" >/dev/null 2>&1 && { echo "restore target volume already exists: $volume" >&2; exit 73; }
done

"${compose[@]}" create postgres seaweedfs >/dev/null
data_volume=${volumes[1]}; home_volume=${volumes[2]}
docker run --pull never --rm -v "$data_volume:/destination" -v "$archive:/backup:ro" "$postgres_image" tar -C /destination -xzf /backup/seaweed-data.tar.gz
docker run --pull never --rm -v "$home_volume:/destination" -v "$archive:/backup:ro" "$postgres_image" tar -C /destination -xzf /backup/seaweed-home.tar.gz
if test "$mcp_enabled" = 1; then
  mcp_state_volume=${volumes[3]}
  "${compose[@]}" create mcp-state-init >/dev/null
  docker run --pull never --rm -v "$mcp_state_volume:/destination" -v "$archive:/backup:ro" "$postgres_image" sh -ec 'tar -C /destination -xzf /backup/mcp-state.tar.gz && chown 10001:10001 /destination && chmod 0700 /destination'
fi
if ! "${compose[@]}" up --wait postgres; then
  echo "PostgreSQL startup failed; restore aborted before pg_restore" >&2
  exit 74
fi
"${compose[@]}" exec -T postgres sh -ec 'pg_restore --single-transaction --exit-on-error -U "$POSTGRES_USER" -d "$POSTGRES_DB" --no-owner --no-privileges' < "$archive/postgres.dump"
echo "Restore complete for $target_project. API and migrations remain stopped for operator reconciliation." >&2
