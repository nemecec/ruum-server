#!/usr/bin/env bash
# The gVisor check of staging (card I-6). In the tools container with the admin kubeconfig:
#
#   deploy/staging/tool.sh bash deploy/staging/ops/gvisor-check.sh [--class-only]
#
# 1. The handler: every node runs the Talos extension gvisor (talosctl get extensions, through the
#    admin tunnel), so containerd knows the handler runsc.
# 2. Unless --class-only (k8s/apply.sh runs that before the runtime class exists): on each node,
#    one pod of the runtime class gvisor in ruum-apps and one control pod without it in ops (the
#    admission rule of ruum-apps refuses a pod without gVisor there: backlog S-83), both
#    restricted, both pinned to the node. The control pod must see the node's kernel
#    (status.nodeInfo.kernelVersion) and the gVisor pod must not: gVisor answers uname with its
#    own kernel. A pod that does not start, or a control pod that does not see the node's
#    kernel, fails the check (no pass on silence, decision 0006 G).
# 3. The admission rule of ruum-apps (k8s/manifests/apps-admission.yaml): a server-side dry run of
#    a pod without the runtime class, and of one with a service account token, in ruum-apps must
#    be refused by the policy; the same pod with gVisor must pass the dry run (the positive
#    control: the refusal comes from the rule, not from another error).
# Not read-only in step 2: it creates two short pods per node (ruum-apps and ops) and deletes
# them. Step 3 creates nothing (dry runs).
# Exit 1 on any failure.
set -uo pipefail
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export TALOSCONFIG="${TALOSCONFIG:-$WORK/talosconfig}"
IMG=busybox:1.37.0
NS=ruum-apps
# The control pod without gVisor: not in ruum-apps, whose admission rule refuses it (S-83).
CONTROL_NS=ops
fail=0
pass() { printf 'PASS %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; fail=1; }

nodes="$(kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}={.status.addresses[?(@.type=="InternalIP")].address}={.status.nodeInfo.kernelVersion}{"\n"}{end}')"
[[ -n "$nodes" ]] || { bad "no nodes"; exit 1; }

# 1. The extension on every node.
while IFS='=' read -r name ip _; do
  [[ -n "$name" ]] || continue
  if talosctl -n "$ip" get extensions -o json 2>/dev/null |
    jq -es 'any(.[]; (.spec.metadata.name // "" | sub("^siderolabs/"; "")) == "gvisor")' >/dev/null 2>&1; then
    pass "$name runs the Talos extension gvisor"
  else
    bad "$name does not run the Talos extension gvisor (ops/upgrade-talos.sh with the schematic of versions.env)"
  fi
done <<<"$nodes"
if [[ "${1:-}" == --class-only ]]; then
  [[ $fail == 0 ]] && echo "gvisor check (extensions): passed" || echo "gvisor check (extensions): FAILED"
  exit "$fail"
fi

kubectl get runtimeclass gvisor >/dev/null 2>&1 || { bad "no runtime class gvisor (k8s/apply.sh applies k8s/manifests/runtimeclass.yaml)"; exit 1; }

# pod_spec NODE IMAGE CLASS: the restricted pod of the checks (CLASS empty: no runtime class).
pod_spec() {
  jq -cn --arg node "$1" --arg img "$2" --arg class "$3" '{
    spec: ({
      nodeName: $node,
      restartPolicy: "Never",
      automountServiceAccountToken: false,
      securityContext: { runAsNonRoot: true, runAsUser: 65534, runAsGroup: 65534, seccompProfile: { type: "RuntimeDefault" } },
      tolerations: [{ key: "node-role.kubernetes.io/control-plane", operator: "Exists", effect: "NoSchedule" }],
      containers: [{
        name: "probe", image: $img, command: ["uname", "-r"],
        resources: { requests: { cpu: "10m", memory: "16Mi" }, limits: { memory: "32Mi" } },
        securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: { drop: ["ALL"] } }
      }]
    } + (if $class != "" then { runtimeClassName: $class } else {} end))
  }'
}

