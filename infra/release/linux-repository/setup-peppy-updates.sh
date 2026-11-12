#!/usr/bin/env bash
set -euo pipefail

# Administrator-operated opt-in only. --dry-run is a fixture mode: it does not
# download keys or modify host configuration.
usage() { echo "usage: $0 --distro <debian|fedora> --repo-url https://... --key-url https://... --key-fingerprint <40-hex> --channel <stable|staging> [--rpm-key-url https://... --rpm-key-fingerprint <40-hex>] [--dry-run]" >&2; exit 2; }
fail() { echo "$*" >&2; exit 1; }
valid_url() { [[ "$1" =~ ^https://[A-Za-z0-9.-]+(:[0-9]+)?(/[A-Za-z0-9._~:/@!\&\(\)*+,;=%-]*)?$ ]]; }
url_authority() { printf '%s\n' "${1#https://}" | sed 's#/.*##'; }
url_hostname() { url_authority "$1" | sed 's/:[0-9][0-9]*$//'; }
write_file() {
  local path=$1 content=$2
  if "$dry_run"; then
    printf '+ write %s\n%s\n' "$path" "$content"
  else
    if [ -e "$path" ] || [ -L "$path" ]; then
      fail "refusing to replace existing $path"
    fi
    printf '%s\n' "$content" > "$path"
  fi
}

distro=''; repo_url=''; key_url=''; fingerprint=''; rpm_key_url=''; rpm_fingerprint=''; rpm_key_url_supplied=false; rpm_fingerprint_supplied=false; channel=''; dry_run=false
while [ "$#" -gt 0 ]; do case "$1" in --distro) distro=${2:-}; shift 2;; --repo-url) repo_url=${2:-}; shift 2;; --key-url) key_url=${2:-}; shift 2;; --key-fingerprint) fingerprint=${2:-}; shift 2;; --rpm-key-url) rpm_key_url_supplied=true; if [ "$#" -lt 2 ] || [[ "$2" == --* ]]; then usage; fi; rpm_key_url=$2; shift 2;; --rpm-key-fingerprint) rpm_fingerprint_supplied=true; if [ "$#" -lt 2 ] || [[ "$2" == --* ]]; then usage; fi; rpm_fingerprint=$2; shift 2;; --channel) channel=${2:-}; shift 2;; --dry-run) dry_run=true; shift;; *) usage;; esac; done
case "$distro" in debian|fedora) ;; *) usage;; esac
case "$channel" in stable|staging) ;; *) usage;; esac
if "$rpm_key_url_supplied" || "$rpm_fingerprint_supplied"; then
  [ "$distro" = fedora ] || fail 'RPM key options are supported only for Fedora'
  if ! "$rpm_key_url_supplied" || ! "$rpm_fingerprint_supplied"; then fail 'RPM key URL and fingerprint must be supplied together'; fi
  if [ -z "$rpm_key_url" ] || [ -z "$rpm_fingerprint" ]; then fail 'RPM key URL and fingerprint must not be empty'; fi
fi
if ! valid_url "$repo_url" || ! valid_url "$key_url"; then
  echo 'repository and key URLs must be strict HTTPS URLs' >&2
  exit 2
fi
[ "$(url_authority "$repo_url")" = "$(url_authority "$key_url")" ] || fail 'repository and key URLs must use the same HTTPS authority'
fingerprint=$(printf '%s' "$fingerprint" | tr '[:lower:]' '[:upper:]'); [[ "$fingerprint" =~ ^[0-9A-F]{40}$ ]] || { echo 'key fingerprint must be exactly 40 hexadecimal characters' >&2; exit 2; }
if "$rpm_key_url_supplied"; then
  valid_url "$rpm_key_url" || { echo 'RPM key URL must be a strict HTTPS URL' >&2; exit 2; }
  [ "$(url_authority "$repo_url")" = "$(url_authority "$rpm_key_url")" ] || fail 'repository and RPM key URLs must use the same HTTPS authority'
  rpm_fingerprint=$(printf '%s' "$rpm_fingerprint" | tr '[:lower:]' '[:upper:]'); [[ "$rpm_fingerprint" =~ ^[0-9A-F]{40}$ ]] || { echo 'RPM key fingerprint must be exactly 40 hexadecimal characters' >&2; exit 2; }
