#!/usr/bin/env bash
# The gVisor check of staging (card I-6). In the tools container with the admin kubeconfig:
#
#   deploy/staging/tool.sh bash deploy/staging/ops/gvisor-check.sh [--class-only]
#   deploy/staging/tool.sh bash deploy/staging/ops/gvisor-check.sh --supervisor-image ghcr.io/nemecec/ruum-supervisor@sha256:<digest>
#   deploy/staging/tool.sh bash deploy/staging/ops/gvisor-check.sh --from-release   (k8s/apply.sh)
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
# 6. The kubelet's PID limit of every pod (backlog I6-19): each node's kubelet configuration
#    (configz, through the API server's node proxy) has podPidsLimit 4096
#    (talos/patches/cluster.yaml).
# 7. The sandbox supervisor under gVisor (backlog I620-3; docs/STEP3.md "The sandbox supervisor"
#    2): a pod in the driver's shape (k8s/sandbox-sample-pod.json: the init container copies the
#    supervisor of --supervisor-image, the app container runs it as PID 1) whose app is a shell
#    script that tries each way around it. First a server-side dry run (the admission rules
#    admit it), then the pod itself: the launch data (a test value, RLIMIT_NPROC 64) goes to the
#    supervisor's standard input (kubectl attach), and the pod's log (the kubelet's CRI file)
#    must show the value only as "<hidden secret>", the app could not open /proc/1/fd/1,
#    /proc/1/mem or /proc/1/environ (but its own /proc/self/mem: the control), and the app
#    started fewer than 64 processes and threads (at least one Node.js worker, not all 200).
#    Last the app sends PID 1 SIGSTOP and SIGKILL: on Linux PID 1 ignores both; gVisor delivers
#    them (backlog I620-15), and then the whole sandbox must end (137) with nothing written after
#    the SIGSTOP in the log. The same script runs on the build server under
#    runsc (crates/ruum-runtime tests/supervisor_sandbox.rs with RUUM_RUNTIME_DRIVER=runsc).
#    Without --supervisor-image (or RUUM_SUPERVISOR_IMAGE) this step FAILS: never a pass on
#    silence. The digest is the one images.yml published for the deployed commit.
#    --from-release (the platform step, k8s/apply.sh; Kimi, run 17, finding 4): the image is the
#    live release's (RUUM_KUBE_SUPERVISOR_IMAGE of deployment ruum/ruum-server, which
#    ops/deploy-chart.sh resolved with its provenance) when that release runs the Kubernetes
#    driver; when it does not (the driver is off, or no release yet), step 7 prints SKIP, not
#    PASS: no sandbox runs, and README "Turning the Kubernetes runtime driver on" step 7 runs it
#    with --supervisor-image before the switch. A release that cannot be read FAILS.
# Not read-only in steps 2 and 7: they create short pods (two per node in ruum-apps and ops, and
# one in ruum-apps) and delete them. Steps 3, 4, 5 and 6 create nothing (dry runs and reads).
# Exit 1 on any failure.
set -uo pipefail
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export TALOSCONFIG="${TALOSCONFIG:-$WORK/talosconfig}"
IMG=busybox:1.37.0
NS=ruum-apps
# The app image of step 7: sh and Node.js (the thread check), the fixture base of the Docker tests
# (crates/ruum-runtime testdata/fixture), pinned by digest.
NODE_IMG=node:22-alpine@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a
SUPERVISOR_IMAGE="${RUUM_SUPERVISOR_IMAGE:-}"
FROM_RELEASE=0
if [[ "${1:-}" == --supervisor-image ]]; then
  SUPERVISOR_IMAGE="${2:-}"
  set -- "${@:3}"
elif [[ "${1:-}" == --from-release ]]; then
  FROM_RELEASE=1
  set -- "${@:2}"
fi
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

# 6. The kubelet's PID limit of every pod (backlog I6-19).
while IFS='=' read -r name _ _; do
  [[ -n "$name" ]] || continue
  limit="$(kubectl get --raw "/api/v1/nodes/$name/proxy/configz" 2>/dev/null | jq -r '.kubeletconfig.podPidsLimit // empty' 2>/dev/null)"
  if [[ "$limit" == 4096 ]]; then
    pass "$name: the kubelet limits every pod to 4096 PIDs (I6-19)"
  else
    bad "$name: the kubelet's podPidsLimit is '${limit:-unset}', not 4096 (talos/patches/cluster.yaml, applied through the admin tunnel)"
  fi
done <<<"$nodes"

