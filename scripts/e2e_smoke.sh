#!/usr/bin/env bash
# End-to-end smoke test of the Ruum server against the laptop stack.
#
# It builds ruum-server and the ruum CLI, starts the server on RUUM_E2E_PORT (default 8491)
# and its WebDAV host on RUUM_E2E_DAV_PORT (default 8493) with the stack's Postgres, Garage and
# SpiceDB, seeds a tenant, users and an agent key (`ruum-server seed-dev`), and then checks with
# the CLI and curl: folders, chunked upload and download, versions, a copy, sharing, a share email
# in Mailpit (RUUM_E2E_MAILPIT_URL, default http://127.0.0.1:8025), the changes feed, MCP, WebDAV
# on its own host and tenant isolation.
#
#   scripts/dev.sh up            # the stack must be healthy
#   scripts/e2e_smoke.sh
#
# RUUM_DEV_SH names the dev.sh whose `env` to use (default scripts/dev.sh of this checkout), so a
# git worktree can run against the stack of the main checkout.
#
# Every run makes new users and a new tenant; nothing is deleted. The server log is kept in the
# work folder that the script prints.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PORT="${RUUM_E2E_PORT:-8491}"
BASE="http://127.0.0.1:$PORT"
DAV_PORT="${RUUM_E2E_DAV_PORT:-8493}"
DAV_BASE="http://127.0.0.1:$DAV_PORT"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ruum-e2e.XXXXXX")"
PASSED=0

# The shared cargo cache of this repository.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=false CARGO_PROFILE_TEST_DEBUG=false

say() { printf '\n== %s\n' "$*"; }
ok() { PASSED=$((PASSED + 1)); printf '   ok: %s\n' "$*"; }
fail() {
  printf '   FAIL: %s\n' "$*" >&2
  if [[ -s "$WORK/body" ]]; then
    printf '   last API body: %s\n' "$(head -c 600 "$WORK/body")" >&2
  fi
  if [[ -f "$WORK/server.log" ]]; then
    printf '   last server log lines:\n' >&2
    tail -n 20 "$WORK/server.log" >&2
  fi
  exit 1
}
expect() { # expect <want> <got> <what>
  [[ "$1" == "$2" ]] || fail "$3: want '$1', got '$2'"
  ok "$3"
}

SERVER_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# ------------------------------------------------------------------ settings
say "settings (work folder: $WORK)"
eval "$("${RUUM_DEV_SH:-scripts/dev.sh}" env)"
# The seed and the server must sign and check tokens with the same key; content URLs too.
export RUUM_TOKEN_SIGNING_KEY="${RUUM_TOKEN_SIGNING_KEY:-$(openssl rand 32 | openssl base64 -A | tr '+/' '-_' | tr -d '=')}"
export RUUM_CONTENT_SIGNING_KEY="${RUUM_CONTENT_SIGNING_KEY:-$(openssl rand -base64 32)}"
export RUUM_BIND="127.0.0.1:$PORT"
export RUUM_PUBLIC_URL="$BASE"
export RUUM_DAV_BIND="127.0.0.1:$DAV_PORT"
export RUUM_DAV_URL="$DAV_BASE/"
export RUST_LOG="${RUST_LOG:-info}"
if curl -fsS "$BASE/healthz" >/dev/null 2>&1; then
  fail "port $PORT is in use; set RUUM_E2E_PORT"
fi
if curl -s -o /dev/null "$DAV_BASE/" 2>/dev/null; then
  fail "port $DAV_PORT is in use; set RUUM_E2E_DAV_PORT"
fi
ok "stack env loaded, ports $PORT and $DAV_PORT free"

# ------------------------------------------------------------------ build and start
say "build"
cargo build -q -p ruum-server -p ruum-cli
SERVER_BIN="$CARGO_TARGET_DIR/debug/ruum-server"
RUUM_BIN="$CARGO_TARGET_DIR/debug/ruum"
ok "built ruum-server and ruum"

say "start the server"
"$SERVER_BIN" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 60); do
  curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break
  kill -0 "$SERVER_PID" 2>/dev/null || fail "the server stopped"
  sleep 0.5
