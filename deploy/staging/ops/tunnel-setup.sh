#!/usr/bin/env bash
# Sets up the admin tunnel of staging, on the Mac (decision of the person, 2026-11-11):
#
#   deploy/staging/ops/tunnel-setup.sh             key (only if missing), build-server user, host key, test
#   deploy/staging/ops/tunnel-setup.sh --key-only  only make the key when it is missing
#
# 1. Makes the tunnel key ~/.config/ruum/staging_tunnel_ed25519 when it is missing (ed25519, no
#    passphrase, 0600). The private key is never printed and never leaves the Mac.
# 2. Reads the build server and the staging nodes' public IPv4 from the Hetzner API
#    (steps.sh tunnel-targets, in the tools container).
# 3. Runs ops/buildbox-tunnel-user.sh on the build server as the admin user:
#      ssh -i ~/.config/ruum/buildbox_admin_ed25519 ruumadmin@<build server> \
#        "sudo bash -s -- '<public key>' <node IPv4>..." < ops/buildbox-tunnel-user.sh
#    (the admin connection checks the host key against ~/.ssh/known_hosts).
# 4. Records the build server's ed25519 host key, which that script prints, in
#    ~/.config/ruum/staging_tunnel_known_hosts (the tunnel trusts only this file).
# 5. Opens and closes the tunnel once (lib.sh stg_tunnel_up).
# Run it again after the node addresses change (a recreate): the key line lists them.
set -euo pipefail
# shellcheck source=../lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib.sh"

ADMIN_KEY="$HOME/.config/ruum/buildbox_admin_ed25519"
ADMIN_USER=ruumadmin

key_only=0
case "${1:-}" in
  --key-only) key_only=1 ;;
  "") ;;
  -h | --help) sed -n '2,19p' "$0"; exit 0 ;;
  *) stg_die "unknown argument $1" ;;
esac

if [[ ! -e "$STG_TUNNEL_KEY" ]]; then
  mkdir -p "$(dirname "$STG_TUNNEL_KEY")"
  (umask 077 && ssh-keygen -q -t ed25519 -a 100 -N '' -C ruum-staging-tunnel -f "$STG_TUNNEL_KEY" </dev/null >/dev/null)
  stg_log "made the tunnel key $STG_TUNNEL_KEY (0600)"
fi
chmod 600 "$STG_TUNNEL_KEY"
[[ -r "$STG_TUNNEL_KEY.pub" ]] || stg_die "no public key $STG_TUNNEL_KEY.pub"
pub="$(cut -d' ' -f1,2 "$STG_TUNNEL_KEY.pub") ruum-staging-tunnel"
[[ "$pub" =~ ^ssh-ed25519\ [A-Za-z0-9+/]{68}\ ruum-staging-tunnel$ ]] || stg_die "$STG_TUNNEL_KEY.pub is not an ed25519 public key"
[[ $key_only == 0 ]] || { stg_log "tunnel key: $STG_TUNNEL_KEY (public key in $STG_TUNNEL_KEY.pub)"; exit 0; }

[[ -r "$ADMIN_KEY" ]] || stg_die "the build server admin key $ADMIN_KEY is missing"
targets="$(stg_tunnel_targets)" || stg_die "cannot read the tunnel targets from the Hetzner API"
tunnel_parse "$targets" >&2 || stg_die "unexpected tunnel targets"
ips=""
for ((i = 0; i < ${#TUNNEL_PUBLIC[@]}; i++)); do ips="$ips ${TUNNEL_PUBLIC[i]}"; done

stg_log "build server $TUNNEL_BASTION: user $TUNNEL_USER, forwards only to$ips (ports 6443, 50000)"
out="$(ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -i "$ADMIN_KEY" \
  -o StrictHostKeyChecking=yes -o ForwardAgent=no -o ClearAllForwardings=yes \
  "$ADMIN_USER@$TUNNEL_BASTION" "sudo bash -s -- '$pub'$ips" <"$STG_HERE/ops/buildbox-tunnel-user.sh")" ||
  stg_die "the setup on the build server failed (nothing is recorded on the Mac)"
hostkey="$(sed -n 's/^hostkey \(ssh-ed25519 [A-Za-z0-9+/]\{68\}\)$/\1/p' <<<"$out" | tail -n 1)"
[[ -n "$hostkey" ]] || stg_die "the build server did not print its ed25519 host key"
(umask 077 && printf '%s %s\n' "$TUNNEL_BASTION" "$hostkey" >"$STG_TUNNEL_KNOWN_HOSTS")
stg_log "recorded the host key of $TUNNEL_BASTION in $STG_TUNNEL_KNOWN_HOSTS"

stg_tunnel_up
stg_log "the admin tunnel opens (ports $STG_TUNNEL_PORTS on 127.0.0.1); closing it"
stg_audit tunnel-setup ok
