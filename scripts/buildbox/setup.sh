#!/usr/bin/env bash
# Set up the Ruum build server (scripts/buildbox/README.md). Runs on the server, as user ruum:
#
#   ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'bash -s' < scripts/buildbox/setup.sh
#
# It is idempotent. It waits for cloud-init, clones or updates the base copy ~/ruum-server from
# GitHub (fast-forward only), starts the dev stack there (scripts/dev.sh up), installs sqlx-cli
# (for the migration check of CI), makes ~/w and ~/targets, and prints a short status.
set -euo pipefail

REPO_URL="${RUUM_REPO_URL:-https://github.com/nemecec/ruum-server}"
BASE="$HOME/ruum-server"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# The whole script is one function, so `bash -s` reads all of it before a command can eat stdin.
main() {
  log "waiting for cloud-init (~/.bootstrap-done)"
  for _ in $(seq 1 120); do
    [[ -f "$HOME/.bootstrap-done" ]] && break
    sleep 5
  done
  [[ -f "$HOME/.bootstrap-done" ]] || die "cloud-init did not finish in 10 minutes"

  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
  # buildx for `docker compose build` (servers made before cloud-init.yaml listed it).
  docker buildx version >/dev/null 2>&1 || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq docker-buildx >/dev/null
  docker info >/dev/null 2>&1 || die "docker does not answer for user $USER"

  if [[ -d "$BASE/.git" ]]; then
    log "updating the base copy $BASE"
    git -C "$BASE" fetch -q origin
    git -C "$BASE" merge -q --ff-only origin/main || log "warning: the base copy cannot fast-forward; left as it is"
  else
    log "cloning $REPO_URL to $BASE"
    git clone -q "$REPO_URL" "$BASE"
  fi

  log "starting the dev stack (log: ~/dev-up.log)"
  # As in scripts/dev.sh, also for a base copy older than that line: a stable postgres image ID.
  export BUILDX_NO_DEFAULT_ATTESTATIONS=1
  "$BASE/scripts/dev.sh" up </dev/null >"$HOME/dev-up.log" 2>&1 || { tail -n 30 "$HOME/dev-up.log" >&2; die "scripts/dev.sh up failed (log: ~/dev-up.log)"; }

  if ! command -v sqlx >/dev/null 2>&1; then
    log "installing sqlx-cli (a few minutes)"
    cargo install -q --locked sqlx-cli --no-default-features --features postgres,rustls
  fi

  mkdir -p "$HOME/w" "$HOME/targets"
  # The images of the per-agent SpiceDB (scripts/buildbox/run.sh) come from the compose file.
  docker pull -q "$(awk '/image: authzed\/spicedb:/ {print $2; exit}' "$BASE/deploy/dev/docker-compose.yml")" >/dev/null

  echo
  echo "base copy:  $(git -C "$BASE" log -1 --format='%h %s')"
  echo "toolchain:  $(rustc --version), $(sqlx --version)"
  echo "disk:       $(df -h --output=avail / | tail -1 | tr -d ' ') free of $(df -h --output=size / | tail -1 | tr -d ' ')"
  echo "stack:"
  docker compose -p ruum ps --format '{{.Service}}\t{{.Status}}' | sed 's/^/  /'
  echo "agents:"
  docker ps -a --filter label=ruum.buildbox=agent --format '{{.Names}}\t{{.Status}}' | sed 's/^/  /'
  du -sh "$HOME"/targets/* 2>/dev/null | sed 's/^/  /' || true
}

main "$@"
