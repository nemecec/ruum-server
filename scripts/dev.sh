#!/usr/bin/env bash
# The Ruum laptop stack (docs/DESIGN.md section 12.2, docs/DEVSTACK.md).
#
#   scripts/dev.sh up [--step4]   create .env on the first run, start, wait until healthy, init
#   scripts/dev.sh down           stop the stack (keeps the data)
#   scripts/dev.sh clean          stop the stack and delete all its data
#   scripts/dev.sh logs [svc]     follow the logs
#   scripts/dev.sh status         show the services and their health
#   scripts/dev.sh psql [db]      open psql as the ruum role (default database: ruum)
#   scripts/dev.sh seed           create missing databases and roles; later also tenants, users and files
#   scripts/dev.sh env            print the settings for the Ruum server (eval "$(scripts/dev.sh env)")
#   scripts/dev.sh ca             copy Caddy's local root certificate to deploy/dev/.state/caddy-root.crt
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEV="$ROOT/deploy/dev"
ENV_FILE="$DEV/.env"
STATE="$DEV/.state"
ZITADEL_URL="http://localhost:8180"

# No provenance attestation: it carries a build time, so every `up --build` would give the postgres
# image a new ID and recreate the container (seen with buildx and the containerd image store).
compose() { BUILDX_NO_DEFAULT_ATTESTATIONS=1 docker compose --project-directory "$DEV" -f "$DEV/docker-compose.yml" --env-file "$ENV_FILE" "$@"; }
log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }

# Replace each @random-hexN@ marker of a line with N random hex characters.
fill_random() {
  local line=$1 n
  while [[ "$line" =~ @random-hex([0-9]+)@ ]]; do
    n="${BASH_REMATCH[1]}"
    line="${line/"${BASH_REMATCH[0]}"/$(openssl rand -hex $(((n + 1) / 2)) | cut -c1-"$n")}"
  done
  printf '%s\n' "$line"
}

