#!/usr/bin/env bash
# Smoke checks of the app runtime on staging (card I-6, milestone M2), without any sign-in. Runs on
# the Mac (curl, openssl, dig or host); reads only public endpoints. status.sh --checks runs it.
#
#   ops/smoke-apps.sh       exit 1 when a check fails; a BLOCKED line is not a failure
#
# 1. DNS: any name under the apps domain resolves to the load balancer (the addresses of the
#    platform host).
# 2. On-demand TLS: a host of an app that does not exist gets no certificate (ruum-server's ask
#    endpoint answers 404 for it).
# 3. A static app (RUUM_STAGING_SMOKE_STATIC, or STG_SMOKE_STATIC_HOST in secrets.env: the host of
#    a live static app, for example site.<tenant slug>.apps.staging.ruumapps.net, deployed once by
#    the person over MCP or the UI): it has a certificate, no HSTS and no Server header; a request
#    without a session goes to the gateway's sign-in (/.ruum/v1/auth/start) and from there to the
#    platform's /v1/auth/login; a public app answers 200 at once. Not set: SKIP.
# 4. A node22 app (RUUM_STAGING_SMOKE_NODE22, or STG_SMOKE_NODE22_HOST): the same checks. Only with
#    RUUM_STAGING_SMOKE_NODE22_READY=1. The Kubernetes runtime driver and the sandbox directory
#    exist (backlog I6-1), but no node22 app can be live on staging yet: the deploy pipeline
#    refuses runtime node22 (runtime_not_ready), staging has no builder and no registry, and
#    nothing makes the spec of a deployment's instance. Until those land the check prints
#    BLOCKED (deploy/staging/README.md "The runtime driver").
# Content behind the sign-in (the page itself) is the person's browser check (README "The app
# runtime"): agents cannot sign in.
set -uo pipefail
PUBLIC="${RUUM_STAGING_PUBLIC:-staging.ruum.space}"
APPS="${RUUM_STAGING_APPS:-apps.staging.ruumapps.net}"
SECRETS="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
failed=0
pass() { printf 'PASS %s\n' "$*"; }
fail() { printf 'FAIL %s\n' "$*"; failed=1; }
secret() { grep -E "^$1=" "$SECRETS" 2>/dev/null | tail -n 1 | cut -d= -f2- || true; }

# The A and AAAA records of a name, sorted, one line.
addrs() {
  local n="$1"
  if command -v dig >/dev/null; then
    { dig +short A "$n"; dig +short AAAA "$n"; } | grep -E '^[0-9a-fA-F.:]+$' | sort | tr '\n' ' '
  else
    host "$n" 2>/dev/null | awk '/has (IPv6 )?address/ {print $NF}' | sort | tr '\n' ' '
  fi
}

# 1. DNS.
want="$(addrs "$PUBLIC")"
rnd="x$(openssl rand -hex 4).y$(openssl rand -hex 4).$APPS"
got="$(addrs "$rnd")"
if [[ -n "$want" && "$got" == "$want" ]]; then
  pass "DNS: $rnd resolves to the load balancer ($got)"
else
  fail "DNS: $rnd resolves to '${got:-nothing}', not the load balancer '$want' (tofu/cluster dns.tf *.apps.staging)"
fi

# The CA: the Let's Encrypt staging CA is not trusted by curl; then -k, and the issuer is checked.
issuer="$(echo | openssl s_client -connect "$PUBLIC:443" -servername "$PUBLIC" 2>/dev/null | openssl x509 -noout -issuer 2>/dev/null || true)"
K=()
case "$issuer" in *STAGING* | *"Fake LE"*) K=(-k) ;; esac
c() { curl -sS --max-time 30 ${K[@]+"${K[@]}"} "$@"; }

# 2. No certificate for an app host that no app has (ask endpoint, fails closed).
if echo | openssl s_client -connect "$PUBLIC:443" -servername "$rnd" 2>/dev/null | grep -q 'BEGIN CERTIFICATE'; then
  fail "a certificate for the unknown app host $rnd"
else
  pass "no certificate for an unknown app host"
fi

# app_checks KIND HOST: the checks of a live app host without a session.
app_checks() {
  local kind="$1" host="$2" cert head code loc hsts
  cert="$(echo | openssl s_client -connect "$PUBLIC:443" -servername "$host" 2>/dev/null | openssl x509 -noout -text 2>/dev/null || true)"
  if [[ "$cert" == *"DNS:$host"* ]]; then pass "$kind: $host has its own certificate"; else fail "$kind: no certificate for $host (is the app live? ask endpoint)"; return; fi
  head="$(c -o /dev/null -D - "https://$host/" 2>/dev/null | tr -d '\r')"
  code="$(awk 'NR == 1 {print $2}' <<<"$head")"
  loc="$(awk -F': ' 'tolower($1) == "location" {print $2}' <<<"$head")"
  hsts="$(awk -F': ' 'tolower($1) == "strict-transport-security"' <<<"$head")"
  [[ -z "$hsts" ]] && pass "$kind: no HSTS (ruumapps.net)" || fail "$kind: HSTS on $host"
  grep -qi '^server:' <<<"$head" && fail "$kind: a Server header on $host" || pass "$kind: no Server header"
  case "$code" in
    200) pass "$kind: $host answers 200 (a public app)" ;;
    302)
      if [[ "$loc" == /.ruum/v1/auth/start* ]]; then
        pass "$kind: without a session the gateway starts its sign-in ($loc)"
        loc="$(c -o /dev/null -w '%{redirect_url}' "https://$host$loc")"
        [[ "$loc" == "https://$PUBLIC/v1/auth/login"* ]] && pass "$kind: the sign-in goes to the platform (https://$PUBLIC/v1/auth/login)" ||
          fail "$kind: the gateway's sign-in goes to '${loc%%\?*}', not https://$PUBLIC/v1/auth/login"
      else
        fail "$kind: $host redirects to '$loc', not the gateway's sign-in"
      fi
      ;;
    *) fail "$kind: $host answers ${code:-nothing} (want 200 for a public app, 302 to the sign-in otherwise)" ;;
  esac
}

# 3. A static app.
static="${RUUM_STAGING_SMOKE_STATIC:-$(secret STG_SMOKE_STATIC_HOST)}"
if [[ -n "$static" ]]; then
  [[ "$static" == *".$APPS" ]] || { fail "the static smoke host $static is not under $APPS"; static=""; }
  [[ -z "$static" ]] || app_checks static "$static"
else
  echo "SKIP static app: no STG_SMOKE_STATIC_HOST (the person deploys one static app once; README \"The app runtime\")"
fi

# 4. A node22 app.
node="${RUUM_STAGING_SMOKE_NODE22:-$(secret STG_SMOKE_NODE22_HOST)}"
if [[ "${RUUM_STAGING_SMOKE_NODE22_READY:-}" != 1 ]]; then
  echo "BLOCKED node22 app: the Kubernetes runtime driver (I6-1) is off on staging until S-87, node22 deploys are refused (runtime_not_ready) and staging has no builder or registry yet; the gateway answers 502 after the sign-in"
elif [[ -n "$node" && "$node" == *".$APPS" ]]; then
  app_checks node22 "$node"
else
  fail "node22 app: RUUM_STAGING_SMOKE_NODE22_READY=1 needs STG_SMOKE_NODE22_HOST under $APPS"
fi

[[ $failed == 0 ]] && echo "smoke-apps: all checks passed" || echo "smoke-apps: FAILED"
exit "$failed"
