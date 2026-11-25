#!/usr/bin/env bash
# The load balancer of a running staging moves a port only to a NodePort that a Service of the
# edge already has (Kimi re-check of O-53; design 12.7: expand, then contract). Backlog O-53 moved
# port 80 from 30080 to 30081 (the Service ruum-edge-http of charts/ruum). Applied before that
# chart, the load balancer sends port 80 to a NodePort that nothing serves: its health check
# fails on every node, and redirects and ACME HTTP-01 stop until the chart comes. steps.sh apply
# runs `check` after the plan and before `tofu apply`.
#
#   lb-guard.sh ports PLAN   the new destination ports of load balancer services that exist and
#                            whose destination port the plan changes (update or replace), one per
#                            line. A service that the plan creates (a new cluster) is not listed:
#                            nothing serves on it yet, and the chart follows the platform step.
#   lb-guard.sh check PLAN   exit 0 when `ports` is empty (contacts nothing), or when each port is
#                            the nodePort of a Service in the namespace edge (kubectl with
#                            KUBECONFIG); otherwise it names the port and exits 1.
# PLAN is the scrubbed plan JSON of tofu/cluster (plan-scrub.jq keeps these values).
set -euo pipefail

die() { printf 'lb-guard: %s\n' "$*" >&2; exit 1; }
command -v jq >/dev/null || die "jq is required"

ports() {
  [[ -r "${1:-}" ]] || die "no plan JSON '${1:-}'"
  jq -r '[ .resource_changes[]? | select(.mode == "managed" and .type == "hcloud_load_balancer_service")
    | select(.change.before != null and .change.after != null)
    | select(.change.before.destination_port != .change.after.destination_port)
    | .change.after.destination_port ] | unique | .[] | tostring' "$1"
}

case "${1:-}" in
  ports) ports "${2:-}" ;;
  check)
    want="$(ports "${2:-}")"
    [[ -n "$want" ]] || exit 0
    have="$(kubectl -n edge get services -o json --request-timeout=20s | jq -r '[.items[].spec.ports[]?.nodePort // empty] | unique | .[] | tostring')" ||
      die "the plan moves a load balancer port to NodePort $(echo $want), and the Services of the namespace edge cannot be listed (admin tunnel, kubeconfig); nothing was applied"
    for p in $want; do
      grep -qxF "$p" <<<"$have" ||
        die "the plan moves a load balancer port to NodePort $p, but no Service in the namespace edge has it: deploy the chart first (deploy/staging/deploy.sh, after secrets-push.sh platform updated the controller; backlog O-53), then apply again. Nothing was applied."
    done
    echo "lb-guard: the new NodePorts $(echo $want) are served by Services of the namespace edge"
    ;;
  *) die "usage: lb-guard.sh ports|check PLAN" ;;
esac
