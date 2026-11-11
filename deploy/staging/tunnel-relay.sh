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
set -euo pipefail
ports="${STG_TUNNEL_PORTS:?STG_TUNNEL_PORTS is not set}"
[[ $# -gt 0 ]] || { echo "usage: tunnel-relay.sh COMMAND [ARGS...]" >&2; exit 1; }
command -v socat >/dev/null || { echo "tunnel-relay: socat is missing in the tools image (tools-image.sh build)" >&2; exit 1; }
for p in $ports; do
  [[ "$p" =~ ^[0-9]{4,5}$ ]] || { echo "tunnel-relay: bad port '$p'" >&2; exit 1; }
  socat -lf /dev/null "TCP-LISTEN:$p,bind=127.0.0.1,reuseaddr,fork" "TCP:host.docker.internal:$p" &
done
# Wait until every relay listens (state 0A in /proc/net/tcp; no connection is made).
for p in $ports; do
  hex="$(printf ':%04X' "$p")"
  for _ in $(seq 1 50); do
    awk -v h="$hex" '$2 ~ h"$" && $4 == "0A" {f=1} END {exit !f}' /proc/net/tcp && break
    sleep 0.1
  done
done
exec "$@"
