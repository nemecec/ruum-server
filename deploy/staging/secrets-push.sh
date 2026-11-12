#!/usr/bin/env bash
# Sends the cluster secrets from the Mac's secrets file to the tools container, on its stdin, for one target
# (review 1 S-3). Each target has a fixed list of names; nothing else is ever sent, and the
# Hetzner token and the Stripe keys are never sent to the cluster (tests/run.sh checks the lists).
#
#   deploy/staging/secrets-push.sh platform [--restore-from GENERATION]
#
# In the container, k8s/apply.sh splits them into one Kubernetes Secret per consumer and
# namespace (README "Secrets").
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

# The platform target. Keep in sync with CLUSTER_SECRET_NAMES in steps.sh (tests/run.sh).
PLATFORM_NAMES=(SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY
  GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET
  STG_ZITADEL_MASTERKEY STG_PG_RUUM_PASSWORD STG_PG_ZITADEL_PASSWORD STG_PG_SPICEDB_PASSWORD
  STG_SPICEDB_PRESHARED_KEY STG_RUUM_TOKEN_SIGNING_KEY STG_RUUM_CONTENT_SIGNING_KEY STG_RCLONE_CRYPT_PASSWORD STG_RCLONE_CRYPT_SALT
  STG_NATS_RUUM_PASSWORD STG_ALLOWED_EMAILS STG_ALLOWED_UNVERIFIED_EMAILS STG_ACME_EMAIL
  SCW_DEFAULT_PROJECT_ID SCW_SMTP_SECRET_KEY)
# Names that never go to the cluster, whatever a list says.
FORBIDDEN_RE='^(HCLOUD_TOKEN|HETZNER_DNS_TOKEN|STRIPE_.*|SCW_ACCESS_KEY|SCW_SECRET_KEY|TOFU_STATE_PASSPHRASE)$'

target="${1:-}"
shift || true
case "$target" in
  platform) names=("${PLATFORM_NAMES[@]}") ;;
  --list) printf '%s\n' "${PLATFORM_NAMES[@]}"; exit 0 ;;
  *) stg_die "usage: secrets-push.sh platform [--restore-from GENERATION]" ;;
esac
for n in "${names[@]}"; do
  [[ ! "$n" =~ $FORBIDDEN_RE ]] || stg_die "refusing to send $n to the cluster"
done
required=(SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY
  GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET
  STG_ZITADEL_MASTERKEY STG_PG_RUUM_PASSWORD STG_PG_ZITADEL_PASSWORD STG_PG_SPICEDB_PASSWORD
  STG_SPICEDB_PRESHARED_KEY STG_RUUM_TOKEN_SIGNING_KEY STG_RUUM_CONTENT_SIGNING_KEY STG_RCLONE_CRYPT_PASSWORD STG_RCLONE_CRYPT_SALT
  STG_NATS_RUUM_PASSWORD STG_ALLOWED_EMAILS SCW_DEFAULT_PROJECT_ID SCW_SMTP_SECRET_KEY)
missing="$(stg_missing "${required[@]}")"
[[ -z "$missing" ]] || stg_die "missing in secrets.env: $(echo $missing) (secrets-init.sh generates the STG_* ones; the person adds SCW_BACKUP_*, SCW_SMTP_SECRET_KEY and STG_ALLOWED_EMAILS, docs/setup/ACCOUNTS.md)"
stg_tunnel_up
stg_log "sending ${#names[@]} names to the tools container (values on stdin, never printed)"
# The tofu names too: apply.sh reads the generation from the encrypted state.
stg_secret_stream "${names[@]}" "${STG_NAMES_TOFU[@]}" | stg_run rw platform "$@"
