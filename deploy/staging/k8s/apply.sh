#!/usr/bin/env bash
# The platform of staging, in the tools container on the Mac (steps.sh platform). The cluster
# secrets are in the environment (from secrets.env over the container's stdin); they become one Kubernetes
# Secret per consumer and namespace (review 1 S-3), through `kubectl apply -f -` on stdin, never
# a file or an argument.
#
#   KUBECONFIG=... DEPLOY_CONTROLLER_COMMIT=<sha of main> k8s/apply.sh <generation> [<restore-from-generation>]
#
# It also installs the deploy controller (backlog O-34) from the image of DEPLOY_CONTROLLER_COMMIT,
# by digest, after the same provenance check as a deploy (cosign: images.yml on refs/heads/main),
# and checks on the live cluster that the deployer may only write release requests
# (ops/rbac-check.sh live).
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
# shellcheck source=../ops/image-check.sh
. "$STG/ops/image-check.sh"

# The deploy controller image (backlog O-34), checked before anything changes.
[[ "${DEPLOY_CONTROLLER_COMMIT:-}" =~ $RUUM_COMMIT_RE ]] ||
  die "DEPLOY_CONTROLLER_COMMIT must be a commit of main (40 hex; secrets-push.sh passes it)"
controller_digest="$(ghcr_digest nemecec/ruum-deploy-controller "$DEPLOY_CONTROLLER_COMMIT" || true)"
[[ "$controller_digest" =~ ^sha256:[a-f0-9]{64}$ ]] ||
  die "no image ghcr.io/nemecec/ruum-deploy-controller:$DEPLOY_CONTROLLER_COMMIT (did images.yml publish it? Is the package public?)"
export DEPLOY_CONTROLLER_IMAGE="ghcr.io/nemecec/ruum-deploy-controller@$controller_digest"
[[ "$DEPLOY_CONTROLLER_IMAGE" =~ $RUUM_CONTROLLER_IMAGE_RE ]] || die "unexpected controller image $DEPLOY_CONTROLLER_IMAGE"
cosign_verify_provenance "$DEPLOY_CONTROLLER_IMAGE" ||
  die "no valid provenance attestation (images.yml on refs/heads/main) for $DEPLOY_CONTROLLER_IMAGE"
log "deploy controller $DEPLOY_CONTROLLER_IMAGE (commit $DEPLOY_CONTROLLER_COMMIT, provenance checked)"

for n in SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY \
  STG_ZITADEL_MASTERKEY STG_PG_RUUM_PASSWORD STG_PG_RUUM_APP_PASSWORD STG_PG_ZITADEL_PASSWORD STG_PG_SPICEDB_PASSWORD \
  STG_SPICEDB_PRESHARED_KEY STG_RUUM_TOKEN_SIGNING_KEY STG_RUUM_CONTENT_SIGNING_KEY \
  STG_RCLONE_CRYPT_PASSWORD STG_RCLONE_CRYPT_SALT STG_NATS_RUUM_PASSWORD STG_ALLOWED_EMAILS \
  SCW_DEFAULT_PROJECT_ID SCW_SMTP_SECRET_KEY; do
  [[ -n "${!n:-}" ]] || die "$n is missing (secrets-push.sh sends it)"
done

# secret NAMESPACE NAME [TYPE]: KEY=value lines on stdin -> a Secret, applied without a file.
secret() {
  local ns="$1" name="$2" type="${3:-generic}"
  k create secret "$type" "$name" -n "$ns" --from-env-file=/dev/stdin --dry-run=client -o yaml |
    k apply -f - >/dev/null
  log "secret $ns/$name"
}

# Right after the bootstrap the API server refuses connections, and then the nodes register one by
# one; `kubectl wait --all` fails at once on the first and passes on a partial list. So: retry
# until the API answers, NODE_COUNT nodes exist, and every one is Ready (15 minutes at most).
log "waiting for the API server and $NODE_COUNT Ready nodes"
deadline=$(( $(date +%s) + 900 ))
while :; do
  ready="$(k get nodes --request-timeout=10s --no-headers 2>/dev/null |
    awk '$2 == "Ready" { r++ } END { printf "%d/%d", r, NR }')" || ready="0/0"
  [[ "$ready" == "$NODE_COUNT/$NODE_COUNT" ]] && break
  (( $(date +%s) < deadline )) || die "after 15 minutes $ready nodes are Ready (want $NODE_COUNT/$NODE_COUNT)"
  log "  nodes Ready: $ready; retry in 10 s"
  sleep 10