# 7. The sandbox supervisor under gVisor (backlog I620-3).
SUP_POD=gvisor-check-supervisor
SUP_PIDS=64
# The app's script (the same checks as crates/ruum-runtime tests/supervisor_sandbox.rs).
read -r -d '' SUP_SCRIPT <<'SH' || true
echo "canary-out-$S"
echo "canary-err-$S" >&2
echo "raw-$S" > /proc/1/fd/1 2>/dev/null && echo fd1-open || echo fd1-refused
( : < /proc/1/mem ) 2>/dev/null && echo mem-open || echo mem-refused
( : < /proc/self/mem ) 2>/dev/null && echo mem-control-open || echo mem-control-refused
cat /proc/1/environ >/dev/null 2>&1 && echo environ-read || echo environ-refused
node -e '
const { Worker } = require("worker_threads");
let online = 0;
for (let i = 0; i < 200; i++) {
  try {
    const w = new Worker("setTimeout(() => {}, 3000)", { eval: true });
    w.on("online", () => { online++; });
    w.on("error", () => {});
  } catch (e) {}
}
setTimeout(() => {
  console.log("workers=" + online);
  console.log("threads=" + require("fs").readdirSync("/proc/self/task").length);
  process.exit(0);
}, 1500);
' 2>/dev/null || echo threads-failed
( n=0; while [ $n -lt 200 ]; do sleep 2 & n=$((n+1)); echo "started=$n"; done ) 2>/dev/null | tail -n 1
echo done
sleep 1
kill -STOP 1 2>/dev/null
echo "after-stop-$S"
kill -9 1 2>/dev/null
echo "after-kill-$S"
echo signals-ignored
SH
# sup_pod: the driver's sample pod with the checked supervisor and the script's app, no labels of
# the driver (its sweep must not take the pod for one of its own).
sup_pod() {
  jq -c --arg sup "$SUPERVISOR_IMAGE" --arg img "$NODE_IMG" --arg name "$SUP_POD" '
    .metadata = {name: $name, namespace: "ruum-apps", labels: {"app.kubernetes.io/name": "gvisor-check"}}
    | .spec.initContainers[0].image = $sup
    | .spec.containers[0] |= (
        .image = $img
        | .command = ["/ruum/bin/ruum-supervisor", "run", "--launch-stdin", "--token-file", "/var/run/ruum/token"]
        | .stdin = true | .stdinOnce = true
        | del(.readinessProbe, .ports))' "$SAMPLE"
}
# --from-release: the live release's supervisor, or SKIP when it has the driver off.
sup_skip=""
if [[ $FROM_RELEASE == 1 ]]; then
  if ! release="$(kubectl -n ruum get deployment ruum-server --ignore-not-found -o json 2>&1)"; then
    SUPERVISOR_IMAGE=""
    bad "cannot read the live release (deployment ruum/ruum-server): $release"
  else
    live_env() { jq -r --arg n "$1" '[.spec.template.spec.containers[].env[]? | select(.name == $n) | .value] | first // empty' <<<"$release"; }
    if [[ -n "$release" && "$(live_env RUUM_RUNTIME_DRIVER)" == kubernetes ]]; then
      SUPERVISOR_IMAGE="$(live_env RUUM_KUBE_SUPERVISOR_IMAGE)"
    else
      sup_skip="the live release has the Kubernetes runtime driver off"
    fi
  fi
fi
if [[ -n "$sup_skip" ]]; then
  printf 'SKIP %s\n' "the supervisor under gVisor (step 7): $sup_skip; README \"Turning the Kubernetes runtime driver on\" step 7 runs it with --supervisor-image before the switch"
elif [[ ! "$SUPERVISOR_IMAGE" =~ ^ghcr\.io/nemecec/ruum-supervisor@sha256:[0-9a-f]{64}$ ]]; then
  bad "the supervisor protections under gVisor were not checked: give --supervisor-image ghcr.io/nemecec/ruum-supervisor@sha256:<digest> (images.yml, the deployed commit)"
elif ! out="$(sup_pod | kubectl create --dry-run=server -f - -o name 2>&1)"; then
  bad "the supervisor check pod did not pass the admission rules: $out"
