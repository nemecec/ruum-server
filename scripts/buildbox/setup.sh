#!/usr/bin/env bash
# Set up the Ruum build server (scripts/buildbox/README.md). Runs on the server, as user ruum:
#
#   ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'bash -s' < scripts/buildbox/setup.sh
#   ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'bash -s -- --fresh-platform-db' < scripts/buildbox/setup.sh
#
# It is idempotent. It waits for cloud-init, installs Node.js (pinned, official binaries) when it
# is missing or another version, clones or updates the base copy ~/ruum-server from GitHub
# (fast-forward only), starts the dev stack there (scripts/dev.sh up), installs sqlx-cli (for the
# migration check of CI), the web dependencies of the base copy and Playwright's Chromium with its
# OS packages (for scripts/e2e_browser.sh), makes ~/w and ~/targets, and prints a short status.
#
# --fresh-platform-db: before the start, give the shared stack a new, empty platform Postgres
# volume (ruum_postgres: the databases ruum, zitadel and spicedb), with a new Zitadel instance and
# a new local KEK. The old volume (as a tar file), Zitadel state and KEK go to ~/backups/. Use it when that database is broken, for example when it holds a migration that
# main does not have. It refuses while an agent command runs. The agent Postgres containers of
# run.sh and the data cluster (postgres-data) are not touched.
set -euo pipefail

# Node.js for the web UI and its browser tests. Keep in step with cloud-init.yaml.
NODE_VERSION=22.22.3
NODE_SHA256_arm64=1c4a9933a5e45bc88f54f70b5f91232c127ec49f1a5989d23fb85824c7adf9b7
NODE_SHA256_x64=2e5d13569282d016861fae7c8f935e741693c269101a5bebcf761a5376d1f99f

REPO_URL="${RUUM_REPO_URL:-https://github.com/nemecec/ruum-server}"
BASE="$HOME/ruum-server"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Install the pinned Node.js to /opt/node and link node, npm, npx and corepack into /usr/local/bin.
install_node() {
  [[ "$(node --version 2>/dev/null || true)" == "v$NODE_VERSION" ]] && return
  local arch sum tmp
  case "$(uname -m)" in
    aarch64) arch=arm64 sum=$NODE_SHA256_arm64 ;;
    x86_64) arch=x64 sum=$NODE_SHA256_x64 ;;
    *) die "no Node.js build for $(uname -m)" ;;
  esac
  log "installing Node.js $NODE_VERSION ($arch)"
  command -v xz >/dev/null 2>&1 || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq xz-utils >/dev/null
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/node.tar.xz" "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-$arch.tar.xz"
  echo "$sum  $tmp/node.tar.xz" | sha256sum -c --quiet - || die "the Node.js download has the wrong SHA-256"
  sudo mkdir -p /opt/node
  sudo tar -xJf "$tmp/node.tar.xz" -C /opt/node
  rm -rf "$tmp"
  for b in node npm npx corepack; do
    sudo ln -sfn "/opt/node/node-v$NODE_VERSION-linux-$arch/bin/$b" "/usr/local/bin/$b"
  done
}

# compose of the stack in the base copy, as scripts/dev.sh runs it.
stack() { docker compose --project-directory "$BASE/deploy/dev" -f "$BASE/deploy/dev/docker-compose.yml" --env-file "$BASE/deploy/dev/.env" "$@"; }