done

log "namespaces, network policy engine, local-path storage"
k apply -f "$HERE/manifests/namespaces.yaml" >/dev/null
k apply -f "$HERE/manifests/kube-network-policies.yaml" >/dev/null
k apply -f "$HERE/manifests/local-path-storage.yaml" >/dev/null
k -n kube-system rollout status ds/kube-network-policies --timeout=5m >/dev/null

log "secrets, one per consumer"
# CNPG: the backup writer key only (object read/write, no delete; review 1 B-3).
printf 'ACCESS_KEY_ID=%s\nACCESS_SECRET_KEY=%s\n' "$SCW_BACKUP_ACCESS_KEY" "$SCW_BACKUP_SECRET_KEY" | secret data pg-backup-s3
# ruum owns the database ruum (the migration Job); ruum_app serves (no DDL; Kimi review of O-25).
for role in ruum ruum_app zitadel spicedb; do
  var="STG_PG_${role^^}_PASSWORD"
  printf 'username=%s\npassword=%s\n' "$role" "${!var}" |
    k create secret generic "pg-role-${role//_/-}" -n data --type=kubernetes.io/basic-auth --from-env-file=/dev/stdin --dry-run=client -o yaml |
    k label --local -f - cnpg.io/reload=true -o yaml | k apply -f - >/dev/null
  log "secret data/pg-role-${role//_/-}"
done
printf 'masterkey=%s\n' "$STG_ZITADEL_MASTERKEY" | secret auth zitadel-masterkey
printf 'dsn=postgres://zitadel:%s@pg-rw.data.svc.cluster.local:5432/zitadel?sslmode=verify-full&sslrootcert=/db-ssl-ca-crt/ca.crt\n' \
  "$STG_PG_ZITADEL_PASSWORD" | secret auth zitadel-db
# Zitadel's SMTP login at Scaleway Transactional Email (backlog O-20): the user is the project
# ID, the password the secret key of the IAM application ruum-staging-smtp (permission
# TransactionalEmailEmailSmtpCreate only), never the agents' full Scaleway key.
printf 'user=%s\npassword=%s\n' "$SCW_DEFAULT_PROJECT_ID" "$SCW_SMTP_SECRET_KEY" | secret auth zitadel-smtp
printf 'preshared-key=%s\ndatastore-uri=postgres://spicedb:%s@pg-rw.data.svc.cluster.local:5432/spicedb?sslmode=verify-full&sslrootcert=/pg-ca/ca.crt\n' \
  "$STG_SPICEDB_PRESHARED_KEY" "$STG_PG_SPICEDB_PASSWORD" | secret authz spicedb
printf 'ruum-password=%s\n' "$STG_NATS_RUUM_PASSWORD" | secret nats nats-auth
{
  printf 'HETZNER_ACCESS_KEY_ID=%s\nHETZNER_SECRET_ACCESS_KEY=%s\n' "$HETZNER_S3_ACCESS_KEY" "$HETZNER_S3_SECRET_KEY"
  printf 'SCW_ACCESS_KEY_ID=%s\nSCW_SECRET_ACCESS_KEY=%s\n' "$SCW_BACKUP_ACCESS_KEY" "$SCW_BACKUP_SECRET_KEY"
  printf 'CRYPT_PASSWORD=%s\nCRYPT_SALT=%s\n' "$STG_RCLONE_CRYPT_PASSWORD" "$STG_RCLONE_CRYPT_SALT"
} | secret ops blob-copy
# ruum/ruum-migrate-db and ruum/ruum-server-env: after the grants of ruum_app (below).

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
# The database roles of ruum (Kimi review of O-25, findings 1 and 5): the serving pods log in as
# ruum_app (data rights only, read-only on _sqlx_migrations); only the migration Job logs in as
# the owner ruum, with the DSN in its own Secret ruum/ruum-migrate-db that the serving pods do not
# mount. CNPG makes the role (managed roles, charts/platform cnpg.yaml); the grants run here, as
# the superuser over the primary's local socket, on every platform step (idempotent). The Secret
# of the serving pods changes only after the grants, so a pod that restarts in between still
# logs in with a role that has its rights.
log "database role ruum_app: grants (k8s/sql/ruum-runtime-grants.sql)"
primary="$(k -n data get cluster pg -o jsonpath='{.status.currentPrimary}')"
[[ "$primary" =~ ^pg-[0-9]+$ ]] || die "no CNPG primary for cluster pg (got '${primary}')"
for _ in $(seq 1 60); do
  [[ "$(k -n data exec "$primary" -c postgres -- psql -tAq -d ruum -c "select 1 from pg_roles where rolname = 'ruum_app'" 2>/dev/null)" == 1 ]] && break
  sleep 5
