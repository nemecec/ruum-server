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
# 3. The admission rules of ruum-apps (k8s/manifests/apps-admission.yaml, backlog S-83 and
#    S-89): server-side dry runs of pods in ruum-apps that must be refused by the policy: without
#    the runtime class, with an automounted token, with another ServiceAccount, with a projected
#    token of another audience or lifetime, with a Secret (a volume, or an environment value),
#    with envFrom, with another init container, with a writable supervisor volume, and with the
#    termination message from the logs. A gVisor pod in the shape of the sandbox supervisor (the
#    init container ruum-supervisor, the read-only volume ruum-bin, the projected token of the
#    audience ruum-supervisor) must pass the dry run (the positive control: the refusals come
#    from the rules, not from another error).
# 4. The Secret rule of ruum-apps (the same file, policy ruum-apps-secrets, backlog S-89): a
#    Secret in ruum-apps is refused, whatever it is (an Opaque one, a service account token
#    Secret).
# 5. The pod read-back (review of I6-20, required change 2): the driver's own sample pod
#    (k8s/sandbox-sample-pod.json, which a test of crates/ruum-runtime holds equal to the
#    driver's pod) as a server-side dry run in ruum-apps. The answer (its name, labels,
#    annotations and spec) goes to $RUUM_STAGING_WORK/sandbox-sample-pod.stored.json and must
#    equal the committed k8s/sandbox-sample-pod.stored.json, which CI holds to the driver's
#    read-back (the test the_api_servers_answer_to_the_sample_pod_passes_the_read_back). A new
#    answer (another Kubernetes release, another admission plugin) fails until it is committed
#    and CI passes with it. The driver is not turned on before this step passes.
# Not read-only in step 2: it creates two short pods per node (ruum-apps and ops) and deletes
# them. Steps 3, 4 and 5 create nothing (dry runs).
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
    } + (if $class != "" then { runtimeClassName: $class, serviceAccountName: "ruum-sandbox" } else {} end))
  }
  # In ruum-apps: the shape of a sandbox with its supervisor (the admission rules, S-89): the
  # init container ruum-supervisor (here only `true`), the read-only volume ruum-bin and the
  # projected token of the audience ruum-supervisor.
  | if $class != "" then
      .spec.volumes = [{name: "ruum-bin", emptyDir: {medium: "Memory"}},
        {name: "ruum-id", projected: {sources: [{serviceAccountToken: {path: "token", audience: "ruum-supervisor", expirationSeconds: 600}}]}}]
      | .spec.initContainers = [.spec.containers[0] | .name = "ruum-supervisor" | .command = ["true"] | .volumeMounts = [{name: "ruum-bin", mountPath: "/ruum/bin"}]]
      | .spec.containers[0].volumeMounts = [{name: "ruum-bin", mountPath: "/ruum/bin", readOnly: true}, {name: "ruum-id", mountPath: "/ruum/id", readOnly: true}]
    else . end'
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

