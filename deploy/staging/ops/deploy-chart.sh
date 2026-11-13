#!/usr/bin/env bash
# Rolls out charts/ruum by digest. Since backlog O-34 (decision 0008-2) it runs only in the deploy
# controller (namespace ruum-deploy, deploy/images/deploy-controller), with the controller's own
# ServiceAccount. The deployer no longer runs it: it may only write a release request
# (ops/release-request.sh), and the controller checks that request itself (provenance, main,
# floor, order against the live release) before it runs this script. This script checks again
# (code review F-3), so a call that skips the controller's checks cannot deploy another image:
#   - every argument is validated: the commit is 40 hex characters, the images are
#     ghcr.io/nemecec/ruum-{server,edge}@sha256:<64 hex>, the issuer is from a fixed list, the
#     email matches a strict pattern;
#   - the tag <commit> of each image resolves to exactly that digest;
#   - each digest has a provenance attestation of images.yml on refs/heads/main (cosign);
#   - the values go to helm in a values file (JSON), never through --set, so no value can add
#     another key (helm splits --set on commas).
# Then (design 12.7, backlog O-25), holding the deploy lock (Lease ruum/ruum-deploy,
# ops/deploy-lock.sh; Kimi review of O-25, finding 3) from step 0 to the end of step 3:
#   0. the live release (ConfigMap ruum-release) must still be the one deploy.sh checked
#      (--expect-deployed), so no other deploy changed it in between; a rollback (--rollback)
#      must go to the previous release, unless --rollback-older (ops/release-floor.sh);
#   1. with new migrations (--backup), an on-demand CNPG backup;
#   2. the migration step: the Job of charts/ruum migrate-job.yaml with the new server image
#      runs `ruum-server migrate` under an advisory lock; when it fails, the rollout does not
#      start and the old release keeps serving (old and new releases both run on the new
#      schema: expand, then contract). Its log lines start with "migrate| ": the controller
#      keeps them in its own log (admin only) and out of the request that the deployer reads;
#   3. the rolling update (helm upgrade --wait): new pods next to the old ones, never fewer
#      ready than before.
#
#   deploy-chart.sh <commit> <server-ref@sha256:...> <edge-ref@sha256:...> --expect-deployed <commit|none>
#                   [--rollback | --rollback-older] [--backup] [--acme ISSUER] [--acme-email EMAIL] [--validate-only]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
# shellcheck source=image-check.sh
. "$HERE/image-check.sh"
# shellcheck source=release-floor.sh
. "$HERE/release-floor.sh"
# shellcheck source=deploy-lock.sh
. "$HERE/deploy-lock.sh"
die() { echo "deploy-chart: $*" >&2; exit 1; }

