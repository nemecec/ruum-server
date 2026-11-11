# Checks of a Ruum image before it runs in staging (code review F-3). Sourced by deploy.sh (laptop)
# and ops/deploy-chart.sh (build server, as ruum-ops), so the provenance check cannot be skipped
# by calling the lower-level script directly.
# shellcheck shell=bash
# shellcheck disable=SC2034 # the patterns are used by the scripts that source this file

RUUM_IMAGE_REPO="nemecec/ruum-server"
# The only images: the two Ruum images of this repository at GHCR, by digest.
RUUM_IMAGE_RE='^ghcr\.io/nemecec/ruum-(server|edge)@sha256:[a-f0-9]{64}$'
RUUM_COMMIT_RE='^[0-9a-f]{40}$'
# The ACME issuers of the chart (review 2 R-3): never production Let's Encrypt.
RUUM_ACME_ISSUERS=(letsencrypt-staging zerossl)
# An email address for ACME/EAB: no comma, quote, space, equals sign or backslash.
RUUM_EMAIL_RE='^[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9-]{1,63}(\.[A-Za-z0-9-]{1,63})*\.[A-Za-z]{2,24}$'

ruum_acme_issuer_ok() {
  local i
  for i in "${RUUM_ACME_ISSUERS[@]}"; do [[ "$1" == "$i" ]] && return 0; done
  return 1
}

# ghcr_digest IMAGE TAG: the manifest digest of a public GHCR image (anonymous pull token).
ghcr_digest() {
  local image="$1" tag="$2" token
  token="$(curl -fsS "https://ghcr.io/token?scope=repository:$image:pull&service=ghcr.io" | jq -r .token)"
  printf 'header = "Authorization: Bearer %s"\n' "$token" |
    curl -fsS -K - -I \
      -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json' \
      "https://ghcr.io/v2/$image/manifests/$tag" | tr -d '\r' | awk -F': ' 'tolower($1) == "docker-content-digest" { print $2 }'
}

# cosign_verify_provenance REF: the SLSA provenance attestation of REF (pushed to GHCR by
# actions/attest-build-provenance in images.yml) was signed by images.yml of this repository at
# refs/heads/main (the certificate identity), through GitHub's OIDC issuer (Sigstore public good).
# On main, images.yml runs only from workflow_run (its push trigger ignores main).
cosign_verify_provenance() {
  local ref="$1"
  [[ "$ref" =~ $RUUM_IMAGE_RE ]] || { echo "not a Ruum image by digest: $ref" >&2; return 1; }
  command -v cosign >/dev/null || { echo "cosign is missing (setup.sh installs it)" >&2; return 1; }
  cosign verify-attestation "$ref" \
    --type https://slsa.dev/provenance/v1 \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity "https://github.com/$RUUM_IMAGE_REPO/.github/workflows/images.yml@refs/heads/main" \
    >/dev/null 2>&1
}
