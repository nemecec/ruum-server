# Shared functions of the laptop-side staging scripts (up.sh, deploy.sh, status.sh, destroy.sh,
# secrets-push.sh). Sourced, not run.
#
# Where things run (review 2 R-1): the scripts run on the laptop, where the secrets file is. Each
# step runs on the build server over ssh. Secrets go to the remote process on its stdin as
# NAME=value lines: never on a command line (not in `ps`), never in a file, never printed.
#
# Two remote modes:
#   ops     (default) ssh as `ruum`, then `sudo -n -u ruum-ops` into a root-owned checkout of a
#           pushed commit (/opt/ruum-staging/current, ops/install-ops-user.sh). Build jobs run as
#           `ruum` and cannot read the staging credentials (review 2 O-1, review 1 S-2).
#   direct  RUUM_STAGING_REMOTE=direct: rsync this checkout to ~/stg-direct of `ruum` and run there.
#           Read-only only: plans, validate, status without cluster credentials. Every step that
#           creates, changes or deletes something refuses this mode.

# shellcheck shell=bash
STG_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG_ROOT="$(cd "$STG_HERE/../.." && pwd)"
# shellcheck source=versions.env
. "$STG_HERE/versions.env"

STG_HOST="${RUUM_BUILD_HOST:-ruum@2.31.194.2}"
STG_SSH_KEY="${RUUM_BUILD_KEY:-$HOME/.config/ruum/buildbox_ed25519}"
STG_SECRETS="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
STG_MODE="${RUUM_STAGING_REMOTE:-ops}"
STG_OPS_RUN="/opt/ruum-staging/current/deploy/staging/remote.sh"
STG_DIRECT_DIR="stg-direct"

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

# Prints NAME=value for each name (to be piped into stg_remote). Never call it on a terminal.
stg_secret_stream() {
  local n v
  for n in "$@"; do
    v="$(stg_secret "$n")"
    [[ -n "$v" ]] && printf '%s=%s\n' "$n" "$v"
  done
  printf '__END__\n'
}

# The names each remote step needs. Nothing else is ever sent.
# shellcheck disable=SC2034 # used by the scripts that source this file
STG_NAMES_TOFU=(HCLOUD_TOKEN HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY SCW_ACCESS_KEY SCW_SECRET_KEY
  SCW_DEFAULT_PROJECT_ID SCW_DEFAULT_ORGANIZATION_ID TOFU_STATE_PASSPHRASE)

stg_ssh() {
  ssh -o BatchMode=yes -o ConnectTimeout=15 -i "$STG_SSH_KEY" "$STG_HOST" "$@"
}

# Sync this checkout's deploy/staging to the build server (direct mode only).
stg_sync_direct() {
  # </dev/null: the secrets on stdin are for the remote step, not for these helpers.
  stg_ssh "mkdir -p $STG_DIRECT_DIR/deploy/staging" </dev/null
  rsync -az --delete -e "ssh -o BatchMode=yes -i $STG_SSH_KEY" \
    "$STG_ROOT/deploy/staging/" "$STG_HOST:$STG_DIRECT_DIR/deploy/staging/" </dev/null
}

# stg_remote MUTATES STEP ARGS...: run remote.sh STEP on the build server; stdin is passed on.
# MUTATES is "ro" or "rw": an "rw" step refuses direct mode.
stg_remote() {
  local mutates="$1"
  shift
  case "$STG_MODE" in
    direct)
      [[ "$mutates" == ro ]] || stg_die "RUUM_STAGING_REMOTE=direct is for read-only steps; '$1' changes staging. Use the ruum-ops mode (ops/install-ops-user.sh)."
      stg_sync_direct
      stg_ssh "bash $STG_DIRECT_DIR/deploy/staging/remote.sh $(printf '%q ' "$@")"
      ;;
    ops)
      stg_ssh "sudo -n -u ruum-ops $STG_OPS_RUN $(printf '%q ' "$@")"
      ;;
    *) stg_die "RUUM_STAGING_REMOTE must be ops or direct" ;;
  esac
}

# One audit line per operation (review 2 O-2): who, what, when, result. Local file; the digest
# reads it. Never holds a secret.
stg_audit() {
  local dir="${RUUM_STAGING_AUDIT_DIR:-$HOME/.local/state/ruum}"
  mkdir -p "$dir"
  printf '%s\t%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${USER:-agent}" "$1" "$2" >>"$dir/staging-audit.tsv"
}
