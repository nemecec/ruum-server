#!/usr/bin/env bash
# The platform of staging, in the tools container on the Mac (steps.sh platform). The cluster
# secrets are in the environment (from secrets.env over the container's stdin); they become one Kubernetes
# Secret per consumer and namespace (review 1 S-3), through `kubectl apply -f -` on stdin, never
# a file or an argument.
#
#   KUBECONFIG=... k8s/apply.sh <generation> [<restore-from-generation>]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
# shellcheck source=../versions.env
set -a
. "$STG/versions.env"
set +a
generation="${1:?generation (tofu output pg_generation)}"
restore_from="${2:-}"
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
k() { kubectl "$@"; }

for n in SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY \
  STG_ZITADEL_MASTERKEY STG_PG_RUUM_PASSWORD STG_PG_ZITADEL_PASSWORD STG_PG_SPICEDB_PASSWORD \
  STG_SPICEDB_PRESHARED_KEY STG_RUUM_TOKEN_SIGNING_KEY STG_RUUM_CONTENT_SIGNING_KEY \
  STG_RCLONE_CRYPT_PASSWORD STG_RCLONE_CRYPT_SALT STG_NATS_RUUM_PASSWORD STG_ALLOWED_EMAILS; do
  [[ -n "${!n:-}" ]] || die "$n is missing (secrets-push.sh sends it)"
done

# secret NAMESPACE NAME [TYPE]: KEY=value lines on stdin -> a Secret, applied without a file.
secret() {
  local ns="$1" name="$2" type="${3:-generic}"
  k create secret "$type" "$name" -n "$ns" --from-env-file=/dev/stdin --dry-run=client -o yaml |
    k apply -f - >/dev/null
  log "secret $ns/$name"
}

log "waiting for the nodes"
k wait --for=condition=Ready node --all --timeout=15m >/dev/null

log "namespaces, network policy engine, local-path storage"
k apply -f "$HERE/manifests/namespaces.yaml" >/dev/null
k apply -f "$HERE/manifests/kube-network-policies.yaml" >/dev/null
k apply -f "$HERE/manifests/local-path-storage.yaml" >/dev/null
k -n kube-system rollout status ds/kube-network-policies --timeout=5m >/dev/null

log "secrets, one per consumer"
# CNPG: the backup writer key only (object read/write, no delete; review 1 B-3).
printf 'ACCESS_KEY_ID=%s\nACCESS_SECRET_KEY=%s\n' "$SCW_BACKUP_ACCESS_KEY" "$SCW_BACKUP_SECRET_KEY" | secret data pg-backup-s3
for role in ruum zitadel spicedb; do
  var="STG_PG_${role^^}_PASSWORD"
  printf 'username=%s\npassword=%s\n' "$role" "${!var}" |
    k create secret generic "pg-role-$role" -n data --type=kubernetes.io/basic-auth --from-env-file=/dev/stdin --dry-run=client -o yaml |
    k label --local -f - cnpg.io/reload=true -o yaml | k apply -f - >/dev/null
  log "secret data/pg-role-$role"
done
printf 'masterkey=%s\n' "$STG_ZITADEL_MASTERKEY" | secret auth zitadel-masterkey
printf 'dsn=postgres://zitadel:%s@pg-rw.data.svc.cluster.local:5432/zitadel?sslmode=verify-full&sslrootcert=/db-ssl-ca-crt/ca.crt\n' \
  "$STG_PG_ZITADEL_PASSWORD" | secret auth zitadel-db
printf 'preshared-key=%s\ndatastore-uri=postgres://spicedb:%s@pg-rw.data.svc.cluster.local:5432/spicedb?sslmode=verify-full&sslrootcert=/pg-ca/ca.crt\n' \
  "$STG_SPICEDB_PRESHARED_KEY" "$STG_PG_SPICEDB_PASSWORD" | secret authz spicedb
