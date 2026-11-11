#!/usr/bin/env bash
# Renders the machine config of every staging node with the pinned Talos provider and the real
# data source of tofu/cluster/talos.tf (local values only: documentation addresses, throwaway
# secrets), then runs `talosctl validate --mode cloud --strict` on each. Needs tofu, talosctl
# and jq (the tools image or CI after setup.sh tofu talosctl). Creates no cloud resource: the
# temporary root has only the talos provider and local state, in a temp folder that is removed
# on exit. Never prints a rendered config (it holds the throwaway cluster secrets).
#
#   bash deploy/staging/tests/talos-validate.sh
set -euo pipefail
STG="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../versions.env
. "$STG/versions.env"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
provider="$(sed -n '/siderolabs\/talos"/{n;s/.*version *= *"\([^"]*\)".*/\1/p;}' "$STG/tofu/cluster/versions.tf")"
[[ -n "$provider" ]] || { echo "no talos provider version in tofu/cluster/versions.tf" >&2; exit 1; }

cat >"$T/main.tf" <<TF
terraform {
  required_providers {
    talos = { source = "siderolabs/talos", version = "$provider" }
  }
}
variable "talos_version" { default = "$TALOS_VERSION" }
variable "kubernetes_version" { default = "$KUBERNETES_VERSION" }
variable "talos_installer_image" { default = "$TALOS_INSTALLER_IMAGE" }
locals {
  path_module      = "$STG/tofu/cluster"
  nodes            = { for i in range($NODE_COUNT) : "stg-\${i + 1}" => { private_ip = "10.20.1.\${11 + i}" } }
  public_v4        = { for i in range($NODE_COUNT) : "stg-\${i + 1}" => "203.0.113.\${1 + i}" }
  cluster_name     = "ruum-staging"
  cluster_endpoint = "https://10.20.1.11:6443"
}
resource "talos_machine_secrets" "this" { talos_version = var.talos_version }
output "mc" {
  value     = { for k, v in data.talos_machine_configuration.node : k => v.machine_configuration }
  sensitive = true
}
TF
# The data source exactly as in talos.tf, with path.module pointing at the real root.
sed -n '/^data "talos_machine_configuration" "node"/,/^}/p' "$STG/tofu/cluster/talos.tf" |
  sed 's/\${path.module}/${local.path_module}/g' >>"$T/main.tf"
grep -q 'data "talos_machine_configuration"' "$T/main.tf" || { echo "no talos_machine_configuration in talos.tf" >&2; exit 1; }

cd "$T"
tofu init -input=false -no-color >init.log 2>&1 || { cat init.log >&2; exit 1; }
tofu apply -auto-approve -input=false -no-color >apply.log 2>&1 || { grep -E '^(Error|│ Error)' -A5 apply.log >&2; exit 1; }
tofu output -json mc >mc.json

failed=0
for n in $(jq -r 'keys[]' mc.json); do
  jq -r --arg n "$n" '.[$n]' mc.json >"$n.yaml"
  if talosctl validate --mode cloud --strict --config "$n.yaml" >"$n.log" 2>&1; then
    echo "ok   $n: talosctl validate --mode cloud"
  else
    echo "FAIL $n: talosctl validate --mode cloud"; sed 's/^/     /' "$n.log"; failed=1
  fi
  # The settings of the plan reviews, in the v1.14 documents (checked without printing them).
  need() { grep -qE "$1" "$n.yaml" && echo "ok   $n: $2" || { echo "FAIL $n: $2"; failed=1; }; }
  never() { grep -qE "$1" "$n.yaml" && { echo "FAIL $n: $2"; failed=1; } || echo "ok   $n: $2"; }
  need '^kind: DiscoveryServiceConfig$' "cluster discovery on"
  need '^kind: KubePrismConfig$' "KubePrism on"
  need '^        enforce: restricted$' "Pod Security enforces restricted"
  need "^hostname: $n\$" "static hostname $n"
  need '^kind: UnattendedInstallConfig$' "install document"
  need '^ *- --iface-can-reach=10\.20\.0\.1$' "Flannel VXLAN on the private network"
  never 'node-role.kubernetes.io/control-plane: NoSchedule' "workloads may run on the control plane nodes"
  never '^    (kubelet|install|network):' "no v1alpha1 machine.kubelet/install/network"
  never '^    (apiServer|network|discovery|allowSchedulingOnControlPlanes):' "no v1alpha1 cluster.apiServer/network/discovery"
done
exit "$failed"
