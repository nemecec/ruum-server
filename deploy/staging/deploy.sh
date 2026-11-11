#!/usr/bin/env bash
# Deploys one commit of main to staging, by digest (plan 6.2, review 1 S-7, review 2 P-4, U-3).
#
#   deploy/staging/deploy.sh <sha> [--rollback] [--acme letsencrypt-staging|zerossl]
#
# Steps (laptop; the rollout itself runs on the build server as ruum-ops):
#   1. <sha> is on origin/main and the `ci` workflow passed for it on a push to main;
#   2. the tag <sha> of ghcr.io/nemecec/ruum-server and ruum-edge resolves to a digest;
#   3. each digest has a provenance attestation from images.yml on refs/heads/main;
#   4. a commit older than the deployed one is refused unless --rollback;
#   5. new migrations since the deployed commit: an on-demand CNPG backup first;
#   6. helm upgrade --rollback-on-failure with image@sha256:..., then the smoke checks.
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

REPO="${RUUM_REPO:-nemecec/ruum-server}"
sha="${1:-}"
[[ "$sha" =~ ^[0-9a-f]{7,40}$ ]] || stg_die "usage: deploy.sh <sha of main> [--rollback] [--acme letsencrypt-staging|zerossl]"
shift
rollback=0
acme=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rollback) rollback=1; shift ;;
    --acme) acme=(--acme "$2"); [[ "$2" == zerossl ]] && acme+=(--acme-email "$(stg_secret STG_ACME_EMAIL)"); shift 2 ;;
    *) stg_die "unknown argument $1" ;;
  esac
done

git -C "$STG_ROOT" fetch -q origin main
sha="$(git -C "$STG_ROOT" rev-parse --verify "$sha^{commit}")"
git -C "$STG_ROOT" merge-base --is-ancestor "$sha" origin/main || stg_die "$sha is not on origin/main"

stg_log "1. ci passed for $sha on a push to main"
ok="$(gh run list -R "$REPO" --workflow ci.yml --commit "$sha" --json conclusion,event,headBranch \
  --jq '[.[] | select(.event == "push" and .headBranch == "main" and .conclusion == "success")] | length')"
[[ "$ok" -ge 1 ]] || stg_die "no green ci run for $sha on a push to main"

# digest IMAGE TAG: the manifest digest of a public GHCR image (anonymous pull token).
digest() {
  local image="$1" tag="$2" token
  token="$(curl -fsS "https://ghcr.io/token?scope=repository:$image:pull&service=ghcr.io" | jq -r .token)"
  printf 'header = "Authorization: Bearer %s"\n' "$token" |
    curl -fsS -K - -I \
      -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json' \
      "https://ghcr.io/v2/$image/manifests/$tag" | tr -d '\r' | awk -F': ' 'tolower($1) == "docker-content-digest" { print $2 }'
}

stg_log "2. digests of the images tagged $sha"
refs=()
for name in ruum-server ruum-edge; do
  d="$(digest "nemecec/$name" "$sha")"
  [[ "$d" =~ ^sha256:[a-f0-9]{64}$ ]] || stg_die "no image ghcr.io/nemecec/$name:$sha (did images.yml run for it?)"
  refs+=("ghcr.io/nemecec/$name@$d")
done

stg_log "3. provenance of each digest (images.yml on refs/heads/main)"
for ref in "${refs[@]}"; do
  gh attestation verify "oci://$ref" -R "$REPO" \
    --signer-workflow "$REPO/.github/workflows/images.yml" --source-ref refs/heads/main >/dev/null ||
    stg_die "no valid provenance attestation for $ref"
done

stg_log "4. the deployed commit"
deployed="$(printf '__END__\n' | stg_remote ro status --deployed-commit 2>/dev/null || true)"
backup=()
if [[ "$deployed" =~ ^[0-9a-f]{40}$ ]]; then
  if [[ "$deployed" != "$sha" ]] && git -C "$STG_ROOT" merge-base --is-ancestor "$sha" "$deployed"; then
    [[ $rollback == 1 ]] || stg_die "$sha is older than the deployed $deployed; pass --rollback to go back on purpose"
  fi
  # 5. Review 2 U-3: new migrations get an on-demand backup first.
  if [[ -n "$(git -C "$STG_ROOT" diff --name-only "$deployed" "$sha" -- migrations/)" ]]; then
    backup=(--backup)
  fi
else
  stg_log "no deployed commit yet (first deploy)"
fi

stg_log "6. rollout of $sha"
printf '__END__\n' | stg_remote rw deploy "$sha" "${refs[0]}" "${refs[1]}" "${backup[@]}" "${acme[@]}"
stg_audit deploy "$sha"
bash "$STG_HERE/ops/smoke.sh"
