#!/usr/bin/env bash
# Browser end-to-end tests (Playwright, Chromium) against the real server, the laptop stack and
# the web UI.
#
# It builds ruum-server, seeds the stack (`ruum-server seed-dev`), starts the server on
# RUUM_E2E_BROWSER_PORT (default 8492), its WebDAV host on RUUM_E2E_BROWSER_DAV_PORT (default
# 8496), and the web UI (Vite) on RUUM_E2E_WEB_PORT (default 5182),
# and runs the specs in web/e2e. The specs make their own Zitadel users and sign in through the
# real OIDC flow. The script stops what it started, also when a test fails.
#
#   scripts/dev.sh up              # the stack must be healthy
#   scripts/e2e_browser.sh         # all specs
#   scripts/e2e_browser.sh files   # extra arguments go to `playwright test`
#
# Settings:
#   RUUM_E2E_WEB=dev|preview       Vite dev server (default) or the built files (`vite build` + preview)
#   RUUM_SERVER_BIN=<path>         use this server binary instead of building one
#   RUUM_DEV_ROOT=<path>           the checkout whose deploy/dev holds the stack's .env and .state
#                                  (default: this checkout, or the main worktree of a git worktree)
#
# Every run makes new users and tenants; nothing is deleted. The work folder (logs, the HTML
# report, traces of failed tests) is printed at the start and kept.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PORT="${RUUM_E2E_BROWSER_PORT:-8492}"
DAV_PORT="${RUUM_E2E_BROWSER_DAV_PORT:-8496}"
WEB_PORT="${RUUM_E2E_WEB_PORT:-5182}"
WEB_MODE="${RUUM_E2E_WEB:-dev}"
SERVER_URL="http://127.0.0.1:$PORT"
WEB_URL="http://localhost:$WEB_PORT"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ruum-e2e-browser.XXXXXX")"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=false CARGO_PROFILE_TEST_DEBUG=false

say() { printf '\n== %s\n' "$*"; }
die() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

