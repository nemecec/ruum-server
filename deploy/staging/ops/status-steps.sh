#!/usr/bin/env bash
# The container half of status.sh (steps.sh status). Environment: WORK, KUBECONFIG_FILE,
# HCLOUD_TOKEN.
#   status-steps.sh [--checks]          the report
#   status-steps.sh --deployed-commit   only the deployed commit (deploy.sh), or nothing
#   status-steps.sh --controller-commit only the deploy controller's commit (deploy.sh), or nothing
#   status-steps.sh --requests          only the release requests (checked values)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
kc=()
if [[ -s "${KUBECONFIG_FILE:-}" ]]; then
  kc=(--kubeconfig "$KUBECONFIG_FILE")
elif [[ -s "$WORK/deployer.kubeconfig" ]]; then
  kc=(--kubeconfig "$WORK/deployer.kubeconfig")
fi
if [[ "${1:-}" == --deployed-commit ]]; then
  [[ ${#kc[@]} -gt 0 ]] && kubectl "${kc[@]}" -n ruum get configmap ruum-release -o jsonpath='{.data.commit}' 2>/dev/null
  exit 0
fi
if [[ "${1:-}" == --controller-commit ]]; then
  # The commit of the chart and the deploy scripts in the controller's image (backlog O-34).
  [[ ${#kc[@]} -gt 0 ]] && kubectl "${kc[@]}" -n ruum-deploy get deployment deploy-controller \
    -o jsonpath='{.metadata.annotations.ruum\.space/commit}' 2>/dev/null
  exit 0
fi
# ruum_printable: drops control characters (ESC included) from stdin.
ruum_printable() { LC_ALL=C tr -d '\000-\010\013-\037\177'; }
# ruum_request_rows KUBECTL_ARGS...: NAME PHASE COMMIT of the release requests, oldest first,
# with every deployer-written value checked.
ruum_request_rows() {
  kubectl "$@" -n ruum-deploy get configmaps -l ruum.space/release-request=true -o json 2>/dev/null |
    jq -r '.items | sort_by(.metadata.creationTimestamp)[]
      | [ (.metadata.name | if test("^[a-z0-9][a-z0-9.-]{0,252}$") then . else "?" end),
          ((.metadata.annotations // {})["ruum.space/phase"] // "" |
            if . == "" then "new" elif IN("Running", "Succeeded", "Failed", "Refused") then . else "?" end),
          (.data.commit // "" | if test("^[0-9a-f]{40}$") then . else "?" end) ] | join(" ")' | ruum_printable
}
if [[ "${1:-}" == --requests ]]; then
  [[ ${#kc[@]} -gt 0 ]] && ruum_request_rows "${kc[@]}"
  exit 0
fi
hapi() {
  printf 'header = "Authorization: Bearer %s"\n' "$HCLOUD_TOKEN" | curl -fsS -K - "https://api.hetzner.cloud/v1/$1"
}
echo "== Hetzner resources with env=staging"
for kind in servers load_balancers primary_ips networks firewalls placement_groups; do
  hapi "$kind?label_selector=env%3Dstaging" | jq -r --arg k "$kind" '.[$k][] | "  \($k): \(.name)\(if .server_type then " (" + .server_type.name + ", " + .status + ")" else "" end)"'
done
hapi "images?type=snapshot&label_selector=env%3Dstaging" | jq -r '.images[] | "  snapshot: \(.description) (\(.image_size // 0) GB)"'
echo "== Orphans: resources without env=staging, except the build server (review 2 C-2)"
for kind in servers load_balancers primary_ips networks firewalls volumes floating_ips; do
  hapi "$kind" | jq -r --arg k "$kind" '.[$k][] | select((.labels.env // "") != "staging") | select((.name // "") | test("buildbox|^primary_ip-160842") | not) | "  ORPHAN \($k): \(.name)"'
done
echo "== Monthly price of what exists"
bash "$HERE/cost-report.sh" 2>/dev/null || echo "  (cost-report failed)"
if [[ ${#kc[@]} -eq 0 ]]; then
  echo "== Cluster: no kubeconfig in $WORK (staging not created yet)"
  exit 0
fi
echo "== Nodes"
kubectl "${kc[@]}" get nodes -o wide 2>&1 | sed 's/^/  /'
echo "== Pods not Running/Completed"
kubectl "${kc[@]}" get pods -A --no-headers 2>&1 | awk '$4 != "Running" && $4 != "Completed"' | sed 's/^/  /'
echo "== Services that are not ClusterIP (only the edge NodePort is expected)"
kubectl "${kc[@]}" get svc -A --no-headers 2>&1 | awk '$3 != "ClusterIP"' | sed 's/^/  /'
echo "== CNPG"
kubectl "${kc[@]}" -n data get cluster pg -o jsonpath='  phase: {.status.phase}; primary: {.status.currentPrimary}; last backup: {.status.lastSuccessfulBackup}{"\n"}' 2>&1
kubectl "${kc[@]}" -n data get backups --sort-by=.metadata.creationTimestamp 2>&1 | tail -3 | sed 's/^/  /'
echo "== Deployed commit"
kubectl "${kc[@]}" -n ruum get configmap ruum-release -o jsonpath='  {.data.commit}{"\n"}' 2>&1
echo "== Deploy controller (backlog O-34) and the last release requests"
kubectl "${kc[@]}" -n ruum-deploy get pods -l app=deploy-controller --no-headers 2>&1 | sed 's/^/  /'
kubectl "${kc[@]}" -n ruum-deploy get deployment deploy-controller \
  -o jsonpath='  controller commit: {.metadata.annotations.ruum\.space/commit}{"\n"}' 2>/dev/null | ruum_printable
# The deployer writes the requests (their commit and, before the controller takes them, any
# annotation), so only checked values reach this terminal: a commit of 40 hex, one of the four
# phases, a name of DNS characters; anything else shows as "?" (no escape sequence gets through;
# review of O-34, change 2).
{ echo "NAME PHASE COMMIT"; ruum_request_rows "${kc[@]}" | tail -5; } | sed 's/^/  /'
if [[ "${1:-}" == --checks ]]; then
  echo "== Secrets that must not be in the cluster (stgsec 5.8)"
  if kubectl "${kc[@]}" get secrets -A -o json | jq -r '.items[].data // {} | keys[]' | grep -qiE 'HCLOUD|STRIPE'; then
    echo "  FAIL: a secret key named HCLOUD* or STRIPE* exists"
  else
    echo "  PASS: no HCLOUD or Stripe key in any Secret"
  fi
  echo "== Zitadel drift"
  bash "$HERE/zitadel-bootstrap.sh" check 2>&1 | sed 's/^/  /'
fi
