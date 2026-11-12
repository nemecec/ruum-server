#!/usr/bin/env bash
# Network policy probe of staging (backlog O-24). Runs in the tools container (kubectl):
#
#   deploy/staging/status.sh --netpol      (or: deploy/staging/tool.sh bash deploy/staging/ops/netpol-probe.sh)
#
# Tests TCP reachability from three places: an unknown pod in a temporary namespace, and
# ephemeral containers inside the real ruum-server and edge pods (same network identity, no
# fake pod behind their Services). Prints PASS/FAIL per path; exit 1 on any FAIL.
# Not read-only: it creates the namespace netpol-probe (deleted on exit) and adds an ephemeral
# container to one ruum-server pod and one edge pod (Kubernetes keeps finished ephemeral
# containers in the pod spec until the pod is replaced; they use no resources).
# The expected paths follow charts/platform/templates/networkpolicies.yaml; change both together.
set -uo pipefail
IMG=busybox:1.37.0
fail=0
T_PG=pg-rw.data.svc.cluster.local:5432
T_SPICE_HTTP=spicedb.authz.svc.cluster.local:8443
T_SPICE_GRPC=spicedb.authz.svc.cluster.local:50051
T_ZITADEL=zitadel.auth.svc.cluster.local:8080
T_NATS=nats.nats.svc.cluster.local:4222
T_MAIL=mailpit.ops.svc.cluster.local:1025
T_SERVER=ruum-server.ruum.svc.cluster.local:8491
T_SERVER_INT=ruum-server-internal.ruum.svc.cluster.local:8493
T_EDGE=ruum-edge.edge.svc.cluster.local:80

# One shell line that tries every target and prints "<target> open|closed".
probe_cmd() {
  local s=""
  for t in "$@"; do
    s+="if nc -z -w 3 ${t%:*} ${t##*:} 2>/dev/null; then echo '$t open'; else echo '$t closed'; fi; "
  done
  printf '%s' "$s"
}

check() { # check <who> <output> <target> <want>
  local got
  got="$(grep -F "$3 " <<<"$2" | awk '{print $2}' | tail -n 1)"
  if [[ "$got" == "$4" ]]; then echo "PASS $1 -> $3 $got"; else echo "FAIL $1 -> $3: got '${got:-nothing}', want $4"; fail=1; fi
}

ALL=("$T_PG" "$T_SPICE_HTTP" "$T_SPICE_GRPC" "$T_ZITADEL" "$T_NATS" "$T_MAIL" "$T_SERVER" "$T_SERVER_INT" "$T_EDGE")

# 1. An unknown pod: every platform port is closed to it.
kubectl create namespace netpol-probe >/dev/null
kubectl label namespace netpol-probe pod-security.kubernetes.io/enforce=restricted >/dev/null
trap 'kubectl delete namespace netpol-probe --wait=false >/dev/null 2>&1' EXIT
out="$(kubectl -n netpol-probe run probe --image=$IMG --restart=Never --rm -i --quiet \
  --overrides='{"spec":{"securityContext":{"runAsNonRoot":true,"runAsUser":65534,"seccompProfile":{"type":"RuntimeDefault"}},"containers":[{"name":"probe","image":"'$IMG'","command":["sh","-c","'"$(probe_cmd "${ALL[@]}")"'"],"securityContext":{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]}}}]}}' 2>&1)"
for t in "${ALL[@]}"; do check unknown-pod "$out" "$t" closed; done

# 2./3. Inside the real pods, through an ephemeral container (restricted profile).
inside() { # inside <ns> <selector> <targets...>
  local ns="$1" sel="$2" pod name
  shift 2
  pod="$(kubectl -n "$ns" get pods -l "$sel" -o jsonpath='{.items[0].metadata.name}')"
  name="probe-$(date +%s)"
  kubectl -n "$ns" debug "$pod" --profile=restricted --image=$IMG -c "$name" --quiet -- sh -c "$(probe_cmd "$@")" >/dev/null 2>&1
  for _ in $(seq 1 30); do
    kubectl -n "$ns" get pod "$pod" -o jsonpath="{.status.ephemeralContainerStatuses[?(@.name=='$name')].state.terminated.reason}" 2>/dev/null | grep -q . && break
    sleep 2
  done
  kubectl -n "$ns" logs "$pod" -c "$name" 2>&1
}

out="$(inside ruum app=ruum-server "${ALL[@]}")"
check ruum-server "$out" "$T_PG" open
check ruum-server "$out" "$T_SPICE_HTTP" open
check ruum-server "$out" "$T_SPICE_GRPC" closed
check ruum-server "$out" "$T_ZITADEL" closed
check ruum-server "$out" "$T_NATS" open
check ruum-server "$out" "$T_MAIL" open
check ruum-server "$out" "$T_EDGE" closed

out="$(inside edge app=ruum-edge "${ALL[@]}")"
check edge "$out" "$T_SERVER" open
check edge "$out" "$T_SERVER_INT" open
check edge "$out" "$T_ZITADEL" open
check edge "$out" "$T_PG" closed
check edge "$out" "$T_SPICE_HTTP" closed
check edge "$out" "$T_NATS" closed
check edge "$out" "$T_MAIL" closed

[[ $fail == 0 ]] && echo "netpol probe: all paths as expected" || echo "netpol probe: FAILED"
exit "$fail"
