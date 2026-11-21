#!/usr/bin/env bash
# Upgrades Talos one node at a time (review 2 U-1). On the Mac, in the tools container, with the
# admin talosconfig and kubeconfig of ~/.config/ruum/staging:
#   deploy/staging/tool.sh bash deploy/staging/ops/upgrade-talos.sh [--check]
# Change TALOS_VERSION or TALOS_SCHEMATIC (the installer image follows) in versions.env in a pull
# request first; then run this from that commit. A node is current when it runs TALOS_VERSION
# with the schematic TALOS_SCHEMATIC (Image Factory adds the pseudo extension "schematic" whose
# version is the schematic ID); a new schematic with the same version is an upgrade too (card
# I-6: the gVisor extension). After each node it checks that the node runs the schematic and
# every extension of talos/schematic.yaml.
#   --check   only print each node's version, schematic and extensions; exit 1 when one is not
#             current. Changes nothing.
#
# Before each node: etcd has 3 healthy members, every pod is Running, and the CNPG primary is
# moved away from the node (switchover). The sum of requests must fit on N-1 nodes (review 2
# U-2, ops/check-capacity.sh), or the drain hangs.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../versions.env
. "$HERE/../versions.env"
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export TALOSCONFIG="$WORK/talosconfig" KUBECONFIG="$WORK/kubeconfig"
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

check_only=0
[[ "${1:-}" == --check ]] && check_only=1
# The extensions of talos/schematic.yaml ("- siderolabs/<name>" lines), for example gvisor.
want_ext="$(sed -n 's#^ *- siderolabs/\([a-z0-9-]*\) *$#\1#p' "$HERE/../talos/schematic.yaml" | sort | tr '\n' ' ')"

# node_state IP: "<version> <schematic> <extension names, sorted>" of a node (talosctl through the
# admin tunnel). Empty fields as "-".
node_state() {
  local ip="$1" version ext schematic names
  version="$(talosctl -n "$ip" version --short 2>/dev/null | awk '/Tag:/ {print $2}' | tail -1)"
  ext="$(talosctl -n "$ip" get extensions -o json 2>/dev/null | jq -rs '[.[] | .spec.metadata // empty | {name, version}]' 2>/dev/null || echo '[]')"
  schematic="$(jq -r '[.[] | select(.name == "schematic") | .version] | first // "-"' <<<"$ext")"
  names="$(jq -r '[.[] | select(.name != "schematic") | .name | sub("^siderolabs/"; "")] | sort | join(" ")' <<<"$ext")"
  printf '%s %s %s\n' "${version:--}" "${schematic:--}" "${names:--}"
}

# current STATE: the node runs TALOS_VERSION, TALOS_SCHEMATIC and every wanted extension.
current() {
  local version schematic names e
  read -r version schematic names <<<"$1"
  [[ "$version" == "$TALOS_VERSION" && "$schematic" == "$TALOS_SCHEMATIC" ]] || return 1
  for e in $want_ext; do
    [[ " $names " == *" $e "* ]] || return 1
  done
}

if [[ $check_only == 1 ]]; then
  rc=0
  while IFS='=' read -r name ip; do
    [[ -n "$name" ]] || continue
    st="$(node_state "$ip")"
    if current "$st"; then log "$name current: $st"; else log "$name NOT current: $st (want $TALOS_VERSION $TALOS_SCHEMATIC $want_ext)"; rc=1; fi
  done < <(kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}={.status.addresses[?(@.type=="InternalIP")].address}{"\n"}{end}')
  exit "$rc"
fi

bash "$HERE/check-capacity.sh"
# Each node by its private address: the talosctl endpoints are the admin tunnel (127.0.0.1:1500N),
# and the endpoint's apid relays to the node over the private network (the firewall closes the
# public Talos API to everything but the build server).
nodes="$(kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}={.status.addresses[?(@.type=="InternalIP")].address}{"\n"}{end}')"
[[ -n "$nodes" ]] || die "no nodes"
while IFS='=' read -r name ip; do
  [[ -n "$name" ]] || continue
  [[ -n "$ip" ]] || die "no internal address for $name"
  st="$(node_state "$ip")"
  if current "$st"; then
    log "$name is at $TALOS_VERSION with schematic $TALOS_SCHEMATIC ($st)"
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
  log "$name: talosctl upgrade to $TALOS_INSTALLER_IMAGE (--preserve); was: $st"
  talosctl -n "$ip" upgrade --image "$TALOS_INSTALLER_IMAGE" --preserve --wait --timeout 15m
  kubectl wait --for=condition=Ready "node/$name" --timeout=10m >/dev/null
  kubectl -n data wait --for=condition=Ready cluster/pg --timeout=15m >/dev/null
  st="$(node_state "$ip")"
  current "$st" || die "$name after the upgrade: $st, not $TALOS_VERSION $TALOS_SCHEMATIC with $want_ext; stopping before the next node"
  log "$name: $st"
done <<<"$nodes"
log "every node is at $TALOS_VERSION with schematic $TALOS_SCHEMATIC (extensions: $want_ext)"
