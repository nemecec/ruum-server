#!/usr/bin/env bash
# Create or update staging with one command (plan section 11, review 2 R-1).
#
#   deploy/staging/up.sh                     the priced plan only (default; changes nothing)
#   deploy/staging/up.sh --out FILE          the same, also written to FILE (no secrets in it)
#   RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply [--restore-from GENERATION]
#                                            plan, guard, price, apply, platform, backup check, Zitadel
#   RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply --infra-only
#                                            only OpenTofu, the talosconfig and the kubeconfig
#
# Runs on the person's Mac (the secrets file and the credentials are here); every step runs in
# the tools container with the secrets on stdin (lib.sh). The Talos and Kubernetes APIs are open
# only to the build server's fixed IPv4 address; after the OpenTofu apply the Mac reaches them
# through the SSH tunnel over the build server (lib.sh stg_tunnel_up, ops/tunnel-setup.sh). The plan is
# refused when check-plan.sh finds a resource outside staging, or when it costs more than EUR 50
# per month (BUDGET_EUR_PER_MONTH). --apply also needs the environment variable
# RUUM_STAGING_APPLY=yes: an agent cannot apply by passing a flag alone.
#
# Exit codes: 0 done, 1 error, 2 over budget, 3 refused by check-plan, 4 a needed secret is missing.
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

apply=0
infra_only=0
out=""
restore_from=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --apply) apply=1; shift ;;
    --infra-only) infra_only=1; shift ;;
    --out) [[ $# -ge 2 ]] || stg_die "--out needs a file"; out="$2"; shift 2 ;;
    --restore-from)
      [[ "${2:-}" =~ ^[0-9]{12}$ ]] || stg_die "--restore-from needs a generation of 12 digits (YYYYMMDDHHMM)"
      restore_from="$2"
      shift 2
      ;;
    -h | --help) sed -n '2,21p' "$0"; exit 0 ;;
    *) stg_die "unknown argument $1" ;;
  esac
done

if [[ $apply == 1 && "${RUUM_STAGING_APPLY:-}" != yes ]]; then
  stg_die "--apply needs RUUM_STAGING_APPLY=yes in the environment (the person or the orchestrator sets it after reading the plan)"
fi

# --- Preflight: names only, never values ---
missing_tofu="$(stg_missing "${STG_NAMES_TOFU[@]}")"
if [[ -n "$missing_tofu" ]]; then
  printf 'STOP: these names are missing from %s:\n%s\n' "$STG_SECRETS" "$missing_tofu" >&2
  printf 'TOFU_STATE_PASSPHRASE: run deploy/staging/secrets-init.sh (it generates it once, never prints it).\n' >&2
  exit 4
fi

# The platform step needs the backup writer key (review 1 B-3): a separate Scaleway IAM
# application with object read and write only, no delete, no bypass, no email. The person
# creates it; the agents never put the full Scaleway key into the cluster.
missing_platform="$(stg_missing SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY)"
missing_signup="$(stg_missing STG_ALLOWED_EMAILS)"
# Zitadel's outgoing email (backlog O-20): the SMTP key of the Scaleway IAM application
# ruum-staging-smtp (TransactionalEmailEmailSmtpCreate only); the user is the project ID. The
# person creates it (docs/setup/ACCOUNTS.md, step 3); secrets-init.sh never generates it.
missing_smtp="$(stg_missing SCW_SMTP_SECRET_KEY SCW_DEFAULT_PROJECT_ID)"

plan_file="$(mktemp)"
stg_on_exit 'rm -f "$plan_file"'
stg_log "planning in the tools container (plan only, no state lock)"
set +e
stg_secret_stream "${STG_NAMES_TOFU[@]}" | stg_run ro plan >"$plan_file"
rc=$?
set -e
{
  echo "# Staging plan, $(date -u +%Y-%m-%dT%H:%M:%SZ), commit $(git -C "$STG_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "# Created by deploy/staging/up.sh (plan only). Secrets are never in this output."
  echo
  cat "$plan_file"
  echo
  echo "################ platform preflight ################"
  if [[ -n "$missing_platform" ]]; then
    echo "STOP: the backup writer key is missing: $(echo $missing_platform)."
    echo "  CloudNativePG needs SCW_BACKUP_ACCESS_KEY and SCW_BACKUP_SECRET_KEY in secrets.env: a Scaleway IAM"
    echo "  application 'ruum-staging-backup-writer' on project ruum-staging with ObjectStorageObjectsRead,"
    echo "  ObjectStorageObjectsWrite and ObjectStorageBucketsRead only (no delete, no email). The person creates"
    echo "  it in the Scaleway console (about 5 minutes, README 'Before the first apply'). --apply refuses until then."
  else
    echo "backup writer key: present"
  fi
  if [[ -n "$missing_signup" ]]; then
    echo "STOP: STG_ALLOWED_EMAILS is missing: the addresses that may sign in to staging (review 1 B-5)."
    echo "  Without it the server is closed to everyone. --apply refuses until it is set."
  else
    echo "sign-up allow-list: present"
  fi
  if [[ -n "$missing_smtp" ]]; then
    echo "STOP: the SMTP key is missing: $(echo $missing_smtp)."
    echo "  Zitadel sends email (the Microsoft email check) through Scaleway Transactional Email with the API key"
    echo "  of the IAM application 'ruum-staging-smtp' (TransactionalEmailEmailSmtpCreate on project ruum-staging"
    echo "  only). The person creates it (docs/setup/ACCOUNTS.md, step 3). --apply refuses until then."
  else
    echo "SMTP key: present"
  fi
} | if [[ -n "$out" ]]; then tee "$out"; else cat; fi

[[ $rc -eq 0 ]] || { stg_audit plan "refused rc=$rc"; exit "$rc"; }
if [[ $infra_only == 0 && ( -n "$missing_platform" || -n "$missing_signup" || -n "$missing_smtp" ) ]]; then
  stg_audit plan "stopped: missing names"
  exit 4
fi
stg_audit plan ok
[[ $apply == 1 ]] || { stg_log "plan only. To apply: RUUM_STAGING_APPLY=yes $0 --apply"; exit 0; }

# --- Apply ---
bash "$STG_HERE/secrets-init.sh"
stg_log "apply: snapshot, OpenTofu (exact reviewed plan), user_data check, talosconfig"
stg_secret_stream "${STG_NAMES_TOFU[@]}" | stg_run rw apply
stg_audit apply "tofu ok"
stg_log "kubeconfig through the admin tunnel over the build server"
stg_tunnel_up
printf '__END__\n' | stg_run rw kubeconfig
if [[ $infra_only == 1 ]]; then
  stg_log "done (--infra-only). kubeconfig and talosconfig are in $STG_WORK"
  exit 0
fi
stg_log "platform: namespaces, network policy, CNPG, SpiceDB, Zitadel, NATS, Mailpit, edge"
bash "$STG_HERE/secrets-push.sh" platform ${restore_from:+--restore-from "$restore_from"}
stg_audit apply "platform ok"
stg_log "backups: WAL push and a base backup into the COMPLIANCE-locked bucket (code review F-7)"
printf '__END__\n' | stg_run rw check-backup
stg_audit apply "backup check ok"
stg_log "Zitadel: OIDC app, Google, Microsoft, login policy, SMTP provider"
stg_secret_stream GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET |
  stg_run rw zitadel-bootstrap apply
stg_audit apply "zitadel ok"
stg_log "done. Next: deploy/staging/deploy.sh <sha of a green main>, then deploy/staging/status.sh --checks"
