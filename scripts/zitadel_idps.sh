#!/usr/bin/env bash
# Sign-in providers of the laptop Zitadel (design 4.2, docs/DEVSTACK.md "Sign-in providers").
#
#   scripts/zitadel_idps.sh          configure the providers whose keys are present; idempotent
#   scripts/zitadel_idps.sh check    start each provider's flow without a password, stop at the provider
#
# `scripts/dev.sh up` runs it after the Zitadel init step, and `scripts/dev.sh idps` runs it alone.
#
# - Google: when GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET are set.
# - Microsoft: when MS_OAUTH_CLIENT_ID and MS_OAUTH_CLIENT_SECRET are set. The "common" endpoint,
#   so work, school and personal Microsoft accounts can sign in.
# - Mock SSO: when the mock OIDC provider of the compose profile `sso-mock` answers on port 8190.
#
# Each provider is an instance-wide identity provider, and it is added to the default login
# policy. A provider that exists is updated (a new secret takes effect). Without keys, nothing is
# changed: a provider that was added before stays.
#
# The keys come from the environment, else from deploy/dev/.env, else from the secrets file
# (RUUM_SECRETS_FILE, default ~/.config/ruum/secrets.env). Only these four names are read from
# that file. The values go to Zitadel on stdin of curl; they are never printed, logged or written
# to the repository.
#
# Settings:
#   RUUM_DEV_ROOT=<path>     the checkout whose deploy/dev holds the stack's .env and .state
#                            (default: this checkout, or the main worktree of a git worktree)
#   ZITADEL_URL=<url>        default http://localhost:8180
#   RUUM_SSO_MOCK_URL=<url>  the issuer of the mock provider, default http://oidc-mock.localhost:8190/ruum
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZITADEL_URL="${ZITADEL_URL:-http://localhost:8180}"
SECRETS_FILE="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
MOCK_ISSUER="${RUUM_SSO_MOCK_URL:-http://oidc-mock.localhost:8190/ruum}"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || die "jq is required"
command -v curl >/dev/null 2>&1 || die "curl is required"

DEV_ROOT="${RUUM_DEV_ROOT:-$ROOT}"
if [[ ! -f "$DEV_ROOT/deploy/dev/.env" ]]; then
  # A git worktree has no deploy/dev/.env of its own; the stack belongs to the main worktree.
  main="$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
  [[ -n "$main" && -f "${main%/.git}/deploy/dev/.env" ]] && DEV_ROOT="${main%/.git}"
fi
ENV_FILE="$DEV_ROOT/deploy/dev/.env"
PAT_FILE="$DEV_ROOT/deploy/dev/.state/zitadel/bootstrap.pat"
[[ -s "$PAT_FILE" ]] || die "no Zitadel bootstrap token at $PAT_FILE; run scripts/dev.sh up"

# The value of NAME: the environment first, then deploy/dev/.env, then the secrets file. The files
# are read line by line for that one name, never sourced, so no other secret gets into the shell.
key() {
  local name="$1" file line value
  if [[ -n "${!name:-}" ]]; then
    printf '%s' "${!name}"
    return
  fi
  for file in "$ENV_FILE" "$SECRETS_FILE"; do
    [[ -r "$file" ]] || continue
    line="$(grep -E "^(export +)?$name=" "$file" | tail -n 1 || true)"
    [[ -n "$line" ]] || continue
    value="${line#*=}"
    value="${value%$'\r'}"
    # Strip one pair of surrounding quotes.
    if [[ "$value" =~ ^\"(.*)\"$ || "$value" =~ ^\'(.*)\'$ ]]; then value="${BASH_REMATCH[1]}"; fi
    if [[ -n "$value" ]]; then
      printf '%s' "$value"
      return
    fi
  done
}

# zapi METHOD PATH: the body comes on stdin, so secrets stay out of the process list (a GET sends
# none). Prints the answer; fails with the HTTP status and Zitadel's message, never the request body.
zapi() {
  local method="$1" path="$2" out status
  local data=(--data-binary @-)
  [[ "$method" == GET ]] && data=()
  out="$(mktemp)"
  status="$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$ZITADEL_URL$path" \
    -H "Authorization: Bearer $(cat "$PAT_FILE")" -H "Content-Type: application/json" ${data[@]+"${data[@]}"})" || {
    rm -f "$out"
    die "$method $path: Zitadel does not answer at $ZITADEL_URL"
  }
  if [[ "$status" != 2* ]]; then
    # An update without a change is not an error ("No changes", gRPC code 9).
    if [[ "$method" == PUT ]] && jq -e '.code == 9' "$out" >/dev/null 2>&1; then
      rm -f "$out"
      echo '{}'
      return
    fi
    local msg
    msg="$(jq -r '.message // empty' "$out" 2>/dev/null || true)"
    rm -f "$out"
    die "$method $path: HTTP $status ${msg:+($msg)}"
  fi
  cat "$out"
  rm -f "$out"
}