done
expect 200 "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/healthz")" "GET /healthz"
grep -q "authz engine: SpiceDB" "$WORK/server.log" || fail "the server does not use SpiceDB"
grep -q "stores: Postgres" "$WORK/server.log" || fail "the server does not use the Postgres stores"
grep -q "no S3 settings" "$WORK/server.log" && fail "the server keeps chunks in memory, not in Garage"
ok "Postgres stores, SpiceDB, Garage chunks"

# ------------------------------------------------------------------ seed
say "seed (ruum-server seed-dev)"
"$SERVER_BIN" seed-dev "$WORK/seed.env" >>"$WORK/seed.log" 2>&1 || {
  cat "$WORK/seed.log" >&2
  fail "seed-dev"
}
# shellcheck disable=SC1091
source "$WORK/seed.env"
# The seed wrote its relationships to SpiceDB from another process. No wait: every request
# reads at least as fresh as its tenant's newest outbox token (decision 0002-4).
ok "tenant A $TENANT_A, Alice $ALICE_EMAIL, Bob (guest), Carol (tenant B), agent $AGENT_ID"

# curl helpers: api <token> <tenant> <method> <path> [json body] -> body in $WORK/body, status echoed
api() {
  local token="$1" tenant="$2" method="$3" path="$4" body="${5:-}"
  local args=(-s -o "$WORK/body" -w '%{http_code}' -X "$method" -H "Authorization: Bearer $token")
  [[ -n "$tenant" ]] && args+=(-H "Ruum-Tenant: $tenant")
  [[ -n "$body" ]] && args+=(-H 'Content-Type: application/json' --data "$body")
  curl "${args[@]}" "$BASE$path"
}
body() { jq -r "$1" "$WORK/body"; }

ruum() { "$RUUM_BIN" --credentials "$WORK/cli.json" --url "$BASE" "$@"; }

# ------------------------------------------------------------------ CLI: folders and files
say "CLI as the agent: login, folder, chunked upload, download"
ruum login --key "$AGENT_KEY" >"$WORK/out"
grep -q "signed in" "$WORK/out" || fail "ruum login: $(cat "$WORK/out")"
ok "ruum login with the agent key (token exchange)"

ruum --json mkdir -p /E2E/Shared >"$WORK/folder.json"
FOLDER="$(jq -r '.id // .node.id' "$WORK/folder.json")"
[[ "$FOLDER" =~ ^[0-9a-f-]{36}$ ]] || fail "mkdir: $(cat "$WORK/folder.json")"
ok "ruum mkdir -p /E2E/Shared ($FOLDER)"

# 3 MB of random bytes: many chunks with the small chunking parameters.
head -c 3145728 /dev/urandom >"$WORK/data.bin"
ruum upload "$WORK/data.bin" /E2E/Shared/ >"$WORK/out"
grep -q "uploaded" "$WORK/out" || fail "upload: $(cat "$WORK/out")"
CHUNKS="$(sed -E 's/.* of ([0-9]+) chunks sent.*/\1/' "$WORK/out")"
[[ "$CHUNKS" -gt 1 ]] || fail "the upload has $CHUNKS chunk(s): $(cat "$WORK/out")"
ok "ruum upload: $(cat "$WORK/out")"

ruum download /E2E/Shared/data.bin "$WORK/back.bin" >/dev/null
expect "$(shasum -a 256 <"$WORK/data.bin")" "$(shasum -a 256 <"$WORK/back.bin")" "download has the same SHA-256"

ruum --json ls /E2E/Shared >"$WORK/ls.json"
FILE="$(jq -r '.[] | select(.name == "data.bin") | .id' "$WORK/ls.json")"
[[ -n "$FILE" ]] || fail "ls: $(cat "$WORK/ls.json")"
ok "ruum ls shows data.bin ($FILE)"

