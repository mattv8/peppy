#!/usr/bin/env bash
set -euo pipefail

# Administrator-operated opt-in only. --dry-run is a fixture mode: it does not
# download keys or modify host configuration.
usage() { echo "usage: $0 --distro <debian|fedora> --repo-url https://... --key-url https://... --key-fingerprint <40-hex> --channel <stable|staging> [--dry-run]" >&2; exit 2; }
fail() { echo "$*" >&2; exit 1; }
valid_url() { [[ "$1" =~ ^https://[A-Za-z0-9.-]+(:[0-9]+)?(/[A-Za-z0-9._~:/@!\&\(\)*+,;=%-]*)?$ ]]; }
url_authority() { printf '%s\n' "${1#https://}" | sed 's#/.*##'; }
write_file() {
  local path=$1 content=$2
  if "$dry_run"; then printf '+ write %s\n%s\n' "$path" "$content"; else [ ! -e "$path" ] || fail "refusing to replace existing $path"; printf '%s\n' "$content" > "$path"; fi
}

distro=''; repo_url=''; key_url=''; fingerprint=''; channel=''; dry_run=false
while [ "$#" -gt 0 ]; do case "$1" in --distro) distro=${2:-}; shift 2;; --repo-url) repo_url=${2:-}; shift 2;; --key-url) key_url=${2:-}; shift 2;; --key-fingerprint) fingerprint=${2:-}; shift 2;; --channel) channel=${2:-}; shift 2;; --dry-run) dry_run=true; shift;; *) usage;; esac; done
case "$distro" in debian|fedora) ;; *) usage;; esac
case "$channel" in stable|staging) ;; *) usage;; esac
if ! valid_url "$repo_url" || ! valid_url "$key_url"; then
  echo 'repository and key URLs must be strict HTTPS URLs' >&2
  exit 2
fi
[ "$(url_authority "$repo_url")" = "$(url_authority "$key_url")" ] || fail 'repository and key URLs must use the same HTTPS authority'
fingerprint=$(printf '%s' "$fingerprint" | tr '[:lower:]' '[:upper:]'); [[ "$fingerprint" =~ ^[0-9A-F]{40}$ ]] || { echo 'key fingerprint must be exactly 40 hexadecimal characters' >&2; exit 2; }
[ "$(id -u)" = 0 ] || "$dry_run" || fail 'run this explicit administrator setup with root privileges'

key_tmp=$(mktemp); trap 'rm -f "$key_tmp"' EXIT
if ! "$dry_run"; then
  curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 "$key_url" -o "$key_tmp"
  read -r primary_count actual < <(gpg --show-keys --with-colons "$key_tmp" | awk -F: '$1 == "pub" { want_fpr=1; next } $1 == "fpr" && want_fpr { primaries++; primary=toupper($10); want_fpr=0 } END { print primaries, primary }')
  [ "$primary_count" = 1 ] && [ "$actual" = "$fingerprint" ] || fail 'downloaded repository key must contain exactly the pinned primary fingerprint'
fi

if [ "$distro" = debian ]; then
  if "$dry_run"; then printf '+ install -d -m 0755 /etc/apt/keyrings\n'; else install -d -m 0755 /etc/apt/keyrings; fi
  if "$dry_run"; then printf '+ verify key fingerprint %s\n+ install key /etc/apt/keyrings/peppy.asc\n' "$fingerprint"; else install -m 0644 "$key_tmp" /etc/apt/keyrings/peppy.asc; fi
  write_file /etc/apt/sources.list.d/peppy.list "deb [arch=amd64 signed-by=/etc/apt/keyrings/peppy.asc] ${repo_url%/}/apt $channel main"
  write_file /etc/apt/preferences.d/peppy "Package: *
Pin: origin $(url_authority "$repo_url")
Pin-Priority: -1

Package: peppy
Pin: origin $(url_authority "$repo_url")
Pin-Priority: 990"
  service='[Unit]
Description=Update Peppy only from its configured signed repository
Wants=network-online.target
After=network-online.target
[Service]
Type=oneshot
ExecStartPre=/usr/bin/apt-get update
ExecStart=/usr/bin/apt-get install --only-upgrade --yes peppy'
else
  if "$dry_run"; then printf '+ install -d -m 0755 /etc/pki/rpm-gpg\n'; else install -d -m 0755 /etc/pki/rpm-gpg; fi
  if "$dry_run"; then printf '+ verify key fingerprint %s\n+ install key /etc/pki/rpm-gpg/RPM-GPG-KEY-peppy\n' "$fingerprint"; else install -m 0644 "$key_tmp" /etc/pki/rpm-gpg/RPM-GPG-KEY-peppy; fi
  write_file /etc/yum.repos.d/peppy.repo "[peppy]
name=Peppy $channel
baseurl=${repo_url%/}/rpm/$channel
enabled=1
gpgcheck=1
repo_gpgcheck=1
includepkgs=peppy
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
  service='[Unit]
Description=Update Peppy only from its configured signed repository
Wants=network-online.target
After=network-online.target
[Service]
Type=oneshot
ExecStart=/usr/bin/dnf --assumeyes upgrade peppy'
fi
write_file /etc/systemd/system/peppy-update.service "$service"
write_file /etc/systemd/system/peppy-update.timer '[Unit]
Description=Daily Peppy-only package update check
[Timer]
OnCalendar=daily
Persistent=true
RandomizedDelaySec=30m
[Install]
WantedBy=timers.target'
if "$dry_run"; then printf '+ systemctl daemon-reload\n+ systemctl enable --now peppy-update.timer\n'; else systemctl daemon-reload; systemctl enable --now peppy-update.timer; fi
echo "Peppy-only unattended updates configured for $channel; no reboot is scheduled."
