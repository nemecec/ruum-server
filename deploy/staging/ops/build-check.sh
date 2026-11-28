#!/usr/bin/env bash
# Live checks of the build pods' namespace (card I6-13), as the admin through the admin tunnel:
#
#   deploy/staging/tool.sh bash deploy/staging/ops/build-check.sh
#
# Server-side dry runs only (nothing is made): a pod in the shape of the Kubernetes builder's
# build pod (crates/ruum-runtime src/builder/kube.rs pod_body) passes the admission rules of
# ruum-builds (k8s/manifests/builds-admission.yaml), and each variant that breaks a rule is
# refused: no gVisor, a token, another account, a Secret volume, an init container, an image that
# is not the node22 base by digest from the cluster registry, a value from a Secret; and a Secret
# in ruum-builds is refused. Prints PASS or FAIL per line; exit 1 on a FAIL.
set -uo pipefail
NS=ruum-builds
IMAGE="registry.ruum.internal:5000/ruum-base/node22@sha256:$(printf '%064d' 0)"
failed=0
pass() { printf 'PASS %s\n' "$*"; }
fail() { printf 'FAIL %s\n' "$*"; failed=1; }

# The build pod's shape (as pod_body makes it), with a jq filter applied.
pod() {
  jq -n --arg image "$IMAGE" '{
    apiVersion: "v1", kind: "Pod",
    metadata: { name: "ruum-b-check", namespace: "ruum-builds", labels: { "ruum.managed": "ruum-builder" } },
    spec: {
      runtimeClassName: "gvisor", serviceAccountName: "ruum-build", automountServiceAccountToken: false,
      enableServiceLinks: false, restartPolicy: "Never", activeDeadlineSeconds: 600,
      dnsPolicy: "None", dnsConfig: { nameservers: ["127.0.0.1"] },
      securityContext: { runAsNonRoot: true, runAsUser: 10001, runAsGroup: 10001, fsGroup: 10001, seccompProfile: { type: "RuntimeDefault" } },
      containers: [{
        name: "build", image: $image, imagePullPolicy: "IfNotPresent", command: ["node", "-e", "0"],
        env: [{ name: "HOME", value: "/tmp" }],
        resources: { limits: { cpu: "1000m", memory: "1Gi" }, requests: { cpu: "1000m", memory: "1Gi" } },
        securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, runAsNonRoot: true,
          runAsUser: 10001, runAsGroup: 10001, capabilities: { drop: ["ALL"] }, seccompProfile: { type: "RuntimeDefault" } },
        volumeMounts: [{ name: "work", mountPath: "/work" }, { name: "tmp", mountPath: "/tmp" }]
      }],
      volumes: [{ name: "work", emptyDir: { sizeLimit: "1Gi" } }, { name: "tmp", emptyDir: { sizeLimit: "1Gi" } }]
    } }' | jq "$1"
}

dry() { kubectl apply --dry-run=server -f - >/dev/null 2>"${TMPDIR:-/tmp}/build-check.err"; }

if pod . | dry; then
  pass "a pod in the shape of a build pod passes the admission rules of $NS (dry run; the positive control)"
else
  fail "the build pod's shape is refused in $NS: $(head -c 300 "${TMPDIR:-/tmp}/build-check.err")"
fi
while IFS='|' read -r what filter; do
  if pod "$filter" | dry; then
    fail "$NS admits a pod with $what"
  else
    pass "$NS refuses a pod with $what"
  fi
done <<'CASES'
no runtime class gvisor|del(.spec.runtimeClassName)
the runtime class runc|.spec.runtimeClassName = "runc"
an automounted token|.spec.automountServiceAccountToken = true
another ServiceAccount|.spec.serviceAccountName = "default"
a Secret volume|.spec.volumes += [{ name: "s", secret: { secretName: "x" } }]
a projected token|.spec.volumes += [{ name: "p", projected: { sources: [{ serviceAccountToken: { path: "t" } }] } }]
an init container|.spec.initContainers = [.spec.containers[0] | .name = "init"]
a second container|.spec.containers += [.spec.containers[0] | .name = "two"]
an image by tag|.spec.containers[0].image = "registry.ruum.internal:5000/ruum-base/node22:latest"
an image of another registry|.spec.containers[0].image = "docker.io/library/node@sha256:0000000000000000000000000000000000000000000000000000000000000000"
an app image|.spec.containers[0].image = "registry.ruum.internal:5000/ruum/t/a@sha256:0000000000000000000000000000000000000000000000000000000000000000"
a value from a Secret|.spec.containers[0].env += [{ name: "X", valueFrom: { secretKeyRef: { name: "x", key: "y" } } }]
envFrom|.spec.containers[0].envFrom = [{ configMapRef: { name: "x" } }]
the host network|.spec.hostNetwork = true
CASES
if kubectl -n "$NS" create secret generic ruum-check --from-literal=a=b --dry-run=server >/dev/null 2>&1; then
  fail "$NS admits a Secret"
else
  pass "$NS refuses a Secret (a build has none)"
fi
[[ $failed == 0 ]] && echo "build-check: the admission rules of $NS hold" || echo "build-check: FAILED"
exit "$failed"
