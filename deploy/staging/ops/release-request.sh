#!/usr/bin/env bash
# Writes a release request for the deploy controller and waits for its outcome (backlog O-34,
# decision 0008-2). Runs in the tools container on the Mac with the deployer kubeconfig (steps.sh
# deploy, called by deploy.sh). The deployer may only create ConfigMaps in ruum-deploy and read
# them; the controller checks the request itself (provenance, main, floor, order against the live
# release), runs the migration step and the rollout with its own ServiceAccount, and writes the
# phase (annotation ruum.space/phase), a status line and the output back into the request.
#
#   release-request.sh <commit> <server-ref@sha256:...> <edge-ref@sha256:...> --expect-deployed <commit|none>
#                      [--rollback | --rollback-older] [--acme ISSUER] [--acme-email EMAIL] [--validate-only]
#
# Exit 0 when the controller deployed the commit; 1 when it refused or failed the request, or
# did not take it within RUUM_RELEASE_CLAIM_SECONDS (the controller refuses a request that waited
# more than five minutes, so a request this script gave up on never deploys later).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=image-check.sh
. "$HERE/image-check.sh"
die() { echo "release-request: $*" >&2; exit 1; }

NS=ruum-deploy
CLAIM_SECONDS="${RUUM_RELEASE_CLAIM_SECONDS:-240}"
DONE_SECONDS="${RUUM_RELEASE_DONE_SECONDS:-3600}"
POLL="${RUUM_RELEASE_POLL_SECONDS:-5}"

[[ $# -ge 3 ]] || die "usage: release-request.sh <commit> <server-ref> <edge-ref> --expect-deployed <commit|none> [--rollback | --rollback-older] [--acme ISSUER] [--acme-email EMAIL]"
commit="$1" server="$2" edge="$3"
shift 3
rollback=none expect_deployed="" acme="" acme_email="" validate_only=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rollback) [[ $rollback == none ]] || die "--rollback and --rollback-older exclude each other"; rollback=previous; shift ;;
    --rollback-older) [[ $rollback == none ]] || die "--rollback and --rollback-older exclude each other"; rollback=older; shift ;;
    --expect-deployed) [[ $# -ge 2 ]] || die "--expect-deployed needs a value"; expect_deployed="$2"; shift 2 ;;
    --acme) [[ $# -ge 2 ]] || die "--acme needs a value"; acme="$2"; shift 2 ;;
    --acme-email) [[ $# -ge 2 ]] || die "--acme-email needs a value"; acme_email="$2"; shift 2 ;;
    --validate-only) validate_only=1; shift ;;
    *) die "unknown argument $1" ;;
  esac
done
[[ "$commit" =~ $RUUM_COMMIT_RE ]] || die "the commit must be 40 lowercase hex characters"
[[ "$server" =~ $RUUM_IMAGE_RE && "$server" == ghcr.io/nemecec/ruum-server@* ]] || die "the server image must be ghcr.io/nemecec/ruum-server@sha256:<digest>"
[[ "$edge" =~ $RUUM_IMAGE_RE && "$edge" == ghcr.io/nemecec/ruum-edge@* ]] || die "the edge image must be ghcr.io/nemecec/ruum-edge@sha256:<digest>"
[[ -z "$acme" ]] || ruum_acme_issuer_ok "$acme" || die "--acme must be one of: ${RUUM_ACME_ISSUERS[*]}"
[[ -z "$acme_email" || "$acme_email" =~ $RUUM_EMAIL_RE ]] || die "--acme-email is not a plain email address"
[[ "$acme" != zerossl || -n "$acme_email" ]] || die "--acme zerossl needs --acme-email"
[[ "$expect_deployed" == none || "$expect_deployed" =~ $RUUM_COMMIT_RE ]] ||
  die "--expect-deployed must be the live commit that deploy.sh read (40 lowercase hex characters) or none"
[[ $rollback == none || "$expect_deployed" != none ]] || die "a rollback needs a live release (--expect-deployed)"

name="release-${commit:0:12}-$(date -u +%Y%m%d%H%M%S)-$(od -An -N2 -tx1 /dev/urandom | tr -d ' \n')"
doc="$(jq -n --arg ns "$NS" --arg name "$name" --arg commit "$commit" --arg server "$server" --arg edge "$edge" \
  --arg expect "$expect_deployed" --arg rollback "$rollback" --arg acme "$acme" --arg email "$acme_email" '
  { apiVersion: "v1", kind: "ConfigMap",
    metadata: { name: $name, namespace: $ns, labels: { "ruum.space/release-request": "true" } },
    data: ({ commit: $commit, server: $server, edge: $edge, expectDeployed: $expect, rollback: $rollback }
      + (if $acme != "" then { acme: $acme } else {} end)
      + (if $email != "" then { acmeEmail: $email } else {} end)) }')"
if [[ $validate_only == 1 ]]; then
  printf '%s\n' "$doc"
  exit 0
fi

kubectl -n "$NS" create -f - >/dev/null <<<"$doc" ||
  die "cannot write the release request into $NS (is the deploy controller installed? Run the platform step, k8s/apply.sh. Or the ResourceQuota release-requests of $NS is full: admin, deploy/staging/tool.sh kubectl -n $NS get configmaps)"
echo "==> release request $NS/$name written; waiting for the deploy controller" >&2

printed=0
start=$SECONDS
phase=""
while :; do
  cm="$(kubectl -n "$NS" get configmap "$name" -o json 2>/dev/null || true)"
  if [[ -n "$cm" ]]; then
    phase="$(jq -r '.metadata.annotations["ruum.space/phase"] // ""' <<<"$cm")"
    # New lines of the controller's output (it keeps the last 300).
    total="$(jq -r '.data.log // "" | if . == "" then 0 else (split("\n") | length) end' <<<"$cm")"
    if (( total > printed )); then
      jq -r --argjson from "$printed" '.data.log | split("\n") | .[$from:][]' <<<"$cm" | sed 's/^/   /' >&2
      printed=$total
    elif (( total < printed )); then
      printed=$total
    fi
  fi
  case "$phase" in
    Succeeded)
      echo "==> $(jq -r '.data.status // ""' <<<"$cm")" >&2
      exit 0 ;;
    Failed | Refused)
      die "the deploy controller ${phase,,} the request: $(jq -r '.data.status // ""' <<<"$cm")" ;;
    "")
      (( SECONDS - start < CLAIM_SECONDS )) ||
        die "the deploy controller did not take the request in ${CLAIM_SECONDS}s (admin: deploy/staging/tool.sh kubectl -n $NS get pods); it refuses it once it is five minutes old" ;;
  esac
  (( SECONDS - start < DONE_SECONDS )) ||
    die "no outcome after ${DONE_SECONDS}s (phase ${phase:-none}); look at the request: kubectl -n $NS get configmap $name -o yaml"
  sleep "$POLL"
done