printf 'ruum-password=%s\n' "$STG_NATS_RUUM_PASSWORD" | secret nats nats-auth
{
  printf 'HETZNER_ACCESS_KEY_ID=%s\nHETZNER_SECRET_ACCESS_KEY=%s\n' "$HETZNER_S3_ACCESS_KEY" "$HETZNER_S3_SECRET_KEY"
  printf 'SCW_ACCESS_KEY_ID=%s\nSCW_SECRET_ACCESS_KEY=%s\n' "$SCW_BACKUP_ACCESS_KEY" "$SCW_BACKUP_SECRET_KEY"
  printf 'CRYPT_PASSWORD=%s\nCRYPT_SALT=%s\n' "$STG_RCLONE_CRYPT_PASSWORD" "$STG_RCLONE_CRYPT_SALT"
} | secret ops blob-copy
{
  printf 'DATABASE_URL=postgres://ruum:%s@pg-rw.data.svc.cluster.local:5432/ruum?sslmode=verify-full&sslrootcert=/pg-ca/ca.crt\n' "$STG_PG_RUUM_PASSWORD"
  printf 'RUUM_S3_ACCESS_KEY_ID=%s\nRUUM_S3_SECRET_ACCESS_KEY=%s\n' "$HETZNER_S3_ACCESS_KEY" "$HETZNER_S3_SECRET_KEY"
  printf 'RUUM_SPICEDB_PRESHARED_KEY=%s\n' "$STG_SPICEDB_PRESHARED_KEY"
  printf 'RUUM_TOKEN_SIGNING_KEY=%s\nRUUM_CONTENT_SIGNING_KEY=%s\n' "$STG_RUUM_TOKEN_SIGNING_KEY" "$STG_RUUM_CONTENT_SIGNING_KEY"
  # The sign-up allow-list (review 1 B-5). The server is closed when it is set.
  printf 'RUUM_SIGNUP_ALLOWED_EMAILS=%s\n' "$STG_ALLOWED_EMAILS"
  [[ -z "${STG_ALLOWED_UNVERIFIED_EMAILS:-}" ]] || printf 'RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS=%s\n' "$STG_ALLOWED_UNVERIFIED_EMAILS"
} | secret ruum ruum-server-env

# `helmfile sync`, not `apply`: apply needs the helm-diff plugin, which the tools image does not
# carry (every tool there is pinned by SHA-256; a plugin would be one more download).
log "platform charts (helmfile)"
export PG_GENERATION="$generation" PG_RESTORE_FROM="$restore_from"
export ZITADEL_PAT_EXPIRY
ZITADEL_PAT_EXPIRY="$(date -u -d '+90 days' +%Y-%m-%dT00:00:00Z)"
# The CNPG CA must exist in the client namespaces before SpiceDB and Zitadel start; the
# cluster makes it, so the first pass installs the operators and the cluster, then copies it.
helmfile --file "$HERE/helmfile.yaml.gotmpl" --selector name=cert-manager --selector name=cnpg \
  --selector name=barman-cloud sync
helmfile --file "$HERE/helmfile.yaml.gotmpl" --selector name=platform sync --skip-deps
log "waiting for the CNPG cluster pg"
k -n data wait --for=condition=Ready cluster/pg --timeout=20m >/dev/null
for ns in authz auth ruum; do
  k -n data get secret pg-ca -o jsonpath='{.data.ca\.crt}' | base64 -d |
    k create secret generic pg-ca -n "$ns" --from-file=ca.crt=/dev/stdin --dry-run=client -o yaml |
    k apply -f - >/dev/null
  log "secret $ns/pg-ca (CNPG CA)"
done
helmfile --file "$HERE/helmfile.yaml.gotmpl" sync

log "deployer kubeconfig (namespaced, review 2 O-1)"
bash "$STG/ops/deployer-kubeconfig.sh"
log "platform is up (generation $generation${restore_from:+, restored from $restore_from})"
