#!/usr/bin/env bash
# Creates the OpenTofu state bucket ruum-staging-tfstate at Scaleway (nl-ams), once: private,
# versioned, kept on destroy. CREATES A CLOUD RESOURCE (about EUR 0.01 per month): run it only
# with RUUM_STAGING_APPLY=yes, after the person or the orchestrator has read the plan.
# The state in it is encrypted by OpenTofu (TOFU_STATE_PASSPHRASE) before it leaves the build
# server, so the bucket never holds a readable secret.
#
#   RUUM_STAGING_APPLY=yes deploy/staging/bootstrap-state.sh     (laptop)
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
[[ "${RUUM_STAGING_APPLY:-}" == yes ]] || stg_die "bootstrap-state.sh creates a bucket; set RUUM_STAGING_APPLY=yes"
missing="$(stg_missing SCW_ACCESS_KEY SCW_SECRET_KEY TOFU_STATE_PASSPHRASE)"
[[ -z "$missing" ]] || stg_die "missing in secrets.env: $(echo $missing) (secrets-init.sh generates TOFU_STATE_PASSPHRASE)"
B=ruum-staging-tfstate
E=https://s3.nl-ams.scw.cloud
# s3 METHOD PATH [BODY-FILE] [curl options...]: the key goes to curl in a config on stdin (-K -),
# never on the command line.
s3() {
  local m="$1" p="$2" body="${3:-}"
  shift 3 || shift $#
  printf 'user = "%s:%s"\n' "$(stg_secret SCW_ACCESS_KEY)" "$(stg_secret SCW_SECRET_KEY)" |
    curl -fsS -K - --aws-sigv4 "aws:amz:nl-ams:s3" -X "$m" ${body:+--data-binary "@$body"} "$@" "$E/$p"
}
if s3 HEAD "$B" "" -o /dev/null -I 2>/dev/null; then
  stg_log "state bucket $B exists"
else
  s3 PUT "$B" "" >/dev/null
  stg_log "created $B"
fi
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
printf '<VersioningConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Status>Enabled</Status></VersioningConfiguration>' >"$tmp"
s3 PUT "$B?versioning" "$tmp" >/dev/null
printf '<Tagging><TagSet><Tag><Key>env</Key><Value>staging</Value></Tag><Tag><Key>managed-by</Key><Value>bootstrap-state.sh</Value></Tag><Tag><Key>project</Key><Value>ruum</Value></Tag></TagSet></Tagging>' >"$tmp"
s3 PUT "$B?tagging" "$tmp" >/dev/null
stg_log "versioning on, tags env=staging; state bucket ready"
stg_audit bootstrap-state ok
