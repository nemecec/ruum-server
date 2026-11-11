#!/usr/bin/env bash
# Rolls out charts/ruum by digest, on the build server as ruum-ops with the namespaced deployer
# kubeconfig (review 2 O-1). Called by remote.sh deploy; deploy.sh (laptop) has checked CI,
# the digests and the provenance first.
#
#   deploy-chart.sh <commit> <server-ref@sha256:...> <edge-ref@sha256:...> [--backup] [--acme ISSUER] [--acme-email EMAIL]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
WORK="${RUUM_STAGING_WORK:-$HOME/.ruum-staging}"
export KUBECONFIG="$WORK/deployer.kubeconfig"
# Helm keeps release records in ConfigMaps, so the deployer needs no access to Secrets.
export HELM_DRIVER=configmap
commit="${1:?commit}" server="${2:?server image by digest}" edge="${3:?edge image by digest}"
shift 3
backup=0
extra=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --backup) backup=1; shift ;;
    --acme) extra+=(--set "acme.issuer=$2"); shift 2 ;;
    --acme-email) extra+=(--set "acme.email=$2"); shift 2 ;;
    *) echo "deploy-chart: unknown argument $1" >&2; exit 1 ;;
  esac
done
re='^ghcr\.io/nemecec/ruum-(server|edge)@sha256:[a-f0-9]{64}$'
[[ "$server" =~ $re && "$edge" =~ $re ]] || { echo "deploy-chart: images must be ghcr.io/nemecec/ruum-*@sha256:<digest>" >&2; exit 1; }

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
    [[ "$phase" == failed ]] && { echo "deploy-chart: the backup failed; not deploying" >&2; exit 1; }
    sleep 10
  done
  [[ "$phase" == completed ]] || { echo "deploy-chart: the backup did not finish in 20 minutes; not deploying" >&2; exit 1; }
fi

# --rollback-on-failure is Helm 4's name for --atomic (review 2 U-4): a failed rollout rolls
# the release back by itself (DESIGN 19.1 layer 1).
helm upgrade --install ruum "$STG/charts/ruum" --namespace ruum \
  --reset-then-reuse-values \
  --set "images.server=$server" --set "images.edge=$edge" --set "commit=$commit" \
  "${extra[@]}" \
  --rollback-on-failure --wait --timeout 10m
kubectl -n ruum get configmap ruum-release -o jsonpath='{.data.commit}{"\n"}'