# The ID of the instance identity provider with this exact name, or nothing.
idp_id() {
  jq -n --arg name "$1" '{queries: [{idpNameQuery: {name: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}' |
    zapi POST /admin/v1/idps/templates/_search | jq -r '.result[0]?.id // empty'
}

# upsert KIND NAME BODY_JSON: create or update the provider; prints its ID.
upsert() {
  local kind="$1" name="$2" body="$3" id
  id="$(idp_id "$name")"
  if [[ -z "$id" ]]; then
    id="$(printf '%s' "$body" | zapi POST "/admin/v1/idps/$kind" | jq -r .id)"
    [[ -n "$id" && "$id" != null ]] || die "Zitadel gave no ID for the provider $name"
    log "zitadel: added the sign-in provider $name"
  else
    printf '%s' "$body" | zapi PUT "/admin/v1/idps/$kind/$id" >/dev/null
    log "zitadel: updated the sign-in provider $name"
  fi
  printf '%s' "$id"
}

# Put the provider on the default login policy of the instance (the login page shows it).
ensure_on_login_policy() {
  local id="$1" name="$2"
  if echo '{}' | zapi POST /admin/v1/policies/login/idps/_search | jq -e --arg id "$id" '.result // [] | any(.idpId == $id)' >/dev/null; then
    return
  fi
  jq -n --arg id "$id" '{idpId: $id}' | zapi POST /admin/v1/policies/login/idps >/dev/null
  log "zitadel: $name is on the login page"
}

# The login policy must allow external providers (the default of a new instance does).
ensure_external_allowed() {
  local policy
  policy="$(zapi GET /admin/v1/policies/login | jq -c .policy)"
  printf '%s' "$policy" | jq -e '.allowExternalIdp == true' >/dev/null && return
  jq '{allowUsernamePassword, allowRegister, allowExternalIdp: true, forceMfa, passwordlessType,
       hidePasswordReset, ignoreUnknownUsernames, defaultRedirectUri, passwordCheckLifetime,
       externalLoginCheckLifetime, mfaInitSkipLifetime, secondFactorCheckLifetime,
       multiFactorCheckLifetime, allowDomainDiscovery, disableLoginWithEmail,
       disableLoginWithPhone, forceMfaLocalOnly} | with_entries(select(.value != null))' < <(printf '%s' "$policy") |
    zapi PUT /admin/v1/policies/login >/dev/null
  log "zitadel: the login policy allows external providers"
}

# Provider options (design 4.2): a person who signs in for the first time gets an account (they
# confirm the name and email on Zitadel's page first), and can link the provider to an existing
# account by signing in to that account. No automatic linking by email: that allows account
# takeover. Zitadel updates the name and email from the provider at each sign-in.
options() {
  jq -n --argjson auto "${1:-false}" '{
    isLinkingAllowed: true,
    isCreationAllowed: true,
    isAutoCreation: $auto,
    isAutoUpdate: true,
    autoLinking: "AUTO_LINKING_OPTION_UNSPECIFIED"
  }'
}