else
  pass "the supervisor check pod in the driver's shape passes the admission rules (dry run)"
  secret="gvisor-check-secret-$RANDOM$RANDOM$RANDOM"
  launch="$(jq -cn --arg script "$SUP_SCRIPT" --arg s "$secret" --argjson pids "$SUP_PIDS" \
    '{v: 1, argv: ["/bin/sh", "-c", $script], env: [["S", $s]], redact: [], pids_limit: $pids}')"
  kubectl -n "$NS" delete pod "$SUP_POD" --ignore-not-found --wait=true >/dev/null 2>&1
  logs="" code=""
  # The pod ends Succeeded (Linux semantics: PID 1 ignores the app's SIGKILL) or Failed with 137
  # (gVisor's: the app's SIGKILL ends the sandbox, backlog I620-15).
  if sup_pod | kubectl create -f - >/dev/null 2>&1 &&
    kubectl -n "$NS" wait --for=condition=Ready "pod/$SUP_POD" --timeout=180s >/dev/null 2>&1 &&
    kubectl -n "$NS" attach -i -c app "$SUP_POD" <<<"$launch" >/dev/null 2>&1; then
    for _ in $(seq 1 120); do
      phase="$(kubectl -n "$NS" get pod "$SUP_POD" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
      [[ "$phase" == Succeeded || "$phase" == Failed ]] && break
      sleep 2
    done
    logs="$(kubectl -n "$NS" logs "$SUP_POD" -c app 2>/dev/null | tr -d '\r')"
    code="$(kubectl -n "$NS" get pod "$SUP_POD" -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}' 2>/dev/null || true)"
  fi
  kubectl -n "$NS" delete pod "$SUP_POD" --wait=false >/dev/null 2>&1
  if [[ -z "$logs" ]]; then
    bad "the supervisor check pod did not run to its end under gVisor (kubectl -n $NS get events)"
  else
    [[ "$logs" != *"$secret"* && "$logs" == *"canary-out-<hidden secret>"* && "$logs" == *"canary-err-<hidden secret>"* ]] &&
      pass "the supervisor redacts the app's output before the kubelet's log (the CRI canary)" ||
      bad "the pod's log holds the test value or no redacted canary"
    for line in fd1-refused mem-refused environ-refused; do
      grep -qx "$line" <<<"$logs" && pass "under gVisor the app is refused /proc/1 (${line%-refused})" || bad "under gVisor the app was not refused /proc/1 (${line%-refused})"
    done
    # /proc/1/mem is only opened; the app's own /proc/self/mem is the positive control (Kimi,
    # run 17, finding 6).
    grep -qx mem-control-open <<<"$logs" && pass "the app can open its own /proc/self/mem (the control of the /proc/1/mem check)" || bad "the app could not open its own /proc/self/mem: the /proc/1/mem check proves nothing"
    grep -qx "done" <<<"$logs" && pass "the app's checks ran to their end" || bad "the app's checks did not run to their end"
    # The app's SIGSTOP and SIGKILL to PID 1, last (backlog I620-15).
    if grep -qx signals-ignored <<<"$logs"; then
      [[ "$code" == 0 && "$logs" == *"after-kill-<hidden secret>"* ]] &&
        pass "the supervisor (PID 1) ignored the app's SIGSTOP and SIGKILL and went on redacting" || bad "PID 1 ignored the signals but the run did not end cleanly (exit ${code:-none})"
    elif [[ "$code" == 137 && "$logs" != *"after-stop-"* && "$logs" != *"after-kill-"* ]]; then
      pass "the app's SIGKILL to PID 1 ended the whole sandbox, and nothing it wrote after its SIGSTOP reached the log (gVisor semantics, backlog I620-15)"
    else
      bad "after the app's SIGSTOP and SIGKILL to PID 1 the run neither went on nor ended with its supervisor (exit ${code:-none})"
    fi
    started="$(sed -n 's/^started=//p' <<<"$logs")"
    [[ "$started" =~ ^[0-9]+$ && "$started" -lt $SUP_PIDS ]] &&
      pass "under gVisor RLIMIT_NPROC bounds the app's processes ($started started of 200 tried, limit $SUP_PIDS)" || bad "the fork check: '${started:-none}' processes started under RLIMIT_NPROC $SUP_PIDS"
    # Node.js ran, at least one worker started, and the limit refused the rest of the 200 (Kimi,
    # run 17, finding 6: no pass when Node.js did not run).
    workers="$(sed -n 's/^workers=//p' <<<"$logs")"
    threads="$(sed -n 's/^threads=//p' <<<"$logs")"
    if [[ "$workers" =~ ^[0-9]+$ && "$workers" -ge 1 && "$workers" -lt 200 && "$threads" =~ ^[0-9]+$ && "$threads" -le $SUP_PIDS ]]; then
      pass "under gVisor RLIMIT_NPROC bounds the app's threads ($workers of 200 workers started, $threads threads)"
    else
      bad "the thread check: '${workers:-none}' workers and '${threads:-none}' threads under RLIMIT_NPROC $SUP_PIDS"
    fi
  fi
fi

[[ $fail == 0 ]] && echo "gvisor check: passed" || echo "gvisor check: FAILED"
exit "$fail"
