#!/usr/bin/env bash
# Runs in the tools container (lib.sh stg_docker starts it while the admin tunnel is up):
#
#   bash tunnel-relay.sh COMMAND [ARGS...]
#
# For each port of STG_TUNNEL_PORTS it listens on 127.0.0.1 of the container and relays to the
# same port on the Mac (host.docker.internal, Docker Desktop), where the SSH local forward of
# lib.sh listens on 127.0.0.1. So kubectl, talosctl, helm and OpenTofu use the same
# https://127.0.0.1:16443 and 127.0.0.1:1500N as the Mac. Then it runs COMMAND. The relays go
# with the container (docker run --rm --init).
# COMMAND runs only when every relay is running and listens (Kimi review F6): a port that is
# taken already, a relay that exits, or a relay that does not listen within 5 seconds ends the
# script with exit 1, and COMMAND does not run.
set -euo pipefail
ports="${STG_TUNNEL_PORTS:?STG_TUNNEL_PORTS is not set}"
PROC_TCP=/proc/net/tcp
die() { echo "tunnel-relay: $*" >&2; exit 1; }
[[ $# -gt 0 ]] || { echo "usage: tunnel-relay.sh COMMAND [ARGS...]" >&2; exit 1; }
command -v socat >/dev/null || die "socat is missing in the tools image (tools-image.sh build)"
[[ -r "$PROC_TCP" ]] || die "no $PROC_TCP (the listener check needs it)"
for p in $ports; do
  [[ "$p" =~ ^[0-9]{4,5}$ ]] || die "bad port '$p'"
done

# listening PORT: true when a socket listens on PORT (state 0A in /proc/net/tcp; no connection
# is made).
listening() {
  local hex
  hex="$(printf ':%04X' "$1")"
  awk -v h="$hex" '$2 ~ h"$" && $4 == "0A" {f=1} END {exit !f}' "$PROC_TCP"
}

# A port that listens before the relays start is not ours: the check below would pass for it.
for p in $ports; do
  ! listening "$p" || die "port $p listens already in the container"
done

pids=()
for p in $ports; do
  socat -lf /dev/null "TCP-LISTEN:$p,bind=127.0.0.1,reuseaddr,fork" "TCP:host.docker.internal:$p" &
  pids+=("$!")
done

# alive: true when every relay still runs.
alive() {
  local pid
  for pid in "${pids[@]}"; do kill -0 "$pid" 2>/dev/null || return 1; done
}

# Wait until every relay listens (up to 5 seconds in all).
for p in $ports; do
  for _ in $(seq 1 50); do
    alive || die "a relay exited (port $p or another one); the command does not run"
    listening "$p" && break
    sleep 0.1
  done
done
# The final check: every relay runs and every port listens.
sleep 0.2
alive || die "a relay exited; the command does not run"
for p in $ports; do
  listening "$p" || die "no relay listens on 127.0.0.1:$p; the command does not run"
done
exec "$@"
