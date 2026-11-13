#!/usr/bin/env bash
# Deploys one commit of main to staging, by digest (plan 6.2, review 1 S-7, review 2 P-4, U-3),
# through the deploy controller (backlog O-34, decision 0008-2).
#
#   deploy/staging/deploy.sh <sha> [--rollback | --rollback-older] [--acme letsencrypt-staging|zerossl]
#
# Steps 1 to 4 run on the Mac (early, readable errors); the deployer kubeconfig may then only
# write a release request, and the deploy controller in the cluster (namespace ruum-deploy) does
# the rest with its own ServiceAccount, after its own checks:
#   1. <sha> is on origin/main and the `ci` workflow passed for it on a push to main;
#   2. the tag <sha> of ghcr.io/nemecec/ruum-server and ruum-edge resolves to a digest;
#   3. each digest has a provenance attestation from images.yml on refs/heads/main;
#   4. a commit older than the deployed one is refused unless --rollback, and --rollback goes
#      only to the previous release (design 12.7); an older one needs --rollback-older, with a
#      warning (Kimi review of O-25, finding 2);
#   5. the release request (ops/release-request.sh: the commit, the two digests, the live commit
#      read in step 4, the rollback mode); deploy.sh waits for its outcome and prints the
#      controller's output;
#   6. the controller checks again by itself: the tag, the provenance (cosign) and the revision
#      label of each digest, that the commit is on main and not older than backlog O-25, and its
#      order against the live release (an older commit needs a rollback); new migrations since
#      the live release get an on-demand CNPG backup first;
#   7. the controller runs ops/deploy-chart.sh under the deploy lock (Lease ruum/ruum-deploy): the
#      live release must still be the one of step 4, the rollback rule again, the migration Job
#      (owner DSN of ruum/ruum-migrate-db, which the deployer cannot reach; if it fails, nothing
#      is rolled out), then helm upgrade --rollback-on-failure --wait with image@sha256:...;
#   8. the smoke checks (here, on the Mac).
# The chart, the migration Job and the deploy scripts come from the controller's image (the
# commit of main that the last platform step installed), not from this checkout or the target
# commit. The floor: no commit before backlog O-25 (ops/release-floor.sh).
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
# shellcheck source=ops/image-check.sh
. "$STG_HERE/ops/image-check.sh"
# shellcheck source=ops/release-floor.sh
. "$STG_HERE/ops/release-floor.sh"

REPO="${RUUM_REPO:-nemecec/ruum-server}"
sha="${1:-}"
[[ "$sha" =~ ^[0-9a-f]{7,40}$ ]] || stg_die "usage: deploy.sh <sha of main> [--rollback | --rollback-older] [--acme letsencrypt-staging|zerossl]"
shift
rollback=0
acme=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rollback) [[ $rollback == 0 ]] || stg_die "--rollback and --rollback-older exclude each other"; rollback=1; shift ;;
    --rollback-older) [[ $rollback == 0 ]] || stg_die "--rollback and --rollback-older exclude each other"; rollback=2; shift ;;
    --acme)
      [[ $# -ge 2 ]] && ruum_acme_issuer_ok "$2" || stg_die "--acme must be one of: ${RUUM_ACME_ISSUERS[*]}"
      acme=(--acme "$2")
      if [[ "$2" == zerossl ]]; then
        email="$(stg_secret STG_ACME_EMAIL)"
        [[ "$email" =~ $RUUM_EMAIL_RE ]] || stg_die "STG_ACME_EMAIL in secrets.env is missing or not a plain email address"
        acme+=(--acme-email "$email")
      fi
      shift 2
      ;;
    *) stg_die "unknown argument $1" ;;
  esac
done

git -C "$STG_ROOT" fetch -q origin main
sha="$(git -C "$STG_ROOT" rev-parse --verify "$sha^{commit}")"
[[ "$sha" =~ $RUUM_COMMIT_RE ]] || stg_die "$sha is not a full commit SHA"
git -C "$STG_ROOT" merge-base --is-ancestor "$sha" origin/main || stg_die "$sha is not on origin/main"
ruum_release_floor_ok "$STG_ROOT" "$sha" ||
  stg_die "$sha is older than backlog O-25: it has no migration step (ruum-server migrate) and no edge certificate storage, so this deploy path cannot run it. Deploy O-25 or a later commit."

stg_log "1. ci passed for $sha on a push to main"
ok="$(gh run list -R "$REPO" --workflow ci.yml --commit "$sha" --json conclusion,event,headBranch \
  --jq '[.[] | select(.event == "push" and .headBranch == "main" and .conclusion == "success")] | length')"
[[ "$ok" -ge 1 ]] || stg_die "no green ci run for $sha on a push to main"

stg_log "2. digests of the images tagged $sha"
refs=()
for name in ruum-server ruum-edge; do
  d="$(ghcr_digest "nemecec/$name" "$sha")"
  [[ "$d" =~ ^sha256:[a-f0-9]{64}$ ]] || stg_die "no image ghcr.io/nemecec/$name:$sha (did images.yml run for it?)"
  ref="ghcr.io/nemecec/$name@$d"
  [[ "$ref" =~ $RUUM_IMAGE_RE ]] || stg_die "unexpected image reference $ref"
  refs+=("$ref")
done

stg_log "3. provenance of each digest (images.yml on refs/heads/main)"
for ref in "${refs[@]}"; do
  gh attestation verify "oci://$ref" -R "$REPO" \
    --signer-workflow "$REPO/.github/workflows/images.yml" --source-ref refs/heads/main >/dev/null ||
    stg_die "no valid provenance attestation for $ref"
done

stg_log "4. the deployed commit (through the admin tunnel)"
stg_tunnel_up
deployed="$(printf '__END__\n' | stg_run ro status --deployed-commit 2>/dev/null || true)"
expect=(--expect-deployed none)
if [[ "$deployed" =~ ^[0-9a-f]{40}$ ]]; then
  expect=(--expect-deployed "$deployed")
  if [[ "$deployed" != "$sha" ]] && git -C "$STG_ROOT" merge-base --is-ancestor "$sha" "$deployed"; then
    case $rollback in
      0) stg_die "$sha is older than the deployed $deployed; pass --rollback to go back one release on purpose" ;;
      1) expect+=(--rollback) ;;
      2) stg_log "$RUUM_ROLLBACK_OLDER_WARNING"; expect+=(--rollback-older) ;;
    esac
  fi
  # The controller decides the backup itself (new migrations since the live release, review 2 U-3).
  if [[ -n "$(git -C "$STG_ROOT" diff --name-only "$deployed" "$sha" -- migrations/)" ]]; then
    stg_log "new migrations since $deployed: the controller takes an on-demand backup first"
  fi
else
  stg_log "no deployed commit yet (first deploy)"
fi

stg_log "5. release request for the deploy controller; 6. its checks, 7. migration step and rolling update of $sha"
printf '__END__\n' | stg_run rw deploy "$sha" "${refs[0]}" "${refs[1]}" "${expect[@]}" ${acme[@]+"${acme[@]}"}
stg_audit deploy "$sha"
bash "$STG_HERE/ops/smoke.sh"
