# Checks of a Ruum image before it runs in staging (code review F-3). Sourced by deploy.sh (on the
# Mac, with gh) and ops/deploy-chart.sh (in the tools container, with cosign), so the provenance
# check cannot be skipped by calling the lower-level script directly.
# shellcheck shell=bash
# shellcheck disable=SC2034 # the patterns are used by the scripts that source this file

RUUM_IMAGE_REPO="nemecec/ruum-server"
# The only images: the two Ruum images of this repository at GHCR, by digest.
RUUM_IMAGE_RE='^ghcr\.io/nemecec/ruum-(server|edge)@sha256:[a-f0-9]{64}$'
RUUM_COMMIT_RE='^[0-9a-f]{40}$'
# The app runtime (card I-6): the gateway and the egress proxy of the same images.yml run as the
# server. ops/deploy-chart.sh resolves them from the tag <commit> and checks their provenance;
# they are never a server, edge or controller image.
RUUM_APP_IMAGE_RE='^ghcr\.io/nemecec/ruum-(gateway|egress)@sha256:[a-f0-9]{64}$'
# The sandbox supervisor (backlog I620-1): the init container of every Kubernetes sandbox, of the
# same images.yml run as the server; ops/deploy-chart.sh resolves it from the tag <commit> into
# runtime.supervisorImage. Never an app runtime, server, edge or controller image.
RUUM_SUPERVISOR_IMAGE_RE='^ghcr\.io/nemecec/ruum-supervisor@sha256:[a-f0-9]{64}$'
# The deploy controller (backlog O-34): installed by the platform step (k8s/apply.sh), by digest.
# Never a server or edge image (the chart and the controller accept only RUUM_IMAGE_RE there).
RUUM_CONTROLLER_IMAGE_RE='^ghcr\.io/nemecec/ruum-deploy-controller@sha256:[a-f0-9]{64}$'
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

# The lookups of the app runtime images (card I-6; review of I-6, 2 and Kimi 5). They tell a
# missing image apart from an error, and they read the revision label like the deploy
# controller's GHCR.Revision (deploy/images/deploy-controller/registry.go), each manifest and the
# config checked against its digest. The token goes to curl in a config on stdin, never in argv.
GHCR_ACCEPT='application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json'

# _ghcr_sha256: the hex SHA-256 of stdin (sha256sum in the tools image, shasum on a Mac).
_ghcr_sha256() {
  if command -v sha256sum >/dev/null; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}

# _ghcr_token IMAGE: an anonymous pull token. Status 3: GHCR denies one, which it does for a
# package that does not exist and for a private one (both mean "no image" to a public reader);
# 1: any other failure (network, rate limit, server error).
_ghcr_token() {
  local image="$1" body code
  body="$(curl -sS -w '\n%{http_code}' "https://ghcr.io/token?scope=repository:$image:pull&service=ghcr.io")" || return 1
  code="${body##*$'\n'}"
  body="${body%$'\n'*}"
  case "$code" in
    200) jq -er '.token | select(type == "string" and length > 0)' <<<"$body" 2>/dev/null || return 1 ;;
    403 | 404) return 3 ;;
    *) return 1 ;;
  esac
}

# _ghcr_get IMAGE TOKEN PATH OUT: GET https://ghcr.io/v2/IMAGE/PATH into OUT (blobs follow the
# redirect to storage; curl does not send the token to another host). Prints the HTTP status.
_ghcr_get() {
  local image="$1" token="$2" rel="$3" out="$4"
  printf 'header = "Authorization: Bearer %s"\n' "$token" |
    curl -sS -L -K - -o "$out" -w '%{http_code}' -H "Accept: $GHCR_ACCEPT" "https://ghcr.io/v2/$image/$rel"
}

