#!/usr/bin/env bash
# Monthly restore test (DESIGN 5.2: an untested backup is not a backup). On the Mac, in the tools
# container (deploy/staging/tool.sh bash deploy/staging/ops/restore-test.sh). It bootstraps a temporary CNPG cluster pg-restore from the backup path of the current
# generation, compares row counts with the live cluster, and deletes pg-restore. About 0.8 GB of
# memory for a few minutes; one instance.
#
# The full disaster-recovery rehearsal is `up.sh --apply --restore-from <generation>` on a new
# cluster: it starts Zitadel on the restored database with the escrowed master key (review 1 S-6).
set -euo pipefail
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export KUBECONFIG="$WORK/kubeconfig"
log() { printf '==> %s\n' "$*" >&2; }
gen="$(kubectl -n data get cluster pg -o jsonpath='{.spec.plugins[0].parameters.serverName}')"
image="$(kubectl -n data get cluster pg -o jsonpath='{.spec.imageName}')"
log "restoring $gen into data/pg-restore"
kubectl apply -f - >/dev/null <<YAML
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: pg-restore
  namespace: data
spec:
  instances: 1
  imageName: $image
  enableSuperuserAccess: false
  storage: { size: 10Gi, storageClass: local-path }
  resources:
    requests: { cpu: 50m, memory: 384Mi }
    limits: { memory: 768Mi }
  bootstrap:
    recovery:
      source: origin
      database: ruum
      owner: ruum
  externalClusters:
    - name: origin
      plugin:
        name: barman-cloud.cloudnative-pg.io
        parameters:
          barmanObjectName: pg-backup
          serverName: $gen
YAML
trap 'kubectl -n data delete cluster pg-restore --wait=false >/dev/null 2>&1 || true' EXIT
kubectl -n data wait --for=condition=Ready cluster/pg-restore --timeout=30m >/dev/null
q="select coalesce(string_agg(relname || '=' || n_live_tup, ' ' order by relname), '') from pg_stat_user_tables"
live_primary="$(kubectl -n data get cluster pg -o jsonpath='{.status.currentPrimary}')"
restored="$(kubectl -n data get pods -l cnpg.io/cluster=pg-restore -o jsonpath='{.items[0].metadata.name}')"
users() { kubectl -n data exec "$1" -c postgres -- psql -At -d ruum -c "select count(*) from users"; }
echo "users: live $(users "$live_primary"), restored $(users "$restored")"
kubectl -n data exec "$restored" -c postgres -- psql -At -d ruum -c "$q" | tr ' ' '\n' | head -40
log "restore test passed (pg-restore is deleted now)"