done
[[ "$(k -n data exec "$primary" -c postgres -- psql -tAq -d ruum -c "select 1 from pg_roles where rolname = 'ruum_app'" 2>/dev/null)" == 1 ]] ||
  die "CNPG did not make the role ruum_app in 5 minutes (managed roles of cluster pg)"
k -n data exec -i "$primary" -c postgres -- psql -q -d ruum -f - <"$HERE/sql/ruum-runtime-grants.sql" >/dev/null ||
  die "the grants of ruum_app failed (k8s/sql/ruum-runtime-grants.sql)"
# The owner's DSN: only the migration Job reads it (charts/ruum migrate-job.yaml).
printf 'DATABASE_URL=postgres://ruum:%s@pg-rw.data.svc.cluster.local:5432/ruum?sslmode=verify-full&sslrootcert=/pg-ca/ca.crt\n' "$STG_PG_RUUM_PASSWORD" |
  secret ruum ruum-migrate-db
# Everything the serving pods read; the DSN is ruum_app's.
{
  printf 'DATABASE_URL=postgres://ruum_app:%s@pg-rw.data.svc.cluster.local:5432/ruum?sslmode=verify-full&sslrootcert=/pg-ca/ca.crt\n' "$STG_PG_RUUM_APP_PASSWORD"
  printf 'RUUM_S3_ACCESS_KEY_ID=%s\nRUUM_S3_SECRET_ACCESS_KEY=%s\n' "$HETZNER_S3_ACCESS_KEY" "$HETZNER_S3_SECRET_KEY"
  printf 'RUUM_SPICEDB_PRESHARED_KEY=%s\n' "$STG_SPICEDB_PRESHARED_KEY"
  printf 'RUUM_TOKEN_SIGNING_KEY=%s\nRUUM_CONTENT_SIGNING_KEY=%s\n' "$STG_RUUM_TOKEN_SIGNING_KEY" "$STG_RUUM_CONTENT_SIGNING_KEY"
  # The sign-up allow-list (review 1 B-5). The server is closed when it is set.
  printf 'RUUM_SIGNUP_ALLOWED_EMAILS=%s\n' "$STG_ALLOWED_EMAILS"
  [[ -z "${STG_ALLOWED_UNVERIFIED_EMAILS:-}" ]] || printf 'RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS=%s\n' "$STG_ALLOWED_UNVERIFIED_EMAILS"
} | secret ruum ruum-server-env

helmfile --file "$HERE/helmfile.yaml.gotmpl" sync

log "deploy controller (namespace ruum-deploy, backlog O-34)"
k -n ruum-deploy rollout status deployment/deploy-controller --timeout=5m >/dev/null ||
  die "the deploy controller did not start (deploy/staging/tool.sh kubectl -n ruum-deploy describe pods)"
log "the deployer may only write release requests (ops/rbac-check.sh live)"
bash "$STG/ops/rbac-check.sh" live || die "the deployer or the deploy controller has other rights than it should (above)"
log "deployer kubeconfig (namespaced, review 2 O-1)"
bash "$STG/ops/deployer-kubeconfig.sh"
log "platform is up (generation $generation${restore_from:+, restored from $restore_from})"