# ------------------------------------------------------------------ versions
say "versions"
# Saves by the same actor within 10 minutes merge (design 5.3); a named version never merges.
V1="$(ruum --json versions /E2E/Shared/data.bin | jq -r '.[0].id')"
expect 200 "$(api "$ALICE_TOKEN" "$TENANT_A" PUT "/v1/nodes/$FILE/versions/$V1/label" '{"label":"first"}')" "Alice names version 1"
head -c 1048576 /dev/urandom >>"$WORK/data.bin"
ruum upload "$WORK/data.bin" /E2E/Shared/data.bin >"$WORK/out"
ok "ruum upload of a second version: $(cat "$WORK/out")"
ruum --json versions /E2E/Shared/data.bin >"$WORK/versions.json"
expect 2 "$(jq length "$WORK/versions.json")" "ruum versions lists 2 versions"
expect first "$(jq -r '.[1].label' "$WORK/versions.json")" "the older version keeps its name"
ruum download /E2E/Shared/data.bin "$WORK/back2.bin" >/dev/null
expect "$(shasum -a 256 <"$WORK/data.bin")" "$(shasum -a 256 <"$WORK/back2.bin")" "download of version 2 has the same SHA-256"
ruum download --version "$V1" /E2E/Shared/data.bin "$WORK/back1.bin" >/dev/null
expect "$(shasum -a 256 <"$WORK/back.bin")" "$(shasum -a 256 <"$WORK/back1.bin")" "download of version 1 has the old content"

# ------------------------------------------------------------------ copy
say "copy (shared chunks, one version)"
REQ="$(jq -nc --arg p "$FOLDER" '{parent_id: $p, name: "copy.bin"}')"
expect 201 "$(api "$ALICE_TOKEN" "$TENANT_A" POST "/v1/nodes/$FILE/copy" "$REQ")" "Alice copies data.bin"
COPY="$(body '.node.id')"
expect 200 "$(api "$ALICE_TOKEN" "$TENANT_A" GET "/v1/nodes/$COPY/versions")" "list the versions of the copy"
expect 1 "$(body '.items | length')" "the copy has one version"
curl -s -H "Authorization: Bearer $ALICE_TOKEN" -H "Ruum-Tenant: $TENANT_A" "$BASE/v1/nodes/$COPY/content" -o "$WORK/copy.bin"
expect "$(shasum -a 256 <"$WORK/data.bin")" "$(shasum -a 256 <"$WORK/copy.bin")" "the copy has the same content"

