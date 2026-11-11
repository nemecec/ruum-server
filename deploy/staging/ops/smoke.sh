#!/usr/bin/env bash
# Smoke checks of staging without any sign-in (plan 6.2 step 3, review 1 S-8, stgsec section 5).
# Runs anywhere (laptop or build server); reads only public endpoints. Client IDs are compared
# with secrets.env locally and never printed.
#
#   ops/smoke.sh            all checks; exit 1 when one fails
set -uo pipefail
PUBLIC="${RUUM_STAGING_PUBLIC:-staging.ruum.space}"
AUTH="auth.$PUBLIC"
DAV="dav.$PUBLIC"
CONTENT="${RUUM_STAGING_CONTENT:-content.staging.ruumapps.net}"
SECRETS="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
failed=0
pass() { printf 'PASS %s\n' "$*"; }
fail() { printf 'FAIL %s\n' "$*"; failed=1; }
secret() { grep -E "^$1=" "$SECRETS" 2>/dev/null | tail -n 1 | cut -d= -f2- || true; }

# The CA of staging is not trusted by curl while it is the Let's Encrypt staging CA; then the
# checks use -k and check that the issuer is that CA, so a wrong certificate still fails.
issuer="$(echo | openssl s_client -connect "$PUBLIC:443" -servername "$PUBLIC" 2>/dev/null | openssl x509 -noout -issuer 2>/dev/null || true)"
K=()
case "$issuer" in
  *STAGING* | *"Fake LE"*) K=(-k); pass "TLS: Let's Encrypt staging CA ($issuer); checks use -k" ;;
  "") fail "TLS: no certificate on $PUBLIC" ;;
  *) pass "TLS: $issuer" ;;
esac
c() { curl -sS --max-time 20 "${K[@]}" "$@"; }

code="$(c -o /dev/null -w '%{http_code}' "https://$PUBLIC/healthz")"
[[ "$code" == 200 ]] && pass "healthz 200" || fail "healthz $code"

body="$(c "https://$PUBLIC/v1/me" -w '\n%{http_code}')"
[[ "${body##*$'\n'}" == 401 && "$body" == *'"code"'*'unauthenticated'* ]] && pass "/v1/me 401 with error JSON" || fail "/v1/me: ${body##*$'\n'}"

iss="$(c "https://$AUTH/.well-known/openid-configuration" | jq -r .issuer 2>/dev/null)"
[[ "$iss" == "https://$AUTH" ]] && pass "OIDC discovery issuer $iss" || fail "OIDC discovery issuer '$iss'"

code="$(c -o /dev/null -w '%{http_code}' -X PROPFIND -H 'Depth: 0' "https://$DAV/")"
[[ "$code" == 401 ]] && pass "WebDAV 401 without credentials" || fail "WebDAV $code"

for h in "$PUBLIC" "$AUTH" "$DAV"; do
  hsts="$(c -o /dev/null -D - "https://$h/healthz" | tr -d '\r' | awk -F': ' 'tolower($1) == "strict-transport-security" { print $2 }')"
  [[ "$hsts" == max-age=*includeSubDomains* && "$hsts" != *preload* ]] && pass "HSTS on $h ($hsts)" || fail "HSTS on $h: '$hsts'"
done
hsts="$(c -o /dev/null -D - "https://$CONTENT/" | tr -d '\r' | awk -F': ' 'tolower($1) == "strict-transport-security"')"
[[ -z "$hsts" ]] && pass "no HSTS on $CONTENT" || fail "HSTS on $CONTENT (ruumapps.net must not get it)"

# Review 1 B-8 / stgsec M-4: the console and admin APIs answer 403 from outside the build server.
if [[ "$(curl -s --max-time 5 https://api.ipify.org 2>/dev/null)" != 2.31.194.2 ]]; then
  for p in /ui/console /admin/v1/healthz /management/v1/healthz /system/v1/healthz; do
    code="$(c -o /dev/null -w '%{http_code}' "https://$AUTH$p")"
    [[ "$code" == 403 ]] && pass "$AUTH$p 403 from outside" || fail "$AUTH$p $code from outside (want 403)"
  done
fi

# On-demand TLS: a content host that is no tenant gets no certificate (review 1 B-6).
rnd="x$(openssl rand -hex 6).$CONTENT"
if echo | openssl s_client -connect "$CONTENT:443" -servername "$rnd" 2>/dev/null | grep -q 'BEGIN CERTIFICATE'; then
  fail "a certificate for the unknown host $rnd"
else
  pass "no certificate for an unknown content host"
fi

# The sign-in chain up to the provider's page, without a password (plan 8.2).
jar="$(mktemp)"
trap 'rm -f "$jar"' EXIT
for idp in google microsoft; do
  url="https://$PUBLIC/v1/auth/login?idp=$idp"
  host="" loc=""
  for _ in 1 2 3 4 5 6 7 8; do
    loc="$(c -o /dev/null -c "$jar" -b "$jar" -w '%{redirect_url}' "$url")"
    [[ -n "$loc" ]] || break
    host="$(sed -E 's#^https?://([^/?]+).*#\1#' <<<"$loc")"
    [[ "$host" == accounts.google.com || "$host" == login.microsoftonline.com ]] && break
    url="$loc"
  done
  case "$idp" in
    google) want_host=accounts.google.com want_id="$(secret GOOGLE_OAUTH_CLIENT_ID)" ;;
    microsoft) want_host=login.microsoftonline.com want_id="$(secret MS_OAUTH_CLIENT_ID)" ;;
  esac
  if [[ "$host" != "$want_host" ]]; then
    fail "$idp: the chain did not reach $want_host (stopped at ${url%%\?*})"
    continue
  fi
  got_id="$(python3 -c 'import sys, urllib.parse as u; print(u.parse_qs(u.urlparse(sys.argv[1]).query).get("client_id", [""])[0])' "$loc")"
  redirect="$(python3 -c 'import sys, urllib.parse as u; print(u.parse_qs(u.urlparse(sys.argv[1]).query).get("redirect_uri", [""])[0])' "$loc")"
  if [[ -n "$want_id" && "$got_id" != "$want_id" ]]; then
    fail "$idp: client_id is not the configured one"
  else
    pass "$idp: reaches $want_host, redirect_uri $redirect"
  fi
done

[[ $failed == 0 ]] && echo "smoke: all checks passed" || echo "smoke: FAILED"
exit "$failed"
