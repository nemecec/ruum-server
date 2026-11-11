#!/usr/bin/env bash
# Upgrades Talos one node at a time (review 2 U-1). On the build server as ruum-ops, with the
# admin talosconfig and kubeconfig of ~/.ruum-staging. Change TALOS_VERSION (and the installer
# image) in versions.env in a pull request first; then run this from that commit.
#
# Before each node: etcd has 3 healthy members, every pod is Running, and the CNPG primary is
# moved away from the node (switchover). The sum of requests must fit on N-1 nodes (review 2
# U-2, ops/check-capacity.sh), or the drain hangs.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../versions.env
. "$HERE/../versions.env"
WORK="${RUUM_STAGING_WORK:-$HOME/.ruum-staging}"
export TALOSCONFIG="$WORK/talosconfig" KUBECONFIG="$WORK/kubeconfig"
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

bash "$HERE/check-capacity.sh"
nodes="$(kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}={.status.addresses[?(@.type=="ExternalIP")].address}{"\n"}{end}')"
[[ -n "$nodes" ]] || die "no nodes"
while IFS='=' read -r name ip; do
  [[ -n "$name" ]] || continue
  [[ -n "$ip" ]] || die "no external address for $name"
  current="$(talosctl -n "$ip" version --short 2>/dev/null | awk '/Tag:/ {print $2}' | tail -1)"
  if [[ "$current" == "$TALOS_VERSION" ]]; then
    log "$name is at $TALOS_VERSION"
    continue
  fi
  log "$name: checks before the upgrade"
  members="$(talosctl -n "$ip" etcd members 2>/dev/null | tail -n +2 | wc -l)"
  [[ "$members" -ge 3 ]] || die "etcd has $members members, not 3"
  talosctl -n "$ip" etcd status >/dev/null
  notready="$(kubectl get pods -A --no-headers | awk '$4 != "Running" && $4 != "Completed"' | wc -l)"
  [[ "$notready" == 0 ]] || die "$notready pods are not Running"
  primary="$(kubectl -n data get cluster pg -o jsonpath='{.status.currentPrimary}')"
  primary_node="$(kubectl -n data get pod "$primary" -o jsonpath='{.spec.nodeName}')"
  if [[ "$primary_node" == "$name" ]]; then
    target="$(kubectl -n data get pods -l cnpg.io/cluster=pg,cnpg.io/instanceRole=replica -o jsonpath='{.items[0].metadata.name}')"
    log "switchover of the CNPG primary $primary to $target"
    kubectl -n data patch cluster pg --type merge --subresource status -p "{\"status\":{\"targetPrimary\":\"$target\"}}" >/dev/null
    kubectl -n data wait --for=jsonpath='{.status.currentPrimary}'="$target" cluster/pg --timeout=5m >/dev/null
  fi
  log "$name: talosctl upgrade to $TALOS_VERSION (--preserve)"
  talosctl -n "$ip" upgrade --image "$TALOS_INSTALLER_IMAGE" --preserve --wait --timeout 15m
  kubectl wait --for=condition=Ready "node/$name" --timeout=10m >/dev/null
  kubectl -n data wait --for=condition=Ready cluster/pg --timeout=15m >/dev/null
done <<<"$nodes"
log "every node is at $TALOS_VERSION"
