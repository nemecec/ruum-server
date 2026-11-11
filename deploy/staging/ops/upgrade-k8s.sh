#!/usr/bin/env bash
# Upgrades Kubernetes with `talosctl upgrade-k8s` (review 2 U-1), after upgrade-talos.sh. Change
# KUBERNETES_VERSION in versions.env in a pull request first. talosctl upgrades the control plane
# components and the kubelets node by node and checks each step.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../versions.env
. "$HERE/../versions.env"
WORK="${RUUM_STAGING_WORK:-$HOME/.ruum-staging}"
export TALOSCONFIG="$WORK/talosconfig" KUBECONFIG="$WORK/kubeconfig"
bash "$HERE/check-capacity.sh"
first="$(talosctl config info -o json | jq -r '.endpoints[0]')"
talosctl -n "$first" upgrade-k8s --to "${KUBERNETES_VERSION#v}"
kubectl -n data wait --for=condition=Ready cluster/pg --timeout=15m >/dev/null
echo "Kubernetes is at $KUBERNETES_VERSION"