# ------------------------------------------------------------------ sharing
say "sharing"
expect 404 "$(api "$BOB_TOKEN" "$TENANT_A" GET "/v1/nodes/$FOLDER")" "Bob (guest, no grant) cannot see the folder"
ruum share /E2E/Shared --user "$BOB_ID" --role view >"$WORK/out"
ok "ruum share /E2E/Shared --user Bob --role view: $(cat "$WORK/out")"
expect 200 "$(api "$BOB_TOKEN" "$TENANT_A" GET "/v1/nodes/$FOLDER")" "Bob sees the folder"
expect view "$(body '.my_role')" "Bob's role is view"
expect 200 "$(api "$BOB_TOKEN" "$TENANT_A" GET "/v1/nodes/$FOLDER/children")" "Bob lists the folder"
curl -s -H "Authorization: Bearer $BOB_TOKEN" -H "Ruum-Tenant: $TENANT_A" "$BASE/v1/nodes/$FILE/content" -o "$WORK/bob.bin"
expect "$(shasum -a 256 <"$WORK/data.bin")" "$(shasum -a 256 <"$WORK/bob.bin")" "Bob downloads the file"
REQ="$(jq -nc --arg p "$FOLDER" '{parent_id: $p, name: "bob.txt", kind: "file"}')"
expect 403 "$(api "$BOB_TOKEN" "$TENANT_A" POST "/v1/nodes" "$REQ")" "Bob (view) cannot create a file there"
expect 200 "$(api "$BOB_TOKEN" "" GET "/v1/shared-with-me")" "Bob lists Shared with me"
[[ "$(body "[.items[] | select(.node.id == \"$FOLDER\" or .node_id == \"$FOLDER\")] | length")" -ge 1 ]] ||
  fail "Shared with me does not list the folder: $(cat "$WORK/body")"
ok "Shared with me lists the folder"

# ------------------------------------------------------------------ share email (Mailpit)
say "share email to Mailpit (decision 0003-3)"
MAILPIT="${RUUM_E2E_MAILPIT_URL:-http://127.0.0.1:8025}"
SHARE_TO="e2e-share-$(openssl rand -hex 6)@example.test"
REQ="$(jq -nc --arg e "$SHARE_TO" '{grantee: {type: "email", email: $e}, role: "view", message: "Hello from the smoke test"}')"
expect 201 "$(api "$ALICE_TOKEN" "$TENANT_A" POST "/v1/nodes/$FOLDER/grants" "$REQ")" "Alice shares the folder with a new address"
expect email "$(body '.grant.grantee.type')" "the grant stays an email grant"
expect true "$(body '.notified')" "the answer says a share email was queued"
MAIL_ID=""
for _ in $(seq 1 40); do
  curl -fsS -G "$MAILPIT/api/v1/search" --data-urlencode "query=to:\"$SHARE_TO\"" >"$WORK/mails.json" 2>/dev/null || true
  MAIL_ID="$(jq -r '.messages[0].ID // empty' "$WORK/mails.json" 2>/dev/null || true)"
  [[ -n "$MAIL_ID" ]] && break
  sleep 0.5
done
[[ -n "$MAIL_ID" ]] || fail "no share email to $SHARE_TO in Mailpit ($MAILPIT)"
ok "the share email reached Mailpit"
curl -fsS "$MAILPIT/api/v1/message/$MAIL_ID" >"$WORK/mail.json"
jq -r '.Subject' "$WORK/mail.json" | grep -q 'shared "Shared" with you' || fail "share email subject: $(jq -r '.Subject' "$WORK/mail.json")"
ok "the subject names the folder: $(jq -r '.Subject' "$WORK/mail.json")"
jq -r '.Text' "$WORK/mail.json" | grep -qF "$BASE/t/$TENANT_A/files/$FOLDER" || fail "share email has no item link: $(jq -r '.Text' "$WORK/mail.json")"
ok "the mail links to the folder page"
jq -r '.Text' "$WORK/mail.json" | grep -q "Hello from the smoke test" || fail "share email has no message"
jq -r '.Text, .HTML' "$WORK/mail.json" | grep -qi "token" && fail "the share email carries a token"
[[ -n "$(jq -r '.HTML // empty' "$WORK/mail.json")" ]] || fail "the share email has no HTML part"
ok "the mail has the message, an HTML part and no token"

# ------------------------------------------------------------------ changes
say "changes feed"
ruum changes >"$WORK/changes.txt"
for t in node.created version.created version.labeled node.copied grant.added; do
  grep -q "$t" "$WORK/changes.txt" || fail "ruum changes has no $t: $(cat "$WORK/changes.txt")"
done
ok "ruum changes has node.created, version.created, version.labeled, node.copied, grant.added"

# ------------------------------------------------------------------ MCP
say "MCP over /mcp with the agent token"
curl -s -X POST "$BASE/v1/oauth/token" -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-urlencode 'grant_type=urn:ietf:params:oauth:grant-type:token-exchange' \
  --data-urlencode "subject_token=$AGENT_KEY" \
  --data-urlencode 'subject_token_type=urn:ruum:params:oauth:token-type:api-key' >"$WORK/token.json"
AGENT_TOKEN="$(jq -r .access_token "$WORK/token.json")"
[[ -n "$AGENT_TOKEN" && "$AGENT_TOKEN" != null ]] || fail "token exchange: $(cat "$WORK/token.json")"
ok "token exchange for the agent key"
mcp() {
  curl -s -X POST "$BASE/mcp" -H "Authorization: Bearer $AGENT_TOKEN" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    --data "$1" | sed -n 's/^data: *//p; /^{/p' | tail -n 1
}
expect 401 "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/mcp" -H 'Content-Type: application/json' --data '{}')" "/mcp without a token is 401"
mcp '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}' >"$WORK/mcp-init.json"
jq -e '.result.serverInfo' "$WORK/mcp-init.json" >/dev/null || fail "initialize: $(cat "$WORK/mcp-init.json")"
ok "MCP initialize"
mcp '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' >"$WORK/mcp-tools.json"
TOOLS="$(jq '.result.tools | length' "$WORK/mcp-tools.json")"
[[ "$TOOLS" -gt 10 ]] || fail "tools/list: $(head -c 500 "$WORK/mcp-tools.json")"
ok "MCP tools/list: $TOOLS tools"
REQ="$(jq -nc --arg n "$FOLDER" '{jsonrpc: "2.0", id: 3, method: "tools/call", params: {name: "list_children", arguments: {node_id: $n}}}')"
mcp "$REQ" >"$WORK/mcp-call.json"
expect false "$(jq -r '.result.isError' "$WORK/mcp-call.json")" "MCP tools/call list_children succeeds"
jq -e '.result.structuredContent.items[] | select(.name == "data.bin")' "$WORK/mcp-call.json" >/dev/null ||
  fail "list_children has no data.bin: $(head -c 500 "$WORK/mcp-call.json")"
ok "list_children over MCP shows data.bin"

# ------------------------------------------------------------------ WebDAV
say "WebDAV on its own host ($DAV_BASE) with Alice's app password"
DAV=(-s -u "$ALICE_EMAIL:$APP_PASSWORD")
expect 404 "$(curl "${DAV[@]}" -o /dev/null -w '%{http_code}' -X PROPFIND -H 'Depth: 1' "$BASE/dav/E2E/")" "the platform origin has no /dav (404)"
expect 201 "$(api "$ALICE_TOKEN" "$TENANT_A" POST "/v1/me/app-passwords" '{"name":"e2e mount"}')" "Alice makes another app password"
expect "$DAV_BASE/" "$(body '.dav_url')" "the answer names the WebDAV host (dav_url)"
expect 401 "$(curl -s -o /dev/null -w '%{http_code}' -X PROPFIND -H 'Depth: 1' "$DAV_BASE/E2E/")" "PROPFIND without credentials is 401"
expect 207 "$(curl "${DAV[@]}" -D "$WORK/dav-headers" -o "$WORK/propfind.xml" -w '%{http_code}' -X PROPFIND -H 'Depth: 1' "$DAV_BASE/E2E/")" "PROPFIND /E2E/ on the WebDAV host"
grep -q "Shared" "$WORK/propfind.xml" || fail "PROPFIND does not list Shared: $(head -c 800 "$WORK/propfind.xml")"
ok "PROPFIND lists Shared"
grep -qi '^set-cookie' "$WORK/dav-headers" && fail "the WebDAV host sets a cookie: $(cat "$WORK/dav-headers")"
grep -qi '^content-security-policy: sandbox' "$WORK/dav-headers" || fail "no sandbox CSP on the WebDAV host: $(cat "$WORK/dav-headers")"
ok "the WebDAV host sets no cookie and sends the sandbox CSP"
printf 'hello from webdav\n' >"$WORK/dav.txt"
expect 201 "$(curl "${DAV[@]}" -o /dev/null -w '%{http_code}' -T "$WORK/dav.txt" "$DAV_BASE/E2E/Shared/dav.txt")" "PUT /E2E/Shared/dav.txt"
expect "hello from webdav" "$(curl "${DAV[@]}" "$DAV_BASE/E2E/Shared/dav.txt")" "GET the file back over WebDAV"
ruum ls /E2E/Shared >"$WORK/out"
grep -q "dav.txt" "$WORK/out" || fail "the CLI does not see dav.txt: $(cat "$WORK/out")"
ok "the CLI sees the file written over WebDAV"
expect 404 "$(curl "${DAV[@]}" -o /dev/null -w '%{http_code}' "$DAV_BASE/v1/me")" "the WebDAV host has no REST API (/v1/me is 404)"
expect 401 "$(api "$APP_PASSWORD" "$TENANT_A" GET "/v1/me")" "the REST API refuses the app password"

# ------------------------------------------------------------------ isolation
say "tenant isolation"
expect 404 "$(api "$CAROL_TOKEN" "$TENANT_B" GET "/v1/nodes/$FOLDER")" "Carol (tenant B) gets 404 for tenant A's folder"
expect 404 "$(api "$CAROL_TOKEN" "$TENANT_B" GET "/v1/nodes/$FILE/content")" "Carol gets 404 for tenant A's file content"
STATUS="$(api "$CAROL_TOKEN" "$TENANT_A" GET "/v1/nodes/$FOLDER")"
[[ "$STATUS" == 403 || "$STATUS" == 404 ]] || fail "Carol with Ruum-Tenant A: $STATUS $(cat "$WORK/body")"
ok "Carol with Ruum-Tenant A is refused ($STATUS $(body '.error.code'))"
mcp_b="$(curl -s -o /dev/null -w '%{http_code}' -X PROPFIND -u "$ALICE_EMAIL:wrong" "$DAV_BASE/")"
expect 401 "$mcp_b" "WebDAV with a wrong app password is 401"

say "PASSED: $PASSED checks"
