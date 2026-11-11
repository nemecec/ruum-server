#!/usr/bin/env bash
# Bring-up check of the backups (code review F-7), on the build server as ruum-ops with the admin
# kubeconfig (remote.sh check-backup; up.sh --apply runs it after the platform step).
#
#   KUBECONFIG=... ops/check-backup.sh [--wal-only]
#
# The backup bucket has a COMPLIANCE object lock. S3 object lock needs a Content-MD5 or checksum
# header on every PUT, and new boto3 checksum defaults have broken S3 services other than AWS
# before. So this check proves, before anything depends on the backups:
#   1. the CNPG cluster pg reports ContinuousArchiving=True;
#   2. a forced WAL switch is archived: pg_stat_archiver.last_archived_wal reaches the new WAL
#      file, with no new failure;
#   3. (unless --wal-only) an on-demand base backup through the Barman Cloud plugin completes.
# It changes nothing but one WAL switch and one Backup object (kept, as a normal backup).
set -euo pipefail
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'check-backup: %s\n' "$*" >&2; exit 1; }
wal_only=0
[[ "${1:-}" == --wal-only ]] && wal_only=1

k() { kubectl -n data "$@"; }
k wait --for=condition=Ready cluster/pg --timeout=10m >/dev/null || die "the CNPG cluster pg is not Ready"
primary="$(k get cluster pg -o jsonpath='{.status.currentPrimary}')"
[[ -n "$primary" ]] || die "no primary in the status of cluster pg"
psql_q() { k exec "$primary" -c postgres -- psql -U postgres -d postgres -tAX -c "$1"; }

failed_before="$(psql_q 'select failed_count from pg_stat_archiver')"
wal="$(psql_q 'select pg_walfile_name(pg_switch_wal())')"
[[ "$wal" =~ ^[0-9A-F]{24}$ ]] || die "unexpected WAL file name from pg_switch_wal: $wal"
log "forced a WAL switch on $primary: waiting for $wal to be archived"
archived=""
for _ in $(seq 1 60); do
  IFS='|' read -r archived failed last_failed < <(psql_q 'select coalesce(last_archived_wal, $$$$), failed_count, coalesce(last_failed_wal, $$$$) from pg_stat_archiver')
  if [[ -n "$archived" && ! "$archived" < "${wal:0:24}" ]]; then break; fi
  if [[ "$failed" -gt "$failed_before" ]]; then
    die "WAL archiving failed (last failed WAL $last_failed); check the barman-cloud plugin log, the backup writer key and the object lock (Content-MD5)"
  fi
  sleep 5
done
[[ -n "$archived" && ! "$archived" < "${wal:0:24}" ]] || die "$wal was not archived in 5 minutes (last archived: ${archived:-none})"
log "WAL push works: last archived $archived"

cond="$(k get cluster pg -o jsonpath='{.status.conditions[?(@.type=="ContinuousArchiving")].status}')"
[[ "$cond" == True ]] || die "cluster pg reports ContinuousArchiving=${cond:-unknown}"
log "cluster pg: ContinuousArchiving=True"

[[ $wal_only == 0 ]] || exit 0
name="bringup-check-$(date -u +%Y%m%d%H%M%S)"
k create -f - >/dev/null <<YAML
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
log "on-demand base backup $name"
phase=""
for _ in $(seq 1 120); do
  phase="$(k get backup "$name" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
  [[ "$phase" == completed ]] && break
  [[ "$phase" == failed ]] && die "base backup $name failed: $(k get backup "$name" -o jsonpath='{.status.error}')"
  sleep 10
done
[[ "$phase" == completed ]] || die "base backup $name did not finish in 20 minutes (phase ${phase:-none})"
log "base backup $name completed: backups into the locked bucket work"