# check: start the flow of each provider on the login page the way a browser does, without any
# password, and stop at the provider. Prints the HTTP status of each step, the provider's host
# and path, whether client_id is the configured one, and redirect_uri. Then one request to the
# provider shows whether it knows the client and the redirect URI. Never prints a secret.
check() {
  local web_client verifier challenge jar loc rid page providers failed=0
  web_client="$(grep -E '^ZITADEL_WEB_CLIENT_ID=' "$DEV_ROOT/deploy/dev/.state/zitadel/apps.env" 2>/dev/null | cut -d= -f2-)"
  [[ -n "$web_client" ]] || die "no ruum-web client in deploy/dev/.state/zitadel/apps.env; run scripts/dev.sh up"
  jar="$(mktemp)"
  page="$(mktemp)"
  verifier="$(openssl rand -hex 32)"
  challenge="$(printf '%s' "$verifier" | openssl dgst -sha256 -binary | openssl base64 -A | tr '+/' '-_' | tr -d '=')"
  loc="$(curl -sS -o /dev/null -c "$jar" -b "$jar" -w '%{redirect_url}' \
    "$ZITADEL_URL/oauth/v2/authorize?client_id=$web_client&redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Fauth%2Fcallback&response_type=code&scope=openid%20email%20profile&code_challenge=$challenge&code_challenge_method=S256")"
  rid="${loc##*authRequestID=}"
  [[ -n "$loc" && "$rid" != "$loc" ]] || die "Zitadel did not start a login (authorize answered no redirect to the login UI)"
  curl -sS -c "$jar" -b "$jar" -o "$page" "$loc"
  # The login page lists one link per provider: /ui/login/login/externalidp?...&idpConfigID=<id>
  providers="$(echo '{}' | zapi POST /admin/v1/policies/login/idps/_search | jq -r '.result // [] | .[] | "\(.idpId)\t\(.idpName)"')"
  [[ -n "$providers" ]] || { rm -f "$jar" "$page"; die "no provider on the login policy"; }
  local idp name url code next host expected q
  while IFS=$'\t' read -r idp name; do
    if ! grep -q "idpConfigID=$idp" "$page"; then
      echo "$name: FAIL, not on the login page"
      failed=1
      continue
    fi
    echo "$name: on the login page"
    url="$ZITADEL_URL/ui/login/login/externalidp?authRequestID=$rid&idpConfigID=$idp"
    next=""
    for _ in 1 2 3 4 5; do
      read -r code next < <(curl -sS -o /dev/null -c "$jar" -b "$jar" -w '%{http_code} %{redirect_url}\n' "$url")
      host="$(sed -E 's#^https?://([^/?]+).*#\1#' <<<"$next")"
      echo "  $code -> ${next%%\?*}"
      [[ -n "$next" && "$host" == "${ZITADEL_URL#*://}" ]] || break
      url="$next"
    done
    if [[ -z "$next" || "$host" == "${ZITADEL_URL#*://}" ]]; then
      echo "  FAIL: Zitadel did not send the browser to the provider"
      failed=1
      continue
    fi
    case "$name" in
      Google) expected="$(key GOOGLE_OAUTH_CLIENT_ID)" ;;
      Microsoft) expected="$(key MS_OAUTH_CLIENT_ID)" ;;
      *) expected="" ;;
    esac
    q="$(python3 -c 'import sys, urllib.parse as u; q = u.parse_qs(u.urlparse(sys.argv[1]).query); print(q.get("client_id", [""])[0]); print(q.get("redirect_uri", [""])[0])' "$next")"
    if [[ -n "$expected" ]]; then
      if [[ "$(sed -n 1p <<<"$q")" == "$expected" ]]; then
        echo "  client_id: the configured one"
      else
        echo "  FAIL: client_id is not the configured one"
        failed=1
      fi
    fi
    echo "  redirect_uri: $(sed -n 2p <<<"$q")"
    # One request to the provider (no sign-in): it shows its sign-in page only for a known client
    # with a registered redirect URI.
    case "$host" in
      accounts.google.com)
        read -r code loc < <(curl -sS -o /dev/null -w '%{http_code} %{redirect_url}\n' "$next")
        if [[ "$code" == 302 && "$loc" != *"/signin/oauth/error"* && "$loc" != *"authError"* ]]; then
          echo "  provider: $code to its sign-in page (${loc%%\?*})"
        else
          echo "  FAIL: provider answered $code ${loc%%\?*}"
          failed=1
        fi
        ;;
      login.microsoftonline.com)
        code="$(curl -sS -o "$page.ms" -w '%{http_code}' "$next")"
        # A known app shows its directory's name (sCompanyDisplayName); an unknown client gives
        # error code 50059 or 700016, a redirect URI that is not registered 50011.
        local ms_err
        ms_err="$(grep -o -E '"sErrorCode":"[0-9]+' "$page.ms" | head -n 1 | grep -o -E '[0-9]+$' || true)"
        if [[ "$code" == 200 ]] && grep -q '"sCompanyDisplayName"' "$page.ms" && ! grep -q 'AADSTS' "$page.ms" &&
          [[ ! "$ms_err" =~ ^(50011|50059|700016)$ ]]; then
          echo "  provider: $code, its sign-in page for the app (code ${ms_err:-none})"
        else
          echo "  FAIL: provider answered $code, code ${ms_err:-none} $(grep -o 'AADSTS[0-9]*' "$page.ms" | head -n 1)"
          failed=1
        fi
        rm -f "$page.ms"
        ;;
      *)
        code="$(curl -sS -o /dev/null -w '%{http_code}' "$next")"
        echo "  provider: $code"
        ;;
    esac
  done <<<"$providers"
  rm -f "$jar" "$page"
  ((failed == 0)) || die "check failed"
  log "check passed"
}