fi
[ "$(id -u)" = 0 ] || "$dry_run" || fail 'run this explicit administrator setup with root privileges'

key_tmp=$(mktemp); rpm_key_tmp=''; trap 'rm -f "$key_tmp" "$rpm_key_tmp"' EXIT
if ! "$dry_run"; then
  curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 "$key_url" -o "$key_tmp"
  read -r primary_count actual < <(gpg --show-keys --with-colons "$key_tmp" | awk -F: '$1 == "pub" { want_fpr=1; next } $1 == "fpr" && want_fpr { primaries++; primary=toupper($10); want_fpr=0 } END { print primaries, primary }')
  if [ "$primary_count" != 1 ] || [ "$actual" != "$fingerprint" ]; then
    fail 'downloaded repository key must contain exactly the pinned primary fingerprint'
  fi
  if "$rpm_key_url_supplied"; then
    rpm_key_tmp=$(mktemp)
    curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 "$rpm_key_url" -o "$rpm_key_tmp"
    read -r primary_count actual < <(gpg --show-keys --with-colons "$rpm_key_tmp" | awk -F: '$1 == "pub" { want_fpr=1; next } $1 == "fpr" && want_fpr { primaries++; primary=toupper($10); want_fpr=0 } END { print primaries, primary }')
    if [ "$primary_count" != 1 ] || [ "$actual" != "$rpm_fingerprint" ]; then
      fail 'downloaded RPM key must contain exactly the pinned primary fingerprint'
    fi
  fi
fi

if [ "$distro" = debian ]; then
  if ! "$dry_run"; then
    for destination in /etc/apt/keyrings/peppy.asc /etc/apt/sources.list.d/peppy.list /etc/apt/preferences.d/peppy /etc/systemd/system/peppy-update.service /etc/systemd/system/peppy-update.timer; do
      if [ -e "$destination" ] || [ -L "$destination" ]; then
        fail "refusing to replace existing $destination"
      fi
    done
  fi
  if "$dry_run"; then printf '+ install -d -m 0755 /etc/apt/keyrings\n'; else install -d -m 0755 /etc/apt/keyrings; fi
  if "$dry_run"; then printf '+ verify key fingerprint %s\n+ install key /etc/apt/keyrings/peppy.asc\n' "$fingerprint"; else install -m 0644 "$key_tmp" /etc/apt/keyrings/peppy.asc; fi
  write_file /etc/apt/sources.list.d/peppy.list "deb [arch=amd64 signed-by=/etc/apt/keyrings/peppy.asc] ${repo_url%/}/apt $channel main"
  write_file /etc/apt/preferences.d/peppy "Package: *
Pin: origin $(url_hostname "$repo_url")
Pin-Priority: -1

Package: peppy
Pin: origin $(url_hostname "$repo_url")
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
  if ! "$dry_run"; then
    destinations=(/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy /etc/yum.repos.d/peppy.repo /etc/systemd/system/peppy-update.service /etc/systemd/system/peppy-update.timer)
    "$rpm_key_url_supplied" && destinations+=(/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm)
    for destination in "${destinations[@]}"; do
      if [ -e "$destination" ] || [ -L "$destination" ]; then
        fail "refusing to replace existing $destination"
      fi
    done
  fi
  if "$dry_run"; then printf '+ install -d -m 0755 /etc/pki/rpm-gpg\n'; else install -d -m 0755 /etc/pki/rpm-gpg; fi
  if "$dry_run"; then printf '+ verify key fingerprint %s\n+ install key /etc/pki/rpm-gpg/RPM-GPG-KEY-peppy\n' "$fingerprint"; else install -m 0644 "$key_tmp" /etc/pki/rpm-gpg/RPM-GPG-KEY-peppy; fi
  rpm_gpgkey=''
  if "$rpm_key_url_supplied"; then
    if "$dry_run"; then printf '+ verify RPM key fingerprint %s\n+ install key /etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm\n' "$rpm_fingerprint"; else install -m 0644 "$rpm_key_tmp" /etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm; fi
    rpm_gpgkey=' file:///etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm'
  fi
  write_file /etc/yum.repos.d/peppy.repo "[peppy]
name=Peppy $channel
baseurl=${repo_url%/}/rpm/$channel
enabled=1
gpgcheck=1
repo_gpgcheck=1
includepkgs=peppy
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-peppy${rpm_gpgkey}"
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
