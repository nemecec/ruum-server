#!/usr/bin/env bash
# Installs the staging tools into ~/.local/bin of the current user, pinned by version and
# SHA-256 (versions.env). Runs on the build server (arm64) or a Linux CI runner (amd64).
# Idempotent: a tool with the right version is not downloaded again.
#
#   deploy/staging/setup.sh            install or check every tool
#   deploy/staging/setup.sh tofu helm  only these
#
# It creates no cloud resource and reads no secret.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=versions.env
. "$HERE/versions.env"

BIN="${RUUM_TOOLS_BIN:-$HOME/.local/bin}"
mkdir -p "$BIN"
case "$(uname -m)" in
  aarch64 | arm64) ARCH=arm64 ;;
  x86_64 | amd64) ARCH=amd64 ;;
  *) echo "setup.sh: unsupported machine $(uname -m)" >&2; exit 1 ;;
esac
[[ "$(uname -s)" == Linux ]] || { echo "setup.sh: Linux only (the build server or CI)" >&2; exit 1; }

log() { printf '==> %s\n' "$*" >&2; }
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# fetch URL SHA256 FILE: download and check, or fail without installing anything.
fetch() {
  local url="$1" sum="$2" out="$3"
  curl -fsSL --retry 3 -o "$out" "$url"
  echo "$sum  $out" | sha256sum -c --quiet - || { echo "setup.sh: checksum mismatch for $url" >&2; exit 1; }
}

sha() { local v="${1}_SHA256_${ARCH^^}"; printf '%s' "${!v}"; }

have() { # have NAME VERSION-SUBSTRING COMMAND...
  local name="$1" want="$2"
  shift 2
  [[ -x "$BIN/$name" ]] && "$BIN/$name" "$@" 2>/dev/null | grep -q -- "$want"
}

install_tofu() {
  have tofu "v$TOFU_VERSION" version && return
  fetch "https://github.com/opentofu/opentofu/releases/download/v$TOFU_VERSION/tofu_${TOFU_VERSION}_linux_$ARCH.tar.gz" "$(sha TOFU)" "$TMP/tofu.tgz"
  tar -xzf "$TMP/tofu.tgz" -C "$TMP" tofu
  install -m 0755 "$TMP/tofu" "$BIN/tofu"
  log "tofu $TOFU_VERSION"
}

install_talosctl() {
  have talosctl "$TALOS_VERSION" version --client --short && return
  fetch "https://github.com/siderolabs/talos/releases/download/$TALOS_VERSION/talosctl-linux-$ARCH" "$(sha TALOSCTL)" "$TMP/talosctl"
  install -m 0755 "$TMP/talosctl" "$BIN/talosctl"
  log "talosctl $TALOS_VERSION"
}

install_kubectl() {
  have kubectl "$KUBERNETES_VERSION" version --client && return
  fetch "https://dl.k8s.io/release/$KUBERNETES_VERSION/bin/linux/$ARCH/kubectl" "$(sha KUBECTL)" "$TMP/kubectl"
  install -m 0755 "$TMP/kubectl" "$BIN/kubectl"
  log "kubectl $KUBERNETES_VERSION"
}

install_helm() {
  have helm "$HELM_VERSION" version --short && return
  fetch "https://get.helm.sh/helm-$HELM_VERSION-linux-$ARCH.tar.gz" "$(sha HELM)" "$TMP/helm.tgz"
  tar -xzf "$TMP/helm.tgz" -C "$TMP" "linux-$ARCH/helm"
  install -m 0755 "$TMP/linux-$ARCH/helm" "$BIN/helm"
  log "helm $HELM_VERSION"
}

install_helmfile() {
  have helmfile "$HELMFILE_VERSION" --version && return
  fetch "https://github.com/helmfile/helmfile/releases/download/v$HELMFILE_VERSION/helmfile_${HELMFILE_VERSION}_linux_$ARCH.tar.gz" "$(sha HELMFILE)" "$TMP/helmfile.tgz"
  tar -xzf "$TMP/helmfile.tgz" -C "$TMP" helmfile
  install -m 0755 "$TMP/helmfile" "$BIN/helmfile"
  log "helmfile $HELMFILE_VERSION"
}

install_hcloud() {
  have hcloud "${HCLOUD_CLI_VERSION#v}" version && return
  fetch "https://github.com/hetznercloud/cli/releases/download/$HCLOUD_CLI_VERSION/hcloud-linux-$ARCH.tar.gz" "$(sha HCLOUD_CLI)" "$TMP/hcloud.tgz"
  tar -xzf "$TMP/hcloud.tgz" -C "$TMP" hcloud
  install -m 0755 "$TMP/hcloud" "$BIN/hcloud"
  log "hcloud $HCLOUD_CLI_VERSION"
}

install_hcloud_upload_image() {
  have hcloud-upload-image "$HCLOUD_UPLOAD_IMAGE_VERSION" --version && return
  local a=arm64
  [[ "$ARCH" == amd64 ]] && a=x86_64
  fetch "https://github.com/apricote/hcloud-upload-image/releases/download/v$HCLOUD_UPLOAD_IMAGE_VERSION/hcloud-upload-image_Linux_$a.tar.gz" "$(sha HCLOUD_UPLOAD_IMAGE)" "$TMP/hui.tgz"
  tar -xzf "$TMP/hui.tgz" -C "$TMP" hcloud-upload-image
  install -m 0755 "$TMP/hcloud-upload-image" "$BIN/hcloud-upload-image"
  log "hcloud-upload-image $HCLOUD_UPLOAD_IMAGE_VERSION"
}

install_cosign() {
  have cosign "${COSIGN_VERSION#v}" version && return
  fetch "https://github.com/sigstore/cosign/releases/download/$COSIGN_VERSION/cosign-linux-$ARCH" "$(sha COSIGN)" "$TMP/cosign"
  install -m 0755 "$TMP/cosign" "$BIN/cosign"
  log "cosign $COSIGN_VERSION"
}

tools=("$@")
[[ ${#tools[@]} -gt 0 ]] || tools=(tofu talosctl kubectl helm helmfile hcloud hcloud_upload_image cosign)
for t in "${tools[@]}"; do
  "install_${t//-/_}"
done
log "tools in $BIN are at the pinned versions"
