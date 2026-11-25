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
#
# After each node (backlog O-54): the kubectl calls retry for a while, because the admin
# tunnel's Kubernetes forward can go to the node that just rebooted, whose API server is still
# starting ("Unable to connect to the server: EOF"). Then the node's edge pod must be ready and
# answer TLS with a certificate for the public host (through kubectl port-forward), or the script
# stops before the next node: an edge without certificates fails requests (backlog O-53).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../versions.env
. "$HERE/../versions.env"
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export TALOSCONFIG="$WORK/talosconfig" KUBECONFIG="$WORK/kubeconfig"
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
# Seconds between two tries of retry (tests/run.sh makes it short).
RETRY_PAUSE="${RUUM_UPGRADE_RETRY_PAUSE:-5}"
# The public host of the edge (hosts.public of charts/ruum).
EDGE_HOST="$(sed -n 's/^  public: *\([a-z0-9.-]*\) *$/\1/p' "$HERE/../charts/ruum/values.yaml" | head -1)"

# retry SECONDS COMMAND...: runs COMMAND until it succeeds, at most SECONDS long in all (backlog
# O-54). SECONDS is a hard limit (Kimi re-check of O-53): each try runs under timeout with what is
# left of it, and timeout kills a try that hangs (a stalled connection, a TLS handshake that never
# ends) with its whole process group, kubectl port-forward and openssl included. COMMAND may be a
# function of this script: the functions are exported (see below), and bash -c runs them.
retry() {
  local limit="$1" start=$SECONDS left
  shift
  while :; do
    left=$(( limit - (SECONDS - start) ))
    (( left > 0 )) || return 1
    timeout -k 5 "$left" bash -o pipefail -c '"$@"' retry "$@" && return 0
    (( SECONDS - start < limit )) || return 1
    log "failed: $* (the API forward may be on the node that rebooted); again in ${RETRY_PAUSE} s"
    sleep "$RETRY_PAUSE"
  done
}

# edge_pod NODE: the name of a running edge pod on NODE (the DaemonSet ruum-edge), or nothing.
edge_pod() {
  kubectl -n edge get pods -l app=ruum-edge --field-selector "spec.nodeName=$1,status.phase=Running" \
    -o jsonpath='{.items[0].metadata.name}' --request-timeout=20s 2>/dev/null
}

# edge_tls POD: the pod answers TLS on 8443 with a certificate for EDGE_HOST (kubectl
# port-forward to the pod; the connection comes from inside the pod, so Caddy wants no PROXY
# header). Prints the certificate's subject and end date.
edge_tls() {
  local pod="$1" pf port rc=1 out
  out="$(mktemp)"
  kubectl -n edge port-forward "pod/$pod" :8443 >"$out" 2>&1 </dev/null &
  pf=$!
  # retry's timeout ends a try that hangs with SIGTERM.
  trap 'kill "$pf" 2>/dev/null; rm -f "$out" "$out.cert"; exit 143' TERM
  port=""
  for _ in $(seq 1 50); do
    port="$(sed -n 's/^Forwarding from 127\.0\.0\.1:\([0-9]*\) .*/\1/p' "$out" | head -1)"
    [[ -n "$port" ]] && break
    kill -0 "$pf" 2>/dev/null || break
    sleep 0.2
  done
  if [[ -n "$port" ]]; then
    timeout -k 2 20 openssl s_client -connect "127.0.0.1:$port" -servername "$EDGE_HOST" </dev/null 2>/dev/null |
      openssl x509 -noout -checkhost "$EDGE_HOST" -subject -enddate >"$out.cert" 2>/dev/null &&
      grep -q 'does match certificate' "$out.cert" && { grep -v 'match certificate' "$out.cert" >&2; rc=0; }
  fi
  kill "$pf" 2>/dev/null || true
  wait "$pf" 2>/dev/null || true
  rm -f "$out" "$out.cert"
  trap - TERM
  return "$rc"
}

# edge_ok NODE: a running edge pod on NODE is Ready and answers with a certificate.
edge_ok() {
  local pod
  pod="$(edge_pod "$1")"
  [[ -n "$pod" ]] || return 1
  kubectl -n edge wait --for=condition=Ready "pod/$pod" --timeout=30s >/dev/null || return 1
  edge_tls "$pod" || return 1
  log "$1: edge pod $pod is ready and answers with a certificate for $EDGE_HOST"
}
# retry runs these in a new bash (bash -c under timeout).
export -f log edge_pod edge_tls edge_ok
export EDGE_HOST

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
  notready="$(kubectl get pods -A --no-headers --field-selector=status.phase!=Succeeded,status.phase!=Failed | awk '$4 != "Running"' | wc -l)"
  [[ "$notready" -eq 0 ]] || die "$((notready)) pods are not Running"
  primary="$(kubectl -n data get cluster pg -o jsonpath='{.status.currentPrimary}')"
  primary_node="$(kubectl -n data get pod "$primary" -o jsonpath='{.spec.nodeName}')"
  if [[ "$primary_node" == "$name" ]]; then
    target="$(kubectl -n data get pods -l cnpg.io/cluster=pg,cnpg.io/instanceRole=replica -o jsonpath='{.items[0].metadata.name}')"
    log "switchover of the CNPG primary $primary to $target"
    kubectl -n data patch cluster pg --type merge --subresource status -p "{\"status\":{\"targetPrimary\":\"$target\"}}" >/dev/null
    kubectl -n data wait --for=jsonpath='{.status.currentPrimary}'="$target" cluster/pg --timeout=5m >/dev/null
  fi
  log "$name: talosctl upgrade to $TALOS_INSTALLER_IMAGE (--preserve); was: $st"
  talosctl -n "$ip" upgrade --legacy --image "$TALOS_INSTALLER_IMAGE" --preserve --wait --timeout 15m
  retry 600 kubectl wait --for=condition=Ready "node/$name" --timeout=10m >/dev/null ||
    die "$name is not Ready after 10 minutes; stopping before the next node"
  retry 900 kubectl -n data wait --for=condition=Ready cluster/pg --timeout=15m >/dev/null ||
    die "the CNPG cluster pg is not Ready after 15 minutes; stopping before the next node"
  st="$(node_state "$ip")"
  current "$st" || die "$name after the upgrade: $st, not $TALOS_VERSION $TALOS_SCHEMATIC with $want_ext; stopping before the next node"
  log "$name: $st"
  # The edge restarts the startup probe's 5 minutes after a failed certificate load; 6 minutes
  # cover one restart.
  retry "${RUUM_UPGRADE_EDGE_SECONDS:-360}" edge_ok "$name" ||
    die "$name: no ready edge pod that answers with a certificate for $EDGE_HOST after 6 minutes; stopping before the next node (look at: kubectl -n edge get pods -o wide --field-selector spec.nodeName=$name; kubectl -n edge logs <pod>; delete the pod to start it again)"
done <<<"$nodes"
log "every node is at $TALOS_VERSION with schematic $TALOS_SCHEMATIC (extensions: $want_ext)"