[[ $# -ge 3 ]] || die "usage: deploy-chart.sh <commit> <server-ref> <edge-ref> --expect-deployed <commit|none> [--rollback | --rollback-older] [--backup] [--acme ISSUER] [--acme-email EMAIL]"
commit="$1" server="$2" edge="$3"
shift 3
backup=0
rollback=0
expect_deployed=""
validate_only=0
acme="" acme_email=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --backup) backup=1; shift ;;
    --rollback) [[ $rollback == 0 ]] || die "--rollback and --rollback-older exclude each other"; rollback=1; shift ;;
    --rollback-older) [[ $rollback == 0 ]] || die "--rollback and --rollback-older exclude each other"; rollback=2; shift ;;
    --expect-deployed) [[ $# -ge 2 ]] || die "--expect-deployed needs a value"; expect_deployed="$2"; shift 2 ;;
    --acme) [[ $# -ge 2 ]] || die "--acme needs a value"; acme="$2"; shift 2 ;;
    --acme-email) [[ $# -ge 2 ]] || die "--acme-email needs a value"; acme_email="$2"; shift 2 ;;
    --validate-only) validate_only=1; shift ;;
    *) die "unknown argument $1" ;;
  esac
done
[[ "$commit" =~ $RUUM_COMMIT_RE ]] || die "the commit must be 40 lowercase hex characters"
[[ "$server" =~ $RUUM_IMAGE_RE && "$server" == ghcr.io/nemecec/ruum-server@* ]] || die "the server image must be ghcr.io/nemecec/ruum-server@sha256:<digest>"
[[ "$edge" =~ $RUUM_IMAGE_RE && "$edge" == ghcr.io/nemecec/ruum-edge@* ]] || die "the edge image must be ghcr.io/nemecec/ruum-edge@sha256:<digest>"
if [[ -n "$acme" ]]; then
  ruum_acme_issuer_ok "$acme" || die "--acme must be one of: ${RUUM_ACME_ISSUERS[*]}"
fi
if [[ -n "$acme_email" ]]; then
  [[ "$acme_email" =~ $RUUM_EMAIL_RE ]] || die "--acme-email is not a plain email address"
fi
[[ "$acme" != zerossl || -n "$acme_email" ]] || die "--acme zerossl needs --acme-email"
[[ "$expect_deployed" == none || "$expect_deployed" =~ $RUUM_COMMIT_RE ]] ||
  die "--expect-deployed must be the live commit that deploy.sh read (40 lowercase hex characters) or none"
[[ $rollback == 0 || "$expect_deployed" != none ]] || die "a rollback needs a live release (--expect-deployed)"
[[ $validate_only == 0 ]] || { echo "deploy-chart: arguments are valid"; exit 0; }

# Only in the deploy controller's pod (backlog O-34): kubectl and helm use its ServiceAccount.
[[ "${RUUM_DEPLOY_IN_CLUSTER:-}" == 1 && -n "${KUBERNETES_SERVICE_HOST:-}" ]] ||
  die "deploy-chart.sh runs in the deploy controller (namespace ruum-deploy); deploy.sh writes a release request for it (backlog O-34)"
unset KUBECONFIG
WORK="${RUUM_STAGING_WORK:-/tmp/work}"
mkdir -p "$WORK"
# Helm keeps release records in ConfigMaps, so the controller needs no access to Secrets.
export HELM_DRIVER=configmap

echo "==> provenance of both images (images.yml on refs/heads/main), checked with cosign" >&2
for ref in "$server" "$edge"; do
  name="${ref%%@*}"; name="${name#ghcr.io/}"
  tagged="$(ghcr_digest "$name" "$commit" || true)"
  [[ "$tagged" == "${ref#*@}" ]] || die "ghcr.io/$name:$commit is not ${ref#*@} (got ${tagged:-nothing})"
  cosign_verify_provenance "$ref" || die "no valid provenance attestation for $ref"
done

tmp="$(mktemp -d "$WORK/deploy.XXXXXX")"
holder="deploy-${commit:0:12}-$(date -u +%Y%m%dT%H%M%SZ)-$$-$RANDOM"
locked=0
cleanup() {
  [[ $locked == 0 ]] || ruum_lock_release "$holder"
  rm -rf "$tmp"
}
trap cleanup EXIT

# 0. The deploy lock, then the live release again (Kimi review of O-25, findings 2 and 3).
kubectl auth can-i update leases.coordination.k8s.io/ruum-deploy -n ruum >/dev/null 2>&1 ||
  die "the deploy controller may not use Leases in ruum; apply the platform first (k8s/apply.sh)"
rc=0
ruum_lock_acquire "$holder" || rc=$?
[[ $rc == 0 ]] || die "not deploying: $([[ $rc == 1 ]] && echo "another deploy runs (wait for it, or for the lease to expire)" || echo "the deploy lock failed")"
locked=1
echo "==> deploy lock ruum/$RUUM_DEPLOY_LOCK_NAME held by $holder" >&2
live="$(kubectl -n ruum get configmap ruum-release -o jsonpath='{.data.commit}' 2>/dev/null || true)"
previous="$(kubectl -n ruum get configmap ruum-release -o jsonpath='{.data.previous}' 2>/dev/null || true)"
[[ "${live:-none}" == "$expect_deployed" ]] ||
  die "the live release is ${live:-none}, not $expect_deployed as deploy.sh checked: another deploy ran in between; run deploy.sh again"
if [[ $rollback != 0 ]]; then
  ruum_rollback_target_ok "$commit" "$previous" "$([[ $rollback == 2 ]] && echo 1 || echo 0)" ||
    die "$commit is not the previous release (${previous:-none recorded}); a plain --rollback goes back one release only (design 12.7). Use --rollback-older on purpose"
  [[ $rollback != 2 || "$commit" == "$previous" ]] || echo "deploy-chart: $RUUM_ROLLBACK_OLDER_WARNING" >&2
fi
# The release before this one, recorded in ruum-release (a redeploy of the live commit keeps it).
if [[ -n "$live" && "$live" != "$commit" ]]; then previous_value="$live"; else previous_value="$previous"; fi
[[ -z "$previous_value" || "$previous_value" =~ $RUUM_COMMIT_RE ]] || previous_value=""

if [[ $backup == 1 ]]; then
  # Review 2 U-3: a release with new migrations gets an on-demand backup first.
  name="pre-deploy-${commit:0:12}-$(date -u +%Y%m%d%H%M)"
  kubectl -n data create -f - >/dev/null <<YAML
apiVersion: postgresql.cnpg.io/v1
kind: Backup
metadata:
  name: $name
  namespace: data
spec:
  cluster: { name: pg }
  method: plugin
  pluginConfiguration:
    name: barman-cloud.cloudnative-pg.io
YAML
  echo "==> on-demand backup $name (new migrations in this release)" >&2
  for _ in $(seq 1 120); do
    phase="$(kubectl -n data get backup "$name" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
    [[ "$phase" == completed ]] && break
    [[ "$phase" == failed ]] && die "the backup failed; not deploying"
    sleep 10
  done
  [[ "$phase" == completed ]] || die "the backup did not finish in 20 minutes; not deploying"
fi
ruum_lock_renew "$holder" || die "lost the deploy lock after the backup; not migrating"

# 2. The migration step (design 12.7, backlog O-25), before the rollout.
kubectl auth can-i create jobs.batch -n ruum >/dev/null 2>&1 ||
  die "the deploy controller may not create Jobs in ruum; apply the platform first (k8s/apply.sh, backlog O-34)"
job="ruum-migrate-${commit:0:12}-$(date -u +%Y%m%d%H%M)"
jq -n --arg server "$server" --arg edge "$edge" --arg commit "$commit" --arg job "$job" '
  { images: { server: $server, edge: $edge }, commit: $commit, migrationJob: true, migrationJobName: $job }
' >"$tmp/job-values.json"
helm template ruum "$STG/charts/ruum" --namespace ruum --values "$tmp/job-values.json" \
  --show-only templates/migrate-job.yaml >"$tmp/job.yaml"
echo "==> migration step $job (ruum-server migrate, image ${server#*@})" >&2
kubectl -n ruum create -f "$tmp/job.yaml" >/dev/null
state=""
for _ in $(seq 1 200); do
  state="$(kubectl -n ruum get job "$job" -o jsonpath='{.status.succeeded}/{.status.failed}' 2>/dev/null || true)"
  [[ "$state" == 1/* || "$state" == */1 ]] && break
  # A missing Secret (ruum-migrate-db before the platform step) never fails the pod; it waits.
  waiting="$(kubectl -n ruum get pods -l "job-name=$job" -o jsonpath='{.items[*].status.containerStatuses[*].state.waiting.reason}' 2>/dev/null || true)"
  if [[ "$waiting" == *CreateContainerConfigError* ]]; then
    kubectl -n ruum delete job "$job" --wait=false >/dev/null 2>&1 || true
    die "the migration step $job cannot start (CreateContainerConfigError: is the Secret ruum/ruum-migrate-db there? Run the platform step, k8s/apply.sh); not rolling out, the old release keeps serving"
  fi
  sleep 5
done
kubectl -n ruum logs "job/$job" --tail 50 2>&1 | sed 's/^/migrate| /' >&2 || true
[[ "$state" == 1/* ]] || die "the migration step $job did not succeed (${state:-no status}); not rolling out, the old release keeps serving"
echo "==> migrations are up to date; rolling update" >&2
ruum_lock_renew "$holder" || die "lost the deploy lock after the migration step; not rolling out (run deploy.sh again)"

# 3. The values as JSON (valid YAML), built by jq from validated strings.
values="$tmp/values.json"
jq -n --arg server "$server" --arg edge "$edge" --arg commit "$commit" --arg previous "$previous_value" --arg acme "$acme" --arg email "$acme_email" '
  { images: { server: $server, edge: $edge }, commit: $commit, previousCommit: $previous }
  + (if $acme != "" then { acme: ({ issuer: $acme } + (if $email != "" then { email: $email } else {} end)) } else {} end)
' >"$values"

# --rollback-on-failure is Helm 4's name for --atomic (review 2 U-4): a failed rollout rolls
# the release back by itself (DESIGN 19.1 layer 1). The schema stays: the previous release
# runs on it (design 12.7). --wait waits for the rolling update of ruum-server and the edge.
helm upgrade --install ruum "$STG/charts/ruum" --namespace ruum \
  --reset-then-reuse-values \
  --values "$values" \
  --rollback-on-failure --wait --timeout 10m
kubectl -n ruum get configmap ruum-release -o jsonpath='{.data.commit}{"\n"}'
