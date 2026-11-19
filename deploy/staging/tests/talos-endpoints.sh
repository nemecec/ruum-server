#!/usr/bin/env bash
# The Talos API endpoints of tofu/cluster (backlog O-21): the variable talos_endpoints and the
# locals of talos.tf, evaluated by OpenTofu in a temporary root with no provider and local state
# (documentation addresses only). Needs tofu (the tools image, or CI after setup.sh tofu). Creates
# nothing; the temp folder is removed on exit.
#
#   bash deploy/staging/tests/talos-endpoints.sh
set -euo pipefail
STG="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
failed=0
ok() { printf 'ok   %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; failed=1; }

{
  sed -n '/^variable "talos_endpoints"/,/^}/p' "$STG/tofu/cluster/variables.tf"
  # The locals block after the O-21 marker in talos.tf, exactly as it is there.
  sed -n '/^# --- Talos API endpoints (backlog O-21) ---$/,/^}/p' "$STG/tofu/cluster/talos.tf" | sed -n '/^locals {/,/^}/p'
  cat <<'TF'
variable "node_count" { default = 3 }
locals {
  nodes = { for i in range(1, var.node_count + 1) : "stg-${i}" => { index = i } }
}
output "apply" { value = local.talos_endpoint }
output "bootstrap" { value = local.bootstrap_endpoint }
TF
} >"$T/main.tf"
grep -q 'tunnel_endpoint' "$T/main.tf" && grep -q 'variable "talos_endpoints"' "$T/main.tf" ||
  { echo "no talos_endpoints variable or endpoint locals in tofu/cluster" >&2; exit 1; }

cd "$T"
tofu init -input=false -no-color >init.log 2>&1 || { cat init.log >&2; exit 1; }

# run VAR_JSON [NODE_COUNT]: apply the root and print "apply-json bootstrap".
run() {
  rm -f terraform.tfstate
  TF_VAR_talos_endpoints="$1" tofu apply -auto-approve -input=false -no-color -var "node_count=${2:-3}" >apply.log 2>&1 || return 1
  printf '%s %s' "$(tofu output -json apply | jq -cS .)" "$(tofu output -raw bootstrap)"
}

got="$(run '{}')"
[[ "$got" == '{"stg-1":"127.0.0.1:15001","stg-2":"127.0.0.1:15002","stg-3":"127.0.0.1:15003"} 127.0.0.1:15001' ]] &&
  ok "no kept endpoint: every Talos resource dials its node through the admin tunnel (127.0.0.1:1500N)" || bad "tunnel endpoints: $got"
got="$(run '{}' 4)"
[[ "$got" == *'"stg-4":"127.0.0.1:15004"'* ]] && ok "a fourth node uses 127.0.0.1:15004" || bad "stg-4 endpoint: $got"
got="$(run '{"stg-1":"203.0.113.1","stg-3":"203.0.113.3","bootstrap":"203.0.113.1"}')"
[[ "$got" == '{"stg-1":"203.0.113.1","stg-2":"127.0.0.1:15002","stg-3":"203.0.113.3"} 203.0.113.1' ]] &&
  ok "endpoints kept from the state stay; the others use the tunnel" || bad "kept endpoints: $got"
got="$(run '{"stg-2":"127.0.0.1:15002"}')"
[[ "$got" == '{"stg-1":"127.0.0.1:15001","stg-2":"127.0.0.1:15002","stg-3":"127.0.0.1:15003"} 127.0.0.1:15001' ]] &&
  ok "a kept tunnel endpoint stays the tunnel" || bad "kept tunnel endpoint: $got"
for v in '{"stg-1":"evil.example:50000"}' '{"stg-1":"127.0.0.1:16443"}' '{"stg-1":"127.0.0.1:15005"}' '{"stg-5":"127.0.0.1:15001"}' \
  '{"web":"203.0.113.1"}' '{"stg-1":"203.0.113.1:50000"}' '{"bootstrap":"0.0.0.0/0"}'; do
  if run "$v" >/dev/null; then bad "talos_endpoints accepts $v"; else
    grep -q 'talos_endpoints' apply.log && ok "talos_endpoints refuses $v" || { bad "talos_endpoints refusal message for $v"; tail -5 apply.log; }
  fi
done
exit "$failed"