# A new, empty platform Postgres volume for the shared stack (see --fresh-platform-db above).
fresh_platform_db() {
  local f busy=()
  for f in "$HOME"/w/.*.pid; do
    [[ -s "$f" ]] || continue
    kill -0 -- "-$(cat "$f")" 2>/dev/null && busy+=("$(basename "$f" .pid)")
  done
  [[ ${#busy[@]} -eq 0 ]] || die "agent commands run (${busy[*]#.}); try again when they end"
  local state="$BASE/deploy/dev/.state" backup
  backup="$HOME/backups/platform-db-$(date -u +%Y%m%dT%H%M%SZ)"
  log "fresh platform database: stopping the stack; the old volume and state go to $backup"
  stack --profile step4 down
  mkdir -p "$backup"
  if docker volume inspect ruum_postgres >/dev/null 2>&1; then
    docker run --rm -v ruum_postgres:/data:ro -v "$backup:/backup" \
      "$(awk '/image: ruum-dev\/postgres:/ {print $2; exit}' "$BASE/deploy/dev/docker-compose.yml")" \
      tar -czf /backup/ruum_postgres.tar.gz -C /data . || die "the backup of ruum_postgres failed; nothing deleted"
    docker volume rm ruum_postgres >/dev/null
  fi
  # Zitadel's bootstrap token and app IDs, and the KEK, belong to the old volume (scripts/dev.sh).
  # Zitadel runs as root, so its files are root's.
  sudo mv "$state/zitadel" "$backup/zitadel" 2>/dev/null || true
  [[ ! -e "$state/tenant-keys.kek" ]] || mv "$state/tenant-keys.kek" "$backup/"
  sudo chown -R "$USER:" "$backup"
  chmod -R go-rwx "$backup"
  log "kept: $backup (delete it when the new stack works)"
}

# The web dependencies of the base copy and Playwright's Chromium (the version that web/ pins),
# with the OS packages it needs. The browsers go to ~/.cache/ms-playwright, which all agents share;
# each agent's scripts/e2e_browser.sh runs its own `npm ci` and finds the browser there.
install_browser() {
  log "web dependencies and Playwright Chromium (log: ~/browser-setup.log)"
  (
    cd "$BASE/web"
    npm ci --no-audit --no-fund
    sudo DEBIAN_FRONTEND=noninteractive npx --no-install playwright install-deps chromium
    npx --no-install playwright install chromium
  ) </dev/null >"$HOME/browser-setup.log" 2>&1 || { tail -n 30 "$HOME/browser-setup.log" >&2; die "the browser setup failed (log: ~/browser-setup.log)"; }
}

# The whole script is one function, so `bash -s` reads all of it before a command can eat stdin.
main() {
  local fresh=""
  case "${1:-}" in
    "") ;;
    --fresh-platform-db) fresh=1 ;;
    *) die "unknown option $1 (known: --fresh-platform-db)" ;;
  esac
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
  install_node

  if [[ -d "$BASE/.git" ]]; then
    log "updating the base copy $BASE"
    git -C "$BASE" fetch -q origin
    git -C "$BASE" merge -q --ff-only origin/main || log "warning: the base copy cannot fast-forward; left as it is"
  else
    log "cloning $REPO_URL to $BASE"
    git clone -q "$REPO_URL" "$BASE"
  fi

  [[ -z "$fresh" ]] || fresh_platform_db

  log "starting the dev stack (log: ~/dev-up.log)"
  # As in scripts/dev.sh, also for a base copy older than that line: a stable postgres image ID.
  export BUILDX_NO_DEFAULT_ATTESTATIONS=1
  "$BASE/scripts/dev.sh" up </dev/null >"$HOME/dev-up.log" 2>&1 || { tail -n 30 "$HOME/dev-up.log" >&2; die "scripts/dev.sh up failed (log: ~/dev-up.log)"; }

  if ! command -v sqlx >/dev/null 2>&1; then
    log "installing sqlx-cli (a few minutes)"
    cargo install -q --locked sqlx-cli --no-default-features --features postgres,rustls
  fi

  install_browser

  mkdir -p "$HOME/w" "$HOME/targets"
  # The images of the per-agent SpiceDB (scripts/buildbox/run.sh) come from the compose file.
  docker pull -q "$(awk '/image: authzed\/spicedb:/ {print $2; exit}' "$BASE/deploy/dev/docker-compose.yml")" >/dev/null

  echo
  echo "base copy:  $(git -C "$BASE" log -1 --format='%h %s')"
  echo "toolchain:  $(rustc --version), $(sqlx --version)"
  echo "web:        node $(node --version), npm $(npm --version), $(cd "$BASE/web" && npx --no-install playwright --version)"
  echo "disk:       $(df -h --output=avail / | tail -1 | tr -d ' ') free of $(df -h --output=size / | tail -1 | tr -d ' ')"
  echo "stack:"
  docker compose -p ruum ps --format '{{.Service}}\t{{.Status}}' | sed 's/^/  /'
  echo "agents:"
  docker ps -a --filter label=ruum.buildbox=agent --format '{{.Names}}\t{{.Status}}' | sed 's/^/  /'
  du -sh "$HOME"/targets/* 2>/dev/null | sed 's/^/  /' || true
}

main "$@"