# ghcr_tag_lookup IMAGE TAG: prints the manifest digest of IMAGE:TAG (the SHA-256 of the manifest
# it serves). Status 3: no such image for a public reader (the package is missing or private, or
# the tag is missing); 1: any other error, so a caller can fail closed.
ghcr_tag_lookup() {
  local image="$1" tag="$2" token rc=0 tmp code
  token="$(_ghcr_token "$image")" || rc=$?
  [[ $rc == 0 ]] || return "$rc"
  tmp="$(mktemp)"
  code="$(_ghcr_get "$image" "$token" "manifests/$tag" "$tmp")" || code=error
  case "$code" in
    200) printf 'sha256:%s\n' "$(_ghcr_sha256 <"$tmp")"; rc=0 ;;
    404) rc=3 ;;
    *) rc=1 ;;
  esac
  rm -f "$tmp"
  return "$rc"
}

# _ghcr_fetch_verified IMAGE TOKEN KIND DIGEST OUT: manifests/DIGEST or blobs/DIGEST into OUT,
# only when the answer is 200 and its SHA-256 is DIGEST.
_ghcr_fetch_verified() {
  local image="$1" token="$2" kind="$3" digest="$4" out="$5" code
  [[ "$digest" =~ ^sha256:[a-f0-9]{64}$ ]] || return 1
  code="$(_ghcr_get "$image" "$token" "$kind/$digest" "$out")" || return 1
  [[ "$code" == 200 && "sha256:$(_ghcr_sha256 <"$out")" == "$digest" ]]
}

# ghcr_revision REF: the label org.opencontainers.image.revision of the linux/amd64 image of REF
# (ghcr.io/<image>@sha256:<digest>; images.yml sets it to the commit it built). Status 1 when any
# step fails or any content does not match its digest.
ghcr_revision() {
  local ref="$1" image digest token tmp want rc=1
  image="${ref#ghcr.io/}"
  image="${image%%@*}"
  digest="${ref#*@}"
  token="$(_ghcr_token "$image")" || return 1
  tmp="$(mktemp -d)"
  if _ghcr_fetch_verified "$image" "$token" manifests "$digest" "$tmp/m"; then
    want=""
    if jq -e '.config == null' "$tmp/m" >/dev/null 2>&1; then
      # An index: the linux/amd64 image (the other entries are attestations).
      want="$(jq -r '[.manifests[]? | select(.platform.os == "linux" and .platform.architecture == "amd64") | .digest][0] // empty' "$tmp/m")"
      _ghcr_fetch_verified "$image" "$token" manifests "$want" "$tmp/m" || want=fail
    fi
    if [[ "$want" != fail ]] && jq -e '.config.digest | type == "string"' "$tmp/m" >/dev/null 2>&1 &&
      _ghcr_fetch_verified "$image" "$token" blobs "$(jq -r '.config.digest' "$tmp/m")" "$tmp/c"; then
      jq -r '.config.Labels["org.opencontainers.image.revision"] // empty' "$tmp/c" && rc=0
    fi
  fi
  rm -rf "$tmp"
  return "$rc"
}

# cosign_verify_provenance REF: the SLSA provenance attestation of REF (pushed to GHCR by
# actions/attest-build-provenance in images.yml) was signed by images.yml of this repository at
# refs/heads/main (the certificate identity), through GitHub's OIDC issuer (Sigstore public good).
# On main, images.yml runs only from workflow_run (its push trigger ignores main).
cosign_verify_provenance() {
  local ref="$1"
  [[ "$ref" =~ $RUUM_IMAGE_RE || "$ref" =~ $RUUM_APP_IMAGE_RE || "$ref" =~ $RUUM_SUPERVISOR_IMAGE_RE || "$ref" =~ $RUUM_CONTROLLER_IMAGE_RE ]] || { echo "not a Ruum image by digest: $ref" >&2; return 1; }
  command -v cosign >/dev/null || { echo "cosign is missing (it is in the tools image, tools-image.sh)" >&2; return 1; }
  cosign verify-attestation "$ref" \
    --type https://slsa.dev/provenance/v1 \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity "https://github.com/$RUUM_IMAGE_REPO/.github/workflows/images.yml@refs/heads/main" \
    >/dev/null 2>&1
}
