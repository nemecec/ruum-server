#!/usr/bin/env bash
# Rolls out charts/ruum by digest, in the tools container on the Mac with the namespaced deployer
# kubeconfig (review 2 O-1). Called by steps.sh deploy. deploy.sh (Mac) checks CI, the digests
# and the provenance first with gh; this script checks again here with cosign (code review F-3),
# so a call of steps.sh deploy that skips deploy.sh cannot deploy another image:
#   - every argument is validated: the commit is 40 hex characters, the images are
#     ghcr.io/nemecec/ruum-{server,edge}@sha256:<64 hex>, the issuer is from a fixed list, the
#     email matches a strict pattern;
#   - the tag <commit> of each image resolves to exactly that digest;
#   - each digest has a provenance attestation of images.yml on refs/heads/main (cosign);
#   - the values go to helm in a values file (JSON), never through --set, so no value can add
#     another key (helm splits --set on commas).
# Then (design 12.7, backlog O-25):
#   1. with new migrations (--backup), an on-demand CNPG backup;
#   2. the migration step: the Job of charts/ruum migrate-job.yaml with the new server image
#      runs `ruum-server migrate` under an advisory lock; when it fails, the rollout does not
#      start and the old release keeps serving (old and new releases both run on the new
#      schema: expand, then contract);
#   3. the rolling update (helm upgrade --wait): new pods next to the old ones, never fewer
#      ready than before.
#
#   deploy-chart.sh <commit> <server-ref@sha256:...> <edge-ref@sha256:...> [--backup] [--acme ISSUER] [--acme-email EMAIL] [--validate-only]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
# shellcheck source=image-check.sh
. "$HERE/image-check.sh"
die() { echo "deploy-chart: $*" >&2; exit 1; }

[[ $# -ge 3 ]] || die "usage: deploy-chart.sh <commit> <server-ref> <edge-ref> [--backup] [--acme ISSUER] [--acme-email EMAIL]"
commit="$1" server="$2" edge="$3"
shift 3
backup=0
validate_only=0
acme="" acme_email=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --backup) backup=1; shift ;;
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
[[ $validate_only == 0 ]] || { echo "deploy-chart: arguments are valid"; exit 0; }

WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export KUBECONFIG="$WORK/deployer.kubeconfig"
# Helm keeps release records in ConfigMaps, so the deployer needs no access to Secrets.
export HELM_DRIVER=configmap

echo "==> provenance of both images (images.yml on refs/heads/main), checked with cosign" >&2
for ref in "$server" "$edge"; do
  name="${ref%%@*}"; name="${name#ghcr.io/}"
  tagged="$(ghcr_digest "$name" "$commit" || true)"
  [[ "$tagged" == "${ref#*@}" ]] || die "ghcr.io/$name:$commit is not ${ref#*@} (got ${tagged:-nothing})"
  cosign_verify_provenance "$ref" || die "no valid provenance attestation for $ref"
done

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

tmp="$(mktemp -d "$WORK/deploy.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# 2. The migration step (design 12.7, backlog O-25), before the rollout.
kubectl auth can-i create jobs.batch -n ruum >/dev/null 2>&1 ||
  die "the deployer may not create Jobs in ruum; apply the platform first (k8s/apply.sh, backlog O-25)"
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
  sleep 5
done
kubectl -n ruum logs "job/$job" --tail 50 >&2 || true
[[ "$state" == 1/* ]] || die "the migration step $job did not succeed (${state:-no status}); not rolling out, the old release keeps serving"
echo "==> migrations are up to date; rolling update" >&2

# 3. The values as JSON (valid YAML), built by jq from validated strings.
values="$tmp/values.json"
jq -n --arg server "$server" --arg edge "$edge" --arg commit "$commit" --arg acme "$acme" --arg email "$acme_email" '
  { images: { server: $server, edge: $edge }, commit: $commit }
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
