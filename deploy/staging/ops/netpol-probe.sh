#!/usr/bin/env bash
# Network policy probe of staging (backlog O-24). Runs in the tools container (kubectl):
#
#   deploy/staging/status.sh --netpol      (or: deploy/staging/tool.sh bash deploy/staging/ops/netpol-probe.sh)
#
# Tests TCP reachability from an unknown pod in a temporary namespace, from ephemeral containers
# inside the real ruum-server, edge and (card I-6) gateway pods (same network identity, no fake pod
# behind their Services), and from a sandbox: a pod of the runtime class gvisor in ruum-apps. Prints PASS/FAIL per path; exit 1 on any FAIL.
# Not read-only: it creates the namespace netpol-probe (deleted on exit), a short sandbox pod in
# ruum-apps, and adds an ephemeral container to one ruum-server pod, one edge pod and one gateway
# pod (Kubernetes keeps finished ephemeral
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

# 4. The app runtime (card I-6), when the release has it.
if [[ -z "$(kubectl -n ruum get pods -l app=ruum-gateway -o name 2>/dev/null)" ]]; then
  echo "SKIP the app runtime: no ruum-gateway pod (a release without it)"
else
  T_GW=ruum-gateway.ruum.svc.cluster.local:8494
  T_EG=ruum-egress.ruum.svc.cluster.local:3128
  APPS=("$T_GW" "$T_EG")
  out="$(kubectl -n netpol-probe run probe2 --image=$IMG --restart=Never --rm -i --quiet \
    --overrides='{"spec":{"securityContext":{"runAsNonRoot":true,"runAsUser":65534,"seccompProfile":{"type":"RuntimeDefault"}},"containers":[{"name":"probe2","image":"'$IMG'","command":["sh","-c","'"$(probe_cmd "${APPS[@]}")"'"],"securityContext":{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]}}}]}}' 2>&1)"
  check unknown-pod "$out" "$T_GW" closed
  check unknown-pod "$out" "$T_EG" closed
  out="$(inside edge app=ruum-edge "$T_GW" "$T_EG")"
  check edge "$out" "$T_GW" open
  check edge "$out" "$T_EG" closed
  out="$(inside ruum app=ruum-server "$T_GW" "$T_EG")"
  check ruum-server "$out" "$T_GW" closed
  check ruum-server "$out" "$T_EG" closed
  # The gateway has an egress policy: the core's internal listener and Postgres, nothing else here.
  out="$(inside ruum app=ruum-gateway "${ALL[@]}" "$T_EG")"
  check gateway "$out" "$T_SERVER_INT" open
  check gateway "$out" "$T_PG" open
  check gateway "$out" "$T_SERVER" closed
  check gateway "$out" "$T_SPICE_HTTP" closed
  check gateway "$out" "$T_NATS" closed
  check gateway "$out" "$T_MAIL" closed
  check gateway "$out" "$T_ZITADEL" closed
  check gateway "$out" "$T_EG" closed
  # A sandbox: a pod of the runtime class gvisor in ruum-apps. It has no DNS, so the targets are
  # Service addresses. The egress proxy is the positive control: open there, closed elsewhere.
  ip() { kubectl -n "$1" get service "$2" -o jsonpath='{.spec.clusterIP}'; }
  S_EG="$(ip ruum ruum-egress):3128" S_GW="$(ip ruum ruum-gateway):8494" S_SRV="$(ip ruum ruum-server):8491"
  S_INT="$(ip ruum ruum-server-internal):8493" S_PG="$(ip data pg-rw):5432" S_SPICE="$(ip authz spicedb):8443"
  S_DNS="$(ip kube-system kube-dns):53" S_META=169.254.169.254:80 S_NET=1.1.1.1:443
  SB=("$S_EG" "$S_GW" "$S_SRV" "$S_INT" "$S_PG" "$S_SPICE" "$S_DNS" "$S_META" "$S_NET")
  # The admission rules of ruum-apps (k8s/manifests/apps-admission.yaml, I6-20) admit only the
  # shape of a sandbox with its supervisor, so the probe pod has the shape of the positive control
  # of ops/gvisor-check.sh (pod_spec): the ServiceAccount ruum-sandbox, the init container
  # ruum-supervisor (here only `true`) that mounts only ruum-bin, ruum-bin read-only in the app
  # container, and the one projected token of the audience ruum-supervisor (600 s). The app
  # container runs the probe. The network policies of ruum-apps select every pod ({}), so this
  # pod gets the same rules as a real sandbox.
  sb_spec="$(jq -cn --arg img "$IMG" --arg cmd "$(probe_cmd "${SB[@]}")" '{spec: {
    restartPolicy: "Never",
    runtimeClassName: "gvisor",
    serviceAccountName: "ruum-sandbox",
    automountServiceAccountToken: false,
    securityContext: { runAsNonRoot: true, runAsUser: 65534, runAsGroup: 65534, seccompProfile: { type: "RuntimeDefault" } },
    volumes: [{name: "ruum-bin", emptyDir: {medium: "Memory"}},
      {name: "ruum-id", projected: {sources: [{serviceAccountToken: {path: "token", audience: "ruum-supervisor", expirationSeconds: 600}}]}}],
    containers: [{
      name: "sandbox-probe", image: $img, command: ["sh", "-c", $cmd],
      resources: { requests: { cpu: "10m", memory: "16Mi" }, limits: { memory: "32Mi" } },
      securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: { drop: ["ALL"] } },
      volumeMounts: [{name: "ruum-bin", mountPath: "/ruum/bin", readOnly: true}, {name: "ruum-id", mountPath: "/ruum/id", readOnly: true}]
    }]
  }} | .spec.initContainers = [.spec.containers[0] | .name = "ruum-supervisor" | .command = ["true"] | .volumeMounts = [{name: "ruum-bin", mountPath: "/ruum/bin"}]]')"
  out="$(kubectl -n ruum-apps run sandbox-probe --image=$IMG --restart=Never --rm -i --quiet --overrides="$sb_spec" 2>&1)"
  check sandbox "$out" "$S_EG" open
  for t in "$S_GW" "$S_SRV" "$S_INT" "$S_PG" "$S_SPICE" "$S_DNS" "$S_META" "$S_NET"; do check sandbox "$out" "$t" closed; done
fi

[[ $fail == 0 ]] && echo "netpol probe: all paths as expected" || echo "netpol probe: FAILED"
exit "$fail"
