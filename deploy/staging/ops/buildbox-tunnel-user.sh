#!/usr/bin/env bash
# Sets up the tunnel user `ruumtunnel` on the build server (decision of the person, 2026-11-11).
# It runs ON THE BUILD SERVER, as root, started from the Mac by the admin user (ops/tunnel-setup.sh
# does exactly this; the node addresses come from the Hetzner API):
#
#   ssh -i ~/.config/ruum/buildbox_admin_ed25519 ruumadmin@<build server> \
#     "sudo bash -s -- '<public key>' <node IPv4> <node IPv4> <node IPv4>" < deploy/staging/ops/buildbox-tunnel-user.sh
#
# The Mac reaches the staging Kubernetes API (6443) and Talos API (50000) through an SSH local
# forward as this user. The build server only relays the TLS and mTLS bytes: the kubeconfig and
# the talosconfig stay on the Mac. What it sets up (idempotent; it changes only what differs):
#   - the user ruumtunnel: no password, shell /usr/sbin/nologin, no home;
#   - /etc/ssh/authorized_keys/ruumtunnel (root, 0644; the user cannot change it), one line:
#       restrict,port-forwarding,permitopen="<ip>:6443",permitopen="<ip>:50000",...,command="/bin/false" <key>
#   - /etc/ssh/sshd_config.d/10-ruumtunnel.conf: TCP forwarding off for every user but
#     ruumtunnel; for ruumtunnel only local forwards to the node ports, no X11, no agent, no tty,
#     no stream-local or remote forward, ForceCommand /bin/false, public key only;
#   - sshd -t, the effective settings checked with sshd -T, then a reload of sshd. Until all of
#     that succeeds, an EXIT trap puts the previous key file and drop-in back (or removes them
#     when they were not there), so a failed check never leaves the new config in place.
# At the end it prints the host key of the build server ("hostkey ssh-ed25519 ..."), which
# tunnel-setup.sh records on the Mac (the admin connection checked it already).
#
# Tests (no root, nothing changed):
#   buildbox-tunnel-user.sh --print-authorized-keys '<public key>' <node IPv4>...
#   buildbox-tunnel-user.sh --print-sshd-config <node IPv4>...
set -euo pipefail

USER_NAME=ruumtunnel
KEYS_DIR=/etc/ssh/authorized_keys
KEYS_FILE="$KEYS_DIR/$USER_NAME"
DROPIN=/etc/ssh/sshd_config.d/10-ruumtunnel.conf
SSHD=/usr/sbin/sshd
SSHD_CONFIG=/etc/ssh/sshd_config
HOSTKEY_PUB=/etc/ssh/ssh_host_ed25519_key.pub
KEY_RE='^ssh-ed25519 [A-Za-z0-9+/]{68}( [A-Za-z0-9@._-]{1,64})?$'

die() { printf 'buildbox-tunnel-user: %s\n' "$*" >&2; exit 1; }
log() { printf '==> %s\n' "$*" >&2; }