# 3. The admission rules of ruum-apps (backlog S-83): dry runs on the API server, nothing is made.
# The pod is labelled as an instance; its own Secret is ruum-i-<that ID without dashes>.
OWN_ID=0190f0f0-0000-7000-8000-000000000001
OWN_SECRET=ruum-i-0190f0f0000070008000000000000001
OTHER_SECRET=ruum-i-0190f0f0000070008000000000000002
# dry_run CLASS [JQ]: the API server's answer to a pod in ruum-apps, changed by the jq filter JQ.
dry_run() {
  local node spec
  node="$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')"
  spec="$(pod_spec "$node" "$IMG" "$1" | jq -c --arg id "$OWN_ID" --arg own "$OWN_SECRET" --arg other "$OTHER_SECRET" "
    {apiVersion: \"v1\", kind: \"Pod\", metadata: {name: \"gvisor-check-admission\", namespace: \"ruum-apps\", labels: {\"ruum.instance\": \$id}}} + .
    | ${2:-.}")"
  kubectl create --dry-run=server -f - -o name <<<"$spec" 2>&1
}
# refused WHAT NEEDLE CLASS [JQ]: the dry run must be refused by the policy with NEEDLE.
refused() {
  local out
  out="$(dry_run "$3" "${4:-.}" || true)"
  if [[ "$out" == *"ruum-apps-gvisor"* && "$out" == *"$2"* ]]; then
    pass "ruum-apps refuses a pod $1 (the admission rule)"
  else
    bad "ruum-apps did not refuse a pod $1 with its admission rule: ${out:-no answer} (k8s/manifests/apps-admission.yaml)"
  fi
}
refused "without the runtime class gvisor" "gVisor only" ""
refused "with an automounted service account token" "no automounted service account token" gvisor '.spec.automountServiceAccountToken = true'
refused "with another ServiceAccount" "ServiceAccount ruum-sandbox only" gvisor '.spec.serviceAccountName = "default"'
refused "with a projected token of the API's audience" "mount only emptyDir" gvisor \
  '.spec.volumes[1].projected.sources[0].serviceAccountToken = {path: "token"}'
refused "with a long-lived supervisor token" "mount only emptyDir" gvisor \
  '.spec.volumes[1].projected.sources[0].serviceAccountToken.expirationSeconds = 86400'
refused "with a second projected volume" "exactly one projected volume" gvisor \
  '.spec.volumes += [.spec.volumes[1] | .name = "ruum-id2"]'
refused "that mounts a Secret" "mount only emptyDir" gvisor \
  '.spec.volumes += [{name: "token", secret: {secretName: $other}}]'
refused "that takes a value of a Secret" "no environment value from another object" gvisor \
  '.spec.containers[0].env = [{name: "X", valueFrom: {secretKeyRef: {name: $other, key: "token"}}}]'
refused "with envFrom" "no envFrom" gvisor '.spec.containers[0].envFrom = [{secretRef: {name: $own}}]'
refused "with another init container" "init container ruum-supervisor" gvisor \
  '.spec.initContainers[0].name = "other"'
refused "without the supervisor's init container" "init container ruum-supervisor" gvisor 'del(.spec.initContainers)'
refused "with a writable supervisor volume" "read-only in every app container" gvisor \
  '.spec.containers[0].volumeMounts[0].readOnly = false'
refused "without the supervisor volume in the app" "read-only in every app container" gvisor \
  '.spec.containers[0].volumeMounts = [.spec.containers[0].volumeMounts[1]]'
refused "with the termination message from the logs" "termination message path" gvisor \
  '.spec.containers[0].terminationMessagePolicy = "FallbackToLogsOnError"'
# The positive control: the shape of a sandbox with its supervisor (the driver's pod, pod.rs).
out="$(dry_run gvisor '.' || true)"
if [[ "$out" == "pod/gvisor-check-admission"* ]]; then
  pass "a restricted gVisor pod in the shape of the sandbox supervisor passes the admission rules (the positive control)"
else
  bad "a restricted gVisor pod in the supervisor's shape did not pass the dry run, so the refusals above prove nothing: ${out:-no answer}"
fi

# 4. The Secret rule of ruum-apps (backlog S-89): dry runs, nothing is made.
# secret_dry_run [JQ]: the API server's answer to a Secret in ruum-apps, changed by JQ.
secret_dry_run() {
  jq -cn --arg id "$OWN_ID" --arg own "$OWN_SECRET" "{apiVersion: \"v1\", kind: \"Secret\", type: \"Opaque\",
    metadata: {name: \$own, namespace: \"ruum-apps\", labels: {\"ruum.instance\": \$id}},
    stringData: {token: \"not-a-secret\"}} | ${1:-.}" | kubectl create --dry-run=server -f - -o name 2>&1
}
# refused_secret WHAT JQ: the dry run must be refused by the Secret policy.
refused_secret() {
  local out
  out="$(secret_dry_run "$2" || true)"
  if [[ "$out" == *"ruum-apps-secrets"* && "$out" == *"no Secret in ruum-apps"* ]]; then
    pass "ruum-apps refuses a Secret $1 (the Secret rule)"
  else
    bad "ruum-apps did not refuse a Secret $1 with its Secret rule: ${out:-no answer} (k8s/manifests/apps-admission.yaml)"
  fi
}
refused_secret "named for an instance (Opaque)" '.'
refused_secret "of the type kubernetes.io/service-account-token" \
  '.type = "kubernetes.io/service-account-token" | .metadata.annotations = {"kubernetes.io/service-account.name": "ruum-sandbox"}'

# 5. The pod read-back (review of I6-20): the API server's defaults against the driver's check.
K8S="$(cd "$(dirname "${BASH_SOURCE[0]}")/../k8s" && pwd)"
SAMPLE="$K8S/sandbox-sample-pod.json"
COMMITTED="$K8S/sandbox-sample-pod.stored.json"
mkdir -p "$WORK"
LIVE="$WORK/sandbox-sample-pod.stored.json"
NORM='{metadata: ({name: .metadata.name, labels: .metadata.labels} + (if (.metadata.annotations // {}) == {} then {} else {annotations: .metadata.annotations} end)), spec: .spec}'
if answer="$(kubectl -n "$NS" create --dry-run=server -o json -f "$SAMPLE" 2>&1)" &&
  jq -S "$NORM" <<<"$answer" >"$LIVE" 2>/dev/null; then
  if [[ -f "$COMMITTED" ]] && [[ "$(jq -S . "$COMMITTED")" == "$(cat "$LIVE")" ]]; then
    pass "the API server stores the driver's sample pod as committed, and CI holds that answer to the driver's read-back"
  else
    bad "the API server's answer to the sample pod is not the committed one: copy $LIVE to deploy/staging/k8s/sandbox-sample-pod.stored.json, commit it, and let CI run the read-back test (the driver stays off until then)"
  fi
else
  bad "the sample pod did not pass the dry run: ${answer:-no answer}"
fi

[[ $fail == 0 ]] && echo "gvisor check: passed" || echo "gvisor check: FAILED"
exit "$fail"
