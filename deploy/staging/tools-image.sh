#!/usr/bin/env bash
# The tools image of the staging steps: one small local image, built on the Mac with Docker
# Desktop, never pulled from a registry that we do not pin.
#
#   deploy/staging/tools-image.sh ensure   print the image name; build it first when it is missing
#   deploy/staging/tools-image.sh build    build it (again)
#   deploy/staging/tools-image.sh name     print the image name only
#   deploy/staging/tools-image.sh clean    remove every ruum-staging-tools image
#
# What is in it:
#   - the base TOOLS_BASE_IMAGE of versions.env (Alpine, pinned by digest), with bash, coreutils,
#     curl, jq, openssl and socat (tunnel-relay.sh) from that Alpine release;
#   - OpenTofu, talosctl, kubectl, helm, helmfile, cosign, hcloud and hcloud-upload-image, each
#     downloaded by setup.sh at the version of versions.env and checked against its SHA-256 there
#     (the same pins as CI);
#   - the OpenTofu providers of both roots, at the versions and hashes of their
#     .terraform.lock.hcl, in a local mirror; `tofu init` in the container uses only that mirror.
#
# The name is ruum-staging-tools:<16 hex>, a hash of this script, setup.sh, versions.env and the
# two lock files: a change to any pin makes a new image, and `ensure` removes the older ones, so
# the Mac keeps one image. The build is `docker run` plus `docker commit`, not `docker build`, so
# it leaves no build cache behind; the downloads live only in the build container.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=versions.env
. "$HERE/versions.env"
REPO=ruum-staging-tools
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'tools-image: %s\n' "$*" >&2; exit 1; }

image_name() {
  local sum
  sum="$(cat "$HERE/tools-image.sh" "$HERE/setup.sh" "$HERE/versions.env" \
    "$HERE/tofu/cluster/.terraform.lock.hcl" "$HERE/tofu/persistent/.terraform.lock.hcl" |
    { command -v sha256sum >/dev/null && sha256sum || shasum -a 256; } | cut -c1-16)"
  printf '%s:%s' "$REPO" "$sum"
}

# The script that runs inside the build container (as root, from the pinned base).
build_script() {
  cat <<'SH'
set -eu
apk add --no-cache bash coreutils curl jq openssl socat ca-certificates >/dev/null
RUUM_TOOLS_BIN=/usr/local/bin bash /src/setup.sh
arch="$(uname -m)"
case "$arch" in aarch64) p=linux_arm64 ;; x86_64) p=linux_amd64 ;; *) echo "unsupported $arch" >&2; exit 1 ;; esac
mkdir -p /usr/local/share/tofu-providers /tmp/m
for root in cluster persistent; do
  mkdir -p "/tmp/m/$root"
  cp /src/tofu/$root/*.tf /src/tofu/$root/.terraform.lock.hcl "/tmp/m/$root/"
  (cd "/tmp/m/$root" && tofu providers mirror -platform="$p" /usr/local/share/tofu-providers >/dev/null)
done
cat >/etc/tofu.rc <<'RC'
provider_installation {
  filesystem_mirror {
    path    = "/usr/local/share/tofu-providers"
    include = ["registry.opentofu.org/*/*"]
  }
}
RC
rm -rf /tmp/m /root/.cache /root/.terraform.d /var/cache/apk/*
tofu version >/dev/null && talosctl version --client --short >/dev/null && kubectl version --client >/dev/null &&
  helm version --short >/dev/null && helmfile --version >/dev/null && cosign version >/dev/null 2>&1 &&
  hcloud version >/dev/null && hcloud-upload-image --version >/dev/null && socat -V >/dev/null
SH
}

build() {
  local name="$1" box
  command -v docker >/dev/null || die "docker is missing (Docker Desktop)"
  [[ "$TOOLS_BASE_IMAGE" =~ @sha256:[0-9a-f]{64}$ ]] || die "TOOLS_BASE_IMAGE in versions.env must be pinned by digest"
  box="ruum-staging-tools-build-$$"
  log "building $name from $TOOLS_BASE_IMAGE (tools of versions.env, checked by SHA-256)"
  # The build container sees only deploy/staging, read-only.
  if ! build_script | docker run -i --name "$box" -v "$HERE:/src:ro" "$TOOLS_BASE_IMAGE" sh -s >&2; then
    docker rm -f "$box" >/dev/null 2>&1 || true
    die "the build failed; nothing was tagged"
  fi
  docker commit \
    --change 'ENV TF_CLI_CONFIG_FILE=/etc/tofu.rc TF_IN_AUTOMATION=1 TF_INPUT=0 CHECKPOINT_DISABLE=1' \
    --change 'WORKDIR /' --change 'ENTRYPOINT []' --change 'CMD ["bash"]' \
    --change "LABEL org.opencontainers.image.title=ruum-staging-tools org.opencontainers.image.base.name=$TOOLS_BASE_IMAGE" \
    "$box" "$name" >/dev/null
  docker rm -f "$box" >/dev/null
  log "built $name ($(docker image inspect -f '{{.Size}}' "$name" | awk '{ printf "%d MB", $1 / 1000000 }'))"
}

# Remove every ruum-staging-tools image except KEEP (or all of them without KEEP).
remove_others() {
  local keep="${1:-}" img
  while read -r img; do
    [[ -n "$img" && "$img" != "$keep" ]] || continue
    docker image rm "$img" >/dev/null 2>&1 && log "removed the older image $img" || true
  done < <(docker image ls --format '{{.Repository}}:{{.Tag}}' "$REPO")
}

name="$(image_name)"
case "${1:-ensure}" in
  name) printf '%s\n' "$name" ;;
  ensure)
    if ! docker image inspect "$name" >/dev/null 2>&1; then
      build "$name"
      remove_others "$name"
    fi
    printf '%s\n' "$name"
    ;;
  build)
    docker image rm "$name" >/dev/null 2>&1 || true
    build "$name"
    remove_others "$name"
    printf '%s\n' "$name"
    ;;
  clean) remove_others "" ;;
  *) die "usage: tools-image.sh [ensure | build | name | clean]" ;;
esac