SERVER_PID=""
WEB_PID=""
cleanup() {
  local pid
  for pid in "$WEB_PID" "$SERVER_PID"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT

wait_for() { # wait_for <url> <what> <pid>
  for _ in $(seq 1 120); do
    curl -fsS -o /dev/null "$1" 2>/dev/null && return 0
    kill -0 "$3" 2>/dev/null || die "$2 stopped; see $WORK"
    sleep 0.5
  done
  die "$2 does not answer on $1"
}

free_gb() { df -Pk "$1" | awk 'NR == 2 { print int($4 / 1048576) }'; }

# ------------------------------------------------------------------ settings
say "settings (work folder: $WORK)"
DEV_ROOT="${RUUM_DEV_ROOT:-$ROOT}"
if [[ ! -f "$DEV_ROOT/deploy/dev/.env" ]]; then
  # A git worktree has no deploy/dev/.env of its own; the stack belongs to the main worktree.
  main="$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
  [[ -n "$main" && -f "${main%/.git}/deploy/dev/.env" ]] && DEV_ROOT="${main%/.git}"
fi
[[ -f "$DEV_ROOT/deploy/dev/.env" ]] || die "no deploy/dev/.env; run scripts/dev.sh up (or set RUUM_DEV_ROOT)"
eval "$("$DEV_ROOT/scripts/dev.sh" env)"
[[ -n "${ZITADEL_WEB_CLIENT_ID:-}" ]] || die "Zitadel apps are missing; run scripts/dev.sh up"
curl -fsS -o /dev/null "$ZITADEL_ISSUER/debug/healthz" || die "Zitadel does not answer at $ZITADEL_ISSUER; run scripts/dev.sh up"
curl -fsS -o /dev/null "http://127.0.0.1:8025/readyz" || die "Mailpit does not answer on 8025"
for p in "$PORT" "$DAV_PORT" "$WEB_PORT"; do
  if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$p/"; then die "port $p is in use"; fi
done
gb="$(free_gb "$CARGO_TARGET_DIR" 2>/dev/null || free_gb "$ROOT")"
[[ "$gb" -ge 8 ]] || die "only $gb GB free on the disk of $CARGO_TARGET_DIR; free space first"
echo "   stack: $DEV_ROOT/deploy/dev, server $SERVER_URL, web $WEB_URL ($WEB_MODE), $gb GB free"

# The server signs tokens and content URLs with these; the seed uses the same token key.
export RUUM_TOKEN_SIGNING_KEY="${RUUM_TOKEN_SIGNING_KEY:-$(openssl rand 32 | openssl base64 -A | tr '+/' '-_' | tr -d '=')}"
export RUUM_CONTENT_SIGNING_KEY="${RUUM_CONTENT_SIGNING_KEY:-$(openssl rand -base64 32)}"
export RUUM_BIND="127.0.0.1:$PORT"
# The WebDAV host listens on its own port (decision 0002-3); the specs do not use it.
export RUUM_DAV_BIND="127.0.0.1:$DAV_PORT"
export RUUM_DAV_URL="http://127.0.0.1:$DAV_PORT/"
# The browser sees one origin: Vite serves the UI and forwards /v1 to the server.
export RUUM_PUBLIC_URL="$WEB_URL"
export RUUM_OIDC_REDIRECT_URI="$WEB_URL/auth/callback"
# Downloads go to http://<tenant>.content.localhost:<port>/c/<token>; Chromium resolves *.localhost.
export RUUM_CONTENT_URL="http://content.localhost:$PORT"
export RUST_LOG="${RUST_LOG:-info}"

# ------------------------------------------------------------------ build
if [[ -n "${RUUM_SERVER_BIN:-}" ]]; then
  SERVER_BIN="$RUUM_SERVER_BIN"
else
  say "build ruum-server"
  cargo build -q -p ruum-server
  SERVER_BIN="$CARGO_TARGET_DIR/debug/ruum-server"
fi
# Other checkouts share the cargo cache and may rebuild target/debug/ruum-server during the run.
# A hard link keeps this build for the run (no copy; it is gone when the work folder is).
if ln "$SERVER_BIN" "$WORK/ruum-server" 2>/dev/null; then
  SERVER_BIN="$WORK/ruum-server"
fi

say "web dependencies"
if [[ ! -x web/node_modules/.bin/playwright ]]; then
  (cd web && npm ci --no-audit --no-fund)
fi
if ! (cd web && node -e "process.exit(require('fs').existsSync(require('playwright-core').chromium.executablePath()) ? 0 : 1)"); then
  (cd web && npx playwright install chromium)
fi
echo "   Playwright $(cd web && npx playwright --version), Chromium ready"

# ------------------------------------------------------------------ seed and server
say "start the server"
"$SERVER_BIN" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
wait_for "$SERVER_URL/healthz" "the server" "$SERVER_PID"
grep -q "stores: Postgres" "$WORK/server.log" || die "the server does not use the Postgres stores"
grep -q "authz engine: SpiceDB" "$WORK/server.log" || die "the server does not use SpiceDB"
echo "   ruum-server on $SERVER_URL (Postgres, SpiceDB, Garage, Zitadel, Mailpit)"

say "seed (ruum-server seed-dev)"
"$SERVER_BIN" seed-dev "$WORK/seed.env" >"$WORK/seed.log" 2>&1 || {
  cat "$WORK/seed.log" >&2
  die "seed-dev"
}
# shellcheck disable=SC1091
source "$WORK/seed.env"
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$SERVER_URL/v1/oauth/token" \
  --data-urlencode 'grant_type=urn:ietf:params:oauth:grant-type:token-exchange' \
  --data-urlencode "subject_token=$AGENT_KEY" \
  --data-urlencode 'subject_token_type=urn:ruum:params:oauth:token-type:api-key')"
[[ "$code" == 200 ]] || die "the seeded agent key does not work ($code)"
echo "   seed run $SEED_RUN: tenant $TENANT_A; the browser specs add Zitadel users of their own"

# ------------------------------------------------------------------ web UI
say "start the web UI ($WEB_MODE)"
export RUUM_API_URL="$SERVER_URL" RUUM_WEB_PORT="$WEB_PORT"
if [[ "$WEB_MODE" == preview ]]; then
  (cd web && npm run -s build) >"$WORK/build.log" 2>&1 || die "vite build; see $WORK/build.log"
  (cd web && exec node node_modules/vite/bin/vite.js preview --port "$WEB_PORT" --strictPort --host 127.0.0.1) >"$WORK/web.log" 2>&1 &
else
  (cd web && exec node node_modules/vite/bin/vite.js --port "$WEB_PORT" --strictPort) >"$WORK/web.log" 2>&1 &
fi
WEB_PID=$!
wait_for "$WEB_URL/healthz" "the web UI" "$WEB_PID"
echo "   web UI on $WEB_URL"

# ------------------------------------------------------------------ tests
say "playwright test $*"
set +e
(cd web && RUUM_E2E_WORK="$WORK" RUUM_E2E_WEB_URL="$WEB_URL" RUUM_E2E_SERVER_URL="$SERVER_URL" \
  RUUM_E2E_SEED="$WORK/seed.env" npx playwright test "$@")
status=$?
set -e

say "result"
echo "   report: $WORK/report/index.html (npx playwright show-report $WORK/report)"
echo "   logs: $WORK/server.log, $WORK/web.log"
if [[ $status -ne 0 ]]; then
  echo "   last server log lines:"
  tail -n 20 "$WORK/server.log" | sed 's/^/   /'
  die "playwright test exited with $status"
fi
echo "PASSED: browser end-to-end tests"
