# Shared functions of the staging scripts (up.sh, deploy.sh, status.sh, destroy.sh,
# secrets-push.sh, bootstrap-state.sh, tool.sh). Sourced, not run.
#
# Where things run (decision of the person, 2026-11-11): every staging step runs on the person's
# Mac, never on the build server. The build server's agent user is in the docker group, which is
# root on that host, so no user separation there can protect the staging credentials.
#
# - The scripts run in the Mac's own bash (macOS bash 3.2 is enough). They read the secrets file
#   ~/.config/ruum/secrets.env line by line, one name at a time, and never source it.
# - Each step runs steps.sh in the tools container (tools-image.sh: one local image with
#   OpenTofu, talosctl, kubectl, helm, helmfile, cosign, hcloud and hcloud-upload-image at the
#   versions and SHA-256 sums of versions.env, on a digest-pinned Alpine base). Docker Desktop
#   runs it; `docker run --rm`, so nothing stays behind but the files in the work folder.
# - Secrets go to the container on its stdin as NAME=value lines: never on a command line (not
#   in `ps`, not in `docker inspect`), never in a file, never printed.
# - The work folder ~/.config/ruum/staging (0700) holds the kubeconfig, the talosconfig and the
#   plans. It is the only folder that the container can write. The checkout is mounted read-only.
#
# The staging firewall opens the Talos API (50000) and the Kubernetes API (6443) only to the Mac's
# current public IPv4 address (steps.sh admin_cidrs). When that address changes, run
# `RUUM_STAGING_APPLY=yes up.sh --apply --infra-only` to move the firewall rule.

# shellcheck shell=bash
STG_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG_ROOT="$(cd "$STG_HERE/../.." && pwd)"
# shellcheck source=versions.env
. "$STG_HERE/versions.env"

STG_SECRETS="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
STG_WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"

stg_log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
stg_warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
stg_die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# The value of one name in the secrets file. The file is read line by line for that name and
# never sourced, so no other secret gets into the shell.
stg_secret() {
  local name="$1" line value
  [[ -r "$STG_SECRETS" ]] || return 0
  line="$(grep -E "^(export +)?$name=" "$STG_SECRETS" | tail -n 1 || true)"
  [[ -n "$line" ]] || return 0
  value="${line#*=}"
  value="${value%$'\r'}"
  if [[ "$value" =~ ^\"(.*)\"$ || "$value" =~ ^\'(.*)\'$ ]]; then value="${BASH_REMATCH[1]}"; fi
  printf '%s' "$value"
}

stg_has_secret() { [[ -n "$(stg_secret "$1")" ]]; }

# The names that are missing from the secrets file (prints them, one per line).
stg_missing() {
  local n
  for n in "$@"; do stg_has_secret "$n" || echo "$n"; done
}

# Prints NAME=value for each name (to be piped into stg_run). Never call it on a terminal.
stg_secret_stream() {
  local n v
  for n in "$@"; do
    v="$(stg_secret "$n")"
    [[ -n "$v" ]] && printf '%s=%s\n' "$n" "$v"
  done
  printf '__END__\n'
}

# The names each step needs. Nothing else is ever sent.
# shellcheck disable=SC2034 # used by the scripts that source this file
STG_NAMES_TOFU=(HCLOUD_TOKEN HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY SCW_ACCESS_KEY SCW_SECRET_KEY
  SCW_DEFAULT_PROJECT_ID SCW_DEFAULT_ORGANIZATION_ID TOFU_STATE_PASSPHRASE)

# The work folder on the Mac: 0700, owned by the person.
stg_work() {
  mkdir -p "$STG_WORK"
  chmod 700 "$STG_WORK"
}

# The name of the tools image for this checkout (tools-image.sh builds it when it is missing).
stg_image() {
  STG_IMAGE="$(bash "$STG_HERE/tools-image.sh" ensure)" || stg_die "the tools image is not available (deploy/staging/tools-image.sh build)"
  [[ "$STG_IMAGE" =~ ^ruum-staging-tools:[0-9a-f]{16}$ ]] || stg_die "unexpected tools image name '$STG_IMAGE'"
}

# stg_docker [docker run options...] -- COMMAND...: run COMMAND in the tools container.
# The checkout is mounted read-only and the work folder read-write, both at their Mac paths, so
# every path in a message is the same on both sides. No capabilities, no new privileges, the
# person's user and group IDs. stdin is passed on (-i), so the secrets reach the command.
stg_docker() {
  local opts=()
  while [[ $# -gt 0 && "$1" != -- ]]; do opts+=("$1"); shift; done
  [[ "${1:-}" == -- ]] && shift
  command -v docker >/dev/null || stg_die "docker is missing (Docker Desktop)"
  stg_work
  stg_image
  docker run --rm -i --init \
    --user "$(id -u):$(id -g)" \
    --cap-drop ALL --security-opt no-new-privileges \
    --tmpfs /tmp:rw,exec,size=2g \
    -e HOME=/tmp \
    -e RUUM_STAGING_WORK="$STG_WORK" \
    -e KUBECONFIG="$STG_WORK/kubeconfig" \
    -e TALOSCONFIG="$STG_WORK/talosconfig" \
    -v "$STG_ROOT:$STG_ROOT:ro" \
    -v "$STG_WORK:$STG_WORK" \
    -w "$STG_ROOT" \
    ${opts[@]+"${opts[@]}"} \
    "$STG_IMAGE" "$@"
}

# stg_run MUTATES STEP ARGS...: run steps.sh STEP in the tools container; stdin is passed on.
# MUTATES is "ro" or "rw". Only an "rw" step gets RUUM_STAGING_APPLY (when it is "yes" here), and
# the steps that create or change cloud resources refuse to run without it (steps.sh).
# RUUM_ADMIN_IP, when set, replaces the detected public address of the Mac (steps.sh checks it).
stg_run() {
  local mutates="$1" env=()
  shift
  case "$mutates" in
    ro) ;;
    rw) [[ "${RUUM_STAGING_APPLY:-}" != yes ]] || env+=(-e RUUM_STAGING_APPLY=yes) ;;
    *) stg_die "stg_run: ro or rw, not '$mutates'" ;;
  esac
  [[ -z "${RUUM_ADMIN_IP:-}" ]] || env+=(-e "RUUM_ADMIN_IP=$RUUM_ADMIN_IP")
  stg_docker ${env[@]+"${env[@]}"} -- bash "$STG_HERE/steps.sh" "$@"
}

# One audit line per operation (review 2 O-2): who, what, when, result. Local file; the digest
# reads it. Never holds a secret.
stg_audit() {
  local dir="${RUUM_STAGING_AUDIT_DIR:-$HOME/.local/state/ruum}"
  mkdir -p "$dir"
  printf '%s\t%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${USER:-agent}" "$1" "$2" >>"$dir/staging-audit.tsv"
}
