#!/usr/bin/env bash
# Destroy staging with one command (plan 11.2, review 2 D-1 to D-4).
#
#   deploy/staging/destroy.sh --plan          three lists: removed, kept on purpose, never touched;
#                                             and the state compared with the labelled resources
#   deploy/staging/destroy.sh                 the same, then asks you to type "destroy ruum-staging"
#   deploy/staging/destroy.sh --sweep         state lost or unreadable: delete by label env=staging
#                                             (same guard: never ruum-buildbox, the zones or production names)
#   deploy/staging/destroy.sh --purge-backups red action for the person, after the object lock
#                                             retention has ended (README)
#
# --plan is read-only and is what tests and agents run. The kept buckets (tofu/persistent and the
# state bucket) are never in the destroy path.
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

mode=destroy
case "${1:-}" in
  --plan) mode=plan ;;
  --sweep) mode=sweep ;;
  --purge-backups)
    cat <<'TXT'
--purge-backups is a red action (DESIGN 19.3): it deletes the kept backup buckets. Only the person
runs it, after the compliance retention (7 days after the last write) has ended:
  1. stop staging first (destroy.sh), so nothing writes to the buckets;
  2. remove `lifecycle { prevent_destroy = true }` from tofu/persistent/main.tf in a branch;
  3. RUUM_STAGING_APPLY=yes ... tofu destroy of tofu/persistent on the build server as ruum-ops.
Scaleway refuses to delete a locked version before its retention date, whatever the key.
TXT
    exit 0
    ;;
  "") ;;
  *) stg_die "usage: destroy.sh [--plan | --sweep | --purge-backups]" ;;
esac

if [[ $mode == plan ]]; then
  stg_secret_stream "${STG_NAMES_TOFU[@]}" | stg_remote ro destroy-plan
  exit 0
fi

stg_secret_stream "${STG_NAMES_TOFU[@]}" | stg_remote ro destroy-plan
printf '\nType "destroy ruum-staging" to %s: ' "$([[ $mode == sweep ]] && echo "delete by label" || echo destroy)"
read -r answer </dev/tty
[[ "$answer" == "destroy ruum-staging" ]] || stg_die "not confirmed; nothing was changed"
step=destroy
[[ $mode == sweep ]] && step=sweep
# The typed confirmation goes to the remote step on stdin, with the secrets.
{ printf 'RUUM_STAGING_DESTROY=%s\n' "$answer"; stg_secret_stream "${STG_NAMES_TOFU[@]}"; } | stg_remote rw "$step"
stg_audit "$step" "done"
