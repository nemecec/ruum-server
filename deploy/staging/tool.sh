#!/usr/bin/env bash
# Runs one command in the tools container on the Mac, with the work folder
# (~/.config/ruum/staging: kubeconfig, talosconfig) and this checkout mounted (lib.sh stg_docker).
# KUBECONFIG and TALOSCONFIG point at the admin files there. No secret from secrets.env goes in.
#
#   deploy/staging/tool.sh kubectl get pods -A
#   deploy/staging/tool.sh talosctl -n <node IP> health
#   deploy/staging/tool.sh bash deploy/staging/ops/upgrade-talos.sh
#   deploy/staging/tool.sh --port 8025 kubectl -n ops port-forward --address 0.0.0.0 svc/mailpit 8025
#                                 (then http://127.0.0.1:8025 on the Mac)
#
# --port N publishes container port N on 127.0.0.1:N of the Mac only.
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

opts=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --port)
      [[ "${2:-}" =~ ^[0-9]{2,5}$ ]] || stg_die "--port needs a port number"
      opts+=(-p "127.0.0.1:$2:$2")
      shift 2
      ;;
    -h | --help) sed -n '2,12p' "$0"; exit 0 ;;
    *) break ;;
  esac
done
[[ $# -gt 0 ]] || stg_die "usage: tool.sh [--port N] COMMAND [ARGS...]"
# A terminal gets a tty (for kubectl exec -it, less); a pipe does not.
[[ -t 0 && -t 1 ]] && opts+=(-t)
stg_docker ${opts[@]+"${opts[@]}"} -- "$@"