# run_on NODE NAME [RUNTIMECLASS]: the kernel release that a restricted pod on NODE sees. Without a
# runtime class the pod runs in CONTROL_NS.
run_on() {
  local node="$1" pod="$2" class="${3:-}" spec ns="$NS"
  [[ -n "$class" ]] || ns="$CONTROL_NS"
  spec="$(pod_spec "$node" "$IMG" "$class")"
  kubectl -n "$ns" delete pod "$pod" --ignore-not-found --wait=true >/dev/null 2>&1
  kubectl -n "$ns" run "$pod" --image="$IMG" --restart=Never --overrides="$spec" >/dev/null 2>&1 || return 1
  kubectl -n "$ns" wait --for=jsonpath='{.status.phase}'=Succeeded "pod/$pod" --timeout=120s >/dev/null 2>&1 || {
    kubectl -n "$ns" delete pod "$pod" --wait=false >/dev/null 2>&1
    return 1
  }
  kubectl -n "$ns" logs "$pod" 2>/dev/null | tr -d '\r' | head -n 1
  kubectl -n "$ns" delete pod "$pod" --wait=false >/dev/null 2>&1
}

# 2. A gVisor pod and a control pod on each node.
while IFS='=' read -r name _ kernel; do
  [[ -n "$name" ]] || continue
  control="$(run_on "$name" "gvisor-check-control-$name")" || control=""
  if [[ -n "$kernel" && "$control" == "$kernel" ]]; then
    pass "$name: a pod without the runtime class sees the node's kernel ($kernel)"
  else
    bad "$name: the control pod saw '${control:-nothing}', not the node's kernel '$kernel'; the check proves nothing on this node"
    continue
  fi
  sandboxed="$(run_on "$name" "gvisor-check-runsc-$name" gvisor)" || sandboxed=""
  if [[ -z "$sandboxed" ]]; then
    bad "$name: a pod of the runtime class gvisor did not run (kubectl -n $NS get events)"
  elif [[ "$sandboxed" == "$kernel" ]]; then
    bad "$name: a pod of the runtime class gvisor sees the node's kernel $kernel: it is not in gVisor"
  else
    pass "$name: a pod of the runtime class gvisor sees gVisor's kernel ($sandboxed), not the node's"
  fi
done <<<"$nodes"

# 3. The admission rule of ruum-apps (backlog S-83): dry runs on the API server, nothing is made.
# dry_run CLASS [TOKEN]: the API server's answer to a pod in ruum-apps (TOKEN=yes: with a token).
dry_run() {
  local node spec
  node="$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')"
  spec="$(pod_spec "$node" "$IMG" "$1" | jq -c --arg tok "${2:-}" '
    {apiVersion: "v1", kind: "Pod", metadata: {name: "gvisor-check-admission", namespace: "ruum-apps"}} + .
    | if $tok == "yes" then .spec.automountServiceAccountToken = true else . end')"
  kubectl create --dry-run=server -f - -o name <<<"$spec" 2>&1
}
out="$(dry_run "" || true)"
if [[ "$out" == *"ruum-apps-gvisor"* && "$out" == *"gVisor only"* ]]; then
  pass "ruum-apps refuses a pod without the runtime class gvisor (the admission rule)"
else
  bad "ruum-apps did not refuse a pod without gVisor with its admission rule: ${out:-no answer} (k8s/manifests/apps-admission.yaml)"
fi
out="$(dry_run gvisor yes || true)"
if [[ "$out" == *"ruum-apps-gvisor"* && "$out" == *"service account token"* ]]; then
  pass "ruum-apps refuses a pod with a service account token"
else
  bad "ruum-apps did not refuse a pod with a service account token: ${out:-no answer}"
fi
out="$(dry_run gvisor || true)"
if [[ "$out" == "pod/gvisor-check-admission"* ]]; then
  pass "a restricted gVisor pod passes the admission rule (the positive control)"
else
  bad "a restricted gVisor pod did not pass the dry run, so the refusals above prove nothing: ${out:-no answer}"
fi

[[ $fail == 0 ]] && echo "gvisor check: passed" || echo "gvisor check: FAILED"
exit "$fail"