# A public unicast IPv4 address (same rules as deploy/staging/tunnel.sh ipv4_public).
ipv4_public() {
  local a b c o octets
  [[ "$1" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  octets=("${BASH_REMATCH[@]:1}")
  for o in "${octets[@]}"; do
    [[ "$o" == 0 || "${o:0:1}" != 0 ]] || return 1
    ((10#$o <= 255)) || return 1
  done
  a=${octets[0]} b=${octets[1]} c=${octets[2]}
  ((a == 0 || a == 10 || a == 127 || a >= 224)) && return 1
  ((a == 100 && b >= 64 && b <= 127)) && return 1
  ((a == 169 && b == 254)) && return 1
  ((a == 172 && b >= 16 && b <= 31)) && return 1
  ((a == 192 && b == 168)) && return 1
  ((a == 192 && b == 0 && (c == 0 || c == 2))) && return 1
  ((a == 198 && (b == 18 || b == 19))) && return 1
  ((a == 198 && b == 51 && c == 100)) && return 1
  ((a == 203 && b == 0 && c == 113)) && return 1
  return 0
}

check_nodes() {
  local ip seen=" "
  [[ $# -ge 1 && $# -le 4 ]] || die "give 1 to 4 node addresses (the staging nodes' public IPv4)"
  for ip in "$@"; do
    ipv4_public "$ip" || die "'$ip' is not a public IPv4 address"
    [[ "$seen" != *" $ip "* ]] || die "$ip is given twice"
    seen="$seen$ip "
  done
}

check_key() {
  [[ "$1" =~ $KEY_RE ]] || die "the public key must be one ssh-ed25519 key line"
}

authorized_keys_line() {
  local key="$1" ip opts="restrict,port-forwarding"
  shift
  for ip in "$@"; do opts="$opts,permitopen=\"$ip:6443\",permitopen=\"$ip:50000\""; done
  printf '%s,command="/bin/false" %s\n' "$opts" "$key"
}

permit_open_list() {
  local ip out=""
  for ip in "$@"; do out="$out $ip:6443 $ip:50000"; done
  printf '%s' "${out# }"
}

sshd_dropin() {
  cat <<EOF
# Managed by deploy/staging/ops/buildbox-tunnel-user.sh (ruum staging admin tunnel). Do not edit.
# TCP forwarding only for $USER_NAME, and only to the staging nodes' Kubernetes and Talos APIs.
AllowTcpForwarding no
AllowStreamLocalForwarding no

Match User $USER_NAME
    AuthorizedKeysFile $KEYS_FILE
    AuthenticationMethods publickey
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    AllowTcpForwarding local
    PermitOpen $(permit_open_list "$@")
    PermitListen none
    GatewayPorts no
    AllowStreamLocalForwarding no
    PermitTunnel no
    X11Forwarding no
    AllowAgentForwarding no
    PermitTTY no
    PermitUserRC no
    ForceCommand /bin/false
Match all
EOF
}

case "${1:-}" in
  --print-authorized-keys)
    shift
    key="${1:-}"
    shift || true
    check_key "$key"
    check_nodes "$@"
    authorized_keys_line "$key" "$@"
    exit 0
    ;;
  --print-sshd-config)
    shift
    check_nodes "$@"
    sshd_dropin "$@"
    exit 0
    ;;
esac

key="${1:-}"
shift || true
check_key "$key"
check_nodes "$@"
[[ "$(id -u)" == 0 ]] || die "run as root (sudo bash -s -- ...)"
[[ -x "$SSHD" ]] || die "no $SSHD"
[[ -x /usr/sbin/nologin && -x /bin/false ]] || die "no /usr/sbin/nologin or /bin/false"
grep -qE '^[[:space:]]*Include[[:space:]]+/etc/ssh/sshd_config\.d/\*\.conf' "$SSHD_CONFIG" ||
  die "$SSHD_CONFIG does not include /etc/ssh/sshd_config.d/*.conf"

changed=0
# write_file PATH MODE CONTENT: replace the file only when the content differs (atomic, root-owned).
write_file() {
  local path="$1" mode="$2" content="$3" tmp
  if [[ -f "$path" ]] && [[ "$(cat "$path")" == "$content" ]] &&
    [[ "$(stat -c '%U:%G %a' "$path")" == "root:root $mode" ]]; then
    return 0
  fi
  tmp="$(mktemp "$path.XXXXXX")"
  printf '%s\n' "$content" >"$tmp"
  chown root:root "$tmp"
  chmod "$mode" "$tmp"
  mv -f "$tmp" "$path"
  changed=1
  log "wrote $path"
}

# 1. The user: no password (locked for passwords, "*" so that sshd still takes the key), no shell.
if ! id "$USER_NAME" >/dev/null 2>&1; then
  useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin \
    --comment "ruum staging admin tunnel" "$USER_NAME"
  log "created user $USER_NAME"
fi
usermod --shell /usr/sbin/nologin --home /nonexistent "$USER_NAME"
usermod --password '*' "$USER_NAME"
for g in $(id -nG "$USER_NAME"); do
  [[ "$g" == "$USER_NAME" ]] || die "$USER_NAME is in group $g; it must be in its own group only"
done

# The rollback (Kimi review F5): a copy of the key file and the drop-in as they are now. Until
# every check and the reload succeed, the EXIT trap puts them back (or removes a file that was
# not there), so a failed check never leaves the new config for a later reload or restart.
rollback_dir="$(mktemp -d)"
committed=0
snapshot() { # PATH NAME
  if [[ -e "$1" ]]; then cp -p "$1" "$rollback_dir/$2"; else : >"$rollback_dir/$2.absent"; fi
}
restore() { # PATH NAME
  if [[ -e "$rollback_dir/$2.absent" ]]; then
    rm -f "$1"
  elif [[ -e "$rollback_dir/$2" ]]; then
    cp -p "$rollback_dir/$2" "$1.rollback" && mv -f "$1.rollback" "$1"
  fi
}
rollback() {
  local rc=$?
  if [[ $committed != 1 ]]; then
    restore "$KEYS_FILE" keys
    restore "$DROPIN" dropin
    printf 'buildbox-tunnel-user: failed; the previous key file and sshd drop-in are back, sshd was not reloaded with the new config\n' >&2
    [[ $rc != 0 ]] || rc=1
  fi
  rm -rf "$rollback_dir"
  exit "$rc"
}
trap rollback EXIT
install -d -o root -g root -m 0755 "$KEYS_DIR"
snapshot "$KEYS_FILE" keys
snapshot "$DROPIN" dropin

# 2. The restricted key, root-owned (the user cannot change it).
write_file "$KEYS_FILE" 644 "$(authorized_keys_line "$key" "$@")"

# 3. sshd: forwarding only for ruumtunnel, only to the node ports.
write_file "$DROPIN" 644 "$(sshd_dropin "$@")"
"$SSHD" -t || die "sshd -t failed"

# 4. The effective settings (sshd -T for a connection of each user).
eff() { "$SSHD" -T -C "user=$1,host=staging-check,addr=192.0.2.1" 2>/dev/null; }
t="$(eff "$USER_NAME")"
expect() { grep -qixF "$1" <<<"$t" || die "effective sshd setting for $USER_NAME is not '$1'"; }
expect "allowtcpforwarding local"
expect "permitopen $(permit_open_list "$@")"
expect "x11forwarding no"
expect "allowagentforwarding no"
expect "permittty no"
expect "permitlisten none"
expect "allowstreamlocalforwarding no"
expect "permittunnel no"
expect "forcecommand /bin/false"
expect "authorizedkeysfile $KEYS_FILE"
grep -qiE '^(allowusers|allowgroups|denyusers|denygroups) ' <<<"$t" &&
  log "note: sshd has AllowUsers/AllowGroups/DenyUsers/DenyGroups; check that $USER_NAME may log in"
eff root | grep -qix 'allowtcpforwarding no' || die "TCP forwarding is still on for other users"

# 5. Reload only when something changed. A failed reload rolls back too (set -e, the EXIT trap).
if [[ $changed == 1 ]]; then
  if systemctl is-active --quiet ssh 2>/dev/null; then
    systemctl reload ssh
    log "reloaded ssh"
  elif systemctl is-active --quiet sshd 2>/dev/null; then
    systemctl reload sshd
    log "reloaded sshd"
  else
    log "sshd is not running as a service; the new config applies at its next start"
  fi
else
  log "no change"
fi
committed=1

hostkey="$(cut -d' ' -f1,2 "$HOSTKEY_PUB")"
[[ "$hostkey" =~ ^ssh-ed25519\ [A-Za-z0-9+/]{68}$ ]] || die "no ed25519 host key"
printf 'hostkey %s\n' "$hostkey"