# Copy .env.example to .env with new random values. An existing .env gets the settings that
# .env.example has and it lacks (for example the passwords of a service added later).
ensure_env() {
  need openssl
  local tmp line key added=()
  if [[ ! -f "$ENV_FILE" ]]; then
    log "creating deploy/dev/.env with new random secrets"
    tmp="$(mktemp)"
    while IFS= read -r line || [[ -n "$line" ]]; do
      fill_random "$line" >>"$tmp"
    done <"$DEV/.env.example"
    mv "$tmp" "$ENV_FILE"
    chmod 600 "$ENV_FILE"
    return
  fi
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^([A-Z0-9_]+)= ]] || continue
    key="${BASH_REMATCH[1]}"
    grep -q "^$key=" "$ENV_FILE" && continue
    fill_random "$line" >>"$ENV_FILE"
    added+=("$key")
  done <"$DEV/.env.example"
  ((${#added[@]} == 0)) || log "added to deploy/dev/.env: ${added[*]}"
}

load_env() {
  [[ -f "$ENV_FILE" ]] || die "deploy/dev/.env is missing; run scripts/dev.sh up"
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
}

# Wait until every running service of the stack is healthy (services without a healthcheck count when running).
wait_healthy() {
  local deadline=$((SECONDS + ${1:-300})) bad
  while :; do
    bad="$(compose ps --format '{{.Service}} {{.State}} {{.Health}}' | awk '$2 != "running" || ($3 != "" && $3 != "healthy") {print $1 "(" $2 ($3 == "" ? "" : "," $3) ")"}')"
    [[ -z "$bad" ]] && return 0
    if ((SECONDS > deadline)); then
      compose ps >&2
      die "not healthy in time: $bad"
    fi
    sleep 3
  done
}

# Run a setup script in a Postgres service. A fresh volume runs the image's first-time setup on a
# temporary server and then restarts; a connection in that gap fails with "shutting down". Retry a
# few times instead of failing the whole start (seen on CI, where every run starts from scratch).
pg_exec_retry() {
  service="$1"; script="$2"; tries=0
  until compose exec -T -e PGOPTIONS="-c client_min_messages=warning" "$service" bash "$script" >/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -ge 20 ] && die "$service: $script failed 20 times"
    log "$service is not ready yet; retrying in 3 s"
    sleep 3
  done
}

ensure_databases() {
  log "creating missing roles, databases and extensions"
  pg_exec_retry postgres /docker-entrypoint-initdb.d/10-ruum.sh
}

# The data cluster: the role ruum_dbhost (password from .env) and the closed shared databases.
ensure_data_cluster() {
  log "data cluster: ruum_dbhost and the closed template1"
  pg_exec_retry postgres-data /docker-entrypoint-initdb.d/10-ruum-data.sh
}

garage() { compose exec -T -e RUST_LOG=warn garage /garage "$@"; }

init_garage() {
  load_env
  log "garage: layout, access key and bucket"
  local node
  if ! garage layout show 2>/dev/null | grep -q 'Current cluster layout version: [1-9]'; then
    node="$(garage node id -q | cut -d@ -f1)"
    garage layout assign -z dc1 -c 2G "$node" >/dev/null
    garage layout apply --version 1 >/dev/null
  fi
  if ! garage key info "$S3_ACCESS_KEY_ID" >/dev/null 2>&1; then
    garage key import --yes -n ruum-dev "$S3_ACCESS_KEY_ID" "$S3_SECRET_ACCESS_KEY" >/dev/null
  fi
  if ! garage bucket info "$S3_BUCKET" >/dev/null 2>&1; then
    garage bucket create "$S3_BUCKET" >/dev/null
  fi
  garage bucket allow --read --write --owner "$S3_BUCKET" --key "$S3_ACCESS_KEY_ID" >/dev/null
}

# Zitadel management API as the bootstrap service account.
zapi() {
  local method="$1" path="$2" body="${3:-}"
  curl -fsS -X "$method" "$ZITADEL_URL$path" \
    -H "Authorization: Bearer $(cat "$STATE/zitadel/bootstrap.pat")" \
    -H "Content-Type: application/json" ${body:+-d "$body"}
}

# Creates the project "Ruum" with two applications and writes their IDs to .state/zitadel/apps.env:
#   ruum-web: public OIDC client (authorization code + PKCE) for the web UI and the CLI
#   ruum-api: API client (basic auth) that the Ruum server uses to introspect tokens
init_zitadel() {
  need jq
  local out="$STATE/zitadel/apps.env"
  [[ -s "$out" ]] && return
  log "zitadel: project Ruum and OIDC applications"
  local i=0
  until [[ -s "$STATE/zitadel/bootstrap.pat" ]]; do
    ((i++ > 40)) && die "zitadel did not write the bootstrap token to $STATE/zitadel/bootstrap.pat"
    sleep 2
  done
  local project web api
  project="$(zapi POST /management/v1/projects/_search \
    '{"queries":[{"nameQuery":{"name":"Ruum","method":"TEXT_QUERY_METHOD_EQUALS"}}]}' | jq -r '.result[0].id // empty')"
  if [[ -z "$project" ]]; then
    project="$(zapi POST /management/v1/projects '{"name":"Ruum","projectRoleAssertion":true}' | jq -r .id)"
  fi
  web="$(zapi POST "/management/v1/projects/$project/apps/oidc" "$(jq -n '{
      name: "ruum-web",
      redirectUris: ["https://ruum.localhost/v1/auth/callback", "http://127.0.0.1:8490/v1/auth/callback", "https://ruum.localhost/auth/callback", "http://localhost:5173/auth/callback", "http://127.0.0.1:8976/callback"],
      postLogoutRedirectUris: ["https://ruum.localhost/", "http://localhost:5173/"],
      responseTypes: ["OIDC_RESPONSE_TYPE_CODE"],
      grantTypes: ["OIDC_GRANT_TYPE_AUTHORIZATION_CODE", "OIDC_GRANT_TYPE_REFRESH_TOKEN", "OIDC_GRANT_TYPE_DEVICE_CODE"],
      appType: "OIDC_APP_TYPE_USER_AGENT",
      authMethodType: "OIDC_AUTH_METHOD_TYPE_NONE",
      accessTokenType: "OIDC_TOKEN_TYPE_JWT",
      idTokenUserinfoAssertion: true,
      accessTokenRoleAssertion: true,
      devMode: true
    }')")"
  api="$(zapi POST "/management/v1/projects/$project/apps/api" \
    '{"name":"ruum-api","authMethodType":"API_AUTH_METHOD_TYPE_BASIC"}')"
  umask 077
  {
    echo "ZITADEL_PROJECT_ID=$project"
    echo "ZITADEL_WEB_CLIENT_ID=$(jq -r .clientId <<<"$web")"
    echo "ZITADEL_API_CLIENT_ID=$(jq -r .clientId <<<"$api")"
    echo "ZITADEL_API_CLIENT_SECRET=$(jq -r .clientSecret <<<"$api")"
  } >"$out"
}

cmd_up() {
  need docker
  ensure_env
  mkdir -p "$STATE/zitadel"
  local profile=()
  [[ "${1:-}" == "--step4" ]] && profile=(--profile step4)
  log "building and starting postgres and postgres-data"
  compose up -d --build postgres postgres-data
  wait_healthy 120
  ensure_databases
  ensure_data_cluster
  log "starting the stack"
  compose ${profile[@]+"${profile[@]}"} up -d
  wait_healthy 300
  init_garage
  init_zitadel
  log "all services are healthy"
  compose ${profile[@]+"${profile[@]}"} ps --format 'table {{.Service}}\t{{.Status}}'
}

cmd_seed() {
  ensure_databases
  ensure_data_cluster
  # Later (section 12.2): two tenants, three users, an agent key and sample files,
  # created through the Ruum API (`ruum` CLI) once the core service exists.
  log "seed: databases are ready; tenant and user seed comes with the core service"
}

cmd_env() {
  load_env
  local apps="$STATE/zitadel/apps.env"
  # shellcheck disable=SC1090
  [[ -f "$apps" ]] && source "$apps"
  local dav_upstream=${RUUM_DAV_UPSTREAM:-host.docker.internal:8492}
  cat <<EOF
export DATABASE_URL=postgres://ruum:$RUUM_DB_PASSWORD@127.0.0.1:5434/ruum
export TEST_DATABASE_URL=postgres://ruum:$RUUM_DB_PASSWORD@127.0.0.1:5434/ruum_test
export RUUM_DBHOST_URL=postgres://ruum_dbhost:${RUUM_DBHOST_PASSWORD:-}@127.0.0.1:${POSTGRES_DATA_PORT:-5435}/postgres
export S3_ENDPOINT=http://127.0.0.1:3900
export S3_REGION=garage
export S3_BUCKET=$S3_BUCKET
export S3_ACCESS_KEY_ID=$S3_ACCESS_KEY_ID
export S3_SECRET_ACCESS_KEY=$S3_SECRET_ACCESS_KEY
export S3_FORCE_PATH_STYLE=true
export SPICEDB_ENDPOINT=http://127.0.0.1:50051
export SPICEDB_PRESHARED_KEY=$SPICEDB_PRESHARED_KEY
export ZITADEL_ISSUER=$ZITADEL_URL
export ZITADEL_PROJECT_ID=${ZITADEL_PROJECT_ID:-}
export ZITADEL_WEB_CLIENT_ID=${ZITADEL_WEB_CLIENT_ID:-}
export ZITADEL_API_CLIENT_ID=${ZITADEL_API_CLIENT_ID:-}
export ZITADEL_API_CLIENT_SECRET=${ZITADEL_API_CLIENT_SECRET:-}
export ZITADEL_SERVICE_PAT_FILE=$STATE/zitadel/bootstrap.pat
export NATS_URL=nats://127.0.0.1:4222
export BAO_ADDR=http://127.0.0.1:8200
export BAO_TOKEN=$BAO_DEV_ROOT_TOKEN
export SMTP_URL=smtp://127.0.0.1:1025
export RUUM_PUBLIC_URL=https://ruum.localhost
export RUUM_APPS_DOMAIN=ruumapps.localhost
export RUUM_BIND=127.0.0.1:${RUUM_UPSTREAM##*:}
export RUUM_DAV_BIND=127.0.0.1:${dav_upstream##*:}
export RUUM_DAV_URL=https://dav.ruum.localhost/
export RUUM_CONTENT_URL=https://content.ruumapps.localhost
export RUUM_OIDC_ISSUER=$ZITADEL_URL
export RUUM_OIDC_CLIENT_ID=${ZITADEL_WEB_CLIENT_ID:-}
export RUUM_S3_ENDPOINT=http://127.0.0.1:3900
export RUUM_S3_BUCKET=$S3_BUCKET
export RUUM_SPICEDB_ENDPOINT=http://127.0.0.1:50051
EOF
}

cmd_ca() {
  mkdir -p "$STATE"
  compose cp caddy:/data/caddy/pki/authorities/local/root.crt "$STATE/caddy-root.crt" >/dev/null 2>&1
  echo "$STATE/caddy-root.crt"
}

main() {
  local cmd="${1:-}"
  shift || true
  case "$cmd" in
    up) cmd_up "$@" ;;
    down) ensure_env; compose --profile step4 down ;;
    clean)
      ensure_env
      compose --profile step4 down -v --remove-orphans
      rm -rf "$STATE"
      log "volumes and deploy/dev/.state deleted (deploy/dev/.env is kept)"
      ;;
    logs) compose logs -f --tail=200 "$@" ;;
    status) compose --profile step4 ps --format 'table {{.Service}}\t{{.Status}}\t{{.Ports}}' ;;
    psql)
      load_env
      local tty=()
      [[ -t 0 ]] || tty=(-T)
      compose exec ${tty[@]+"${tty[@]}"} -e PGPASSWORD="$RUUM_DB_PASSWORD" postgres psql -h 127.0.0.1 -U ruum "${1:-ruum}"
      ;;
    seed) cmd_seed ;;
    env) cmd_env ;;
    ca) cmd_ca ;;
    *) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; [[ -z "$cmd" ]] || exit 1 ;;
  esac
}

main "$@"
