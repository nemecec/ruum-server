#!/usr/bin/env bash
# What staging is now (read-only).
#
#   deploy/staging/status.sh            resources by label and their monthly price, nodes, pods,
#                                       backups, deployed commit
#   deploy/staging/status.sh --checks   also the smoke checks (ops/smoke.sh) and Zitadel drift
#   deploy/staging/status.sh --netpol   only the network policy probe (ops/netpol-probe.sh; not
#                                       read-only: a temporary namespace and ephemeral containers)
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
# The cluster part needs the admin tunnel; without it the cloud part still reports.
if [[ -r "$STG_TUNNEL_KEY" && -s "$STG_TUNNEL_KNOWN_HOSTS" ]]; then
  stg_tunnel_up
else
  stg_warn "no admin tunnel (ops/tunnel-setup.sh); the cluster part will fail"
fi
if [[ "${1:-}" == --netpol ]]; then
  exec bash "$STG_HERE/tool.sh" bash "$STG_HERE/ops/netpol-probe.sh"
fi
stg_secret_stream "${STG_NAMES_TOFU[@]}" | stg_run ro status "$@"
if [[ "${1:-}" == --checks ]]; then
  bash "$STG_HERE/ops/smoke.sh" || true
fi