configure() {
  local configured=() skipped=() id google_id google_secret ms_id ms_secret summary

  ensure_external_allowed

  google_id="$(key GOOGLE_OAUTH_CLIENT_ID)"
  google_secret="$(key GOOGLE_OAUTH_CLIENT_SECRET)"
  if [[ -n "$google_id" && -n "$google_secret" ]]; then
    # The secret goes to jq in its environment, not on its command line.
    id="$(upsert google Google "$(CLIENT_ID="$google_id" CLIENT_SECRET="$google_secret" jq -n --argjson opts "$(options)" '{
        name: "Google", clientId: env.CLIENT_ID, clientSecret: env.CLIENT_SECRET,
        scopes: ["openid", "email", "profile"], providerOptions: $opts}')")"
    ensure_on_login_policy "$id" Google
    configured+=(Google)
  else
    skipped+=("Google (GOOGLE_OAUTH_CLIENT_ID/SECRET not set)")
  fi
  unset google_secret

  ms_id="$(key MS_OAUTH_CLIENT_ID)"
  ms_secret="$(key MS_OAUTH_CLIENT_SECRET)"
  if [[ -n "$ms_id" && -n "$ms_secret" ]]; then
    # emailVerified false: Microsoft does not prove that a work account owns the email address in
    # its profile, so Zitadel asks the person to verify it once (the code goes to Mailpit here).
    id="$(upsert azure Microsoft "$(CLIENT_ID="$ms_id" CLIENT_SECRET="$ms_secret" jq -n --argjson opts "$(options)" '{
        name: "Microsoft", clientId: env.CLIENT_ID, clientSecret: env.CLIENT_SECRET,
        tenant: {tenantType: "AZURE_AD_TENANT_TYPE_COMMON"}, emailVerified: false,
        scopes: ["openid", "email", "profile", "User.Read"], providerOptions: $opts}')")"
    ensure_on_login_policy "$id" Microsoft
    configured+=(Microsoft)
  else
    skipped+=("Microsoft (MS_OAUTH_CLIENT_ID/SECRET not set)")
  fi
  unset ms_secret

  if curl -fsS -o /dev/null --max-time 3 "$MOCK_ISSUER/.well-known/openid-configuration" 2>/dev/null; then
    # The mock accepts any client; these are fixed test values, not secrets. Its users are made
    # without the confirm page, so tests sign in in one step; the ID token carries the claims.
    id="$(upsert generic_oidc "Mock SSO" "$(jq -n --arg issuer "$MOCK_ISSUER" --argjson opts "$(options true)" '{
        name: "Mock SSO", issuer: $issuer, clientId: "ruum-zitadel", clientSecret: "mock-provider-test-value",
        scopes: ["openid", "email", "profile"], isIdTokenMapping: true, providerOptions: $opts}')")"
    ensure_on_login_policy "$id" "Mock SSO"
    configured+=("Mock SSO")
  else
    skipped+=("Mock SSO (start it with scripts/dev.sh up --sso-mock)")
  fi

  summary="sign-in providers: ${configured[*]+${configured[*]}}"
  ((${#configured[@]} > 0)) || summary="sign-in providers: none"
  if ((${#skipped[@]} > 0)); then
    summary="$summary; skipped: $(printf '%s; ' "${skipped[@]}")"
    summary="${summary%; }"
  fi
  log "$summary"
}

case "${1:-}" in
  "") configure ;;
  check) check ;;
  *) die "usage: $0 [check]" ;;
esac
