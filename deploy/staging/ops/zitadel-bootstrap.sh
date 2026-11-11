#!/usr/bin/env bash
# Zitadel on staging (plan 5.3, review 1 B-7, B-8), on the build server as ruum-ops with the admin
# kubeconfig. It talks to Zitadel through `kubectl port-forward` (in-cluster), with the machine
# user's PAT from the Secret auth/iam-admin-pat; the PAT never leaves this process.
#
#   zitadel-bootstrap.sh apply   login policy, OIDC app (client ID -> ruum/ruum-oidc), Google, Microsoft
#   zitadel-bootstrap.sh check   report drift: password login, registration, auto linking,
#                                Microsoft email trust, human users, the PAT's expiry
#
# The Google and Microsoft keys come in the environment (remote.sh reads them from stdin) and go
# to curl on stdin; they are never printed or stored outside Zitadel's database.
set -euo pipefail
mode="${1:-check}"
NS=auth
HOST=auth.staging.ruum.space
PUBLIC=https://staging.ruum.space
PORT="${ZITADEL_LOCAL_PORT:-18080}"
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

PAT="$(kubectl -n "$NS" get secret iam-admin-pat -o jsonpath='{.data.pat}' | base64 -d)"
[[ -n "$PAT" ]] || die "no PAT in $NS/iam-admin-pat (did the Zitadel setup job run?)"
kubectl -n "$NS" port-forward svc/zitadel "$PORT:8080" >/dev/null 2>&1 &
pf=$!
trap 'kill $pf 2>/dev/null || true' EXIT
for _ in $(seq 1 30); do
  curl -fsS -o /dev/null -H "Host: $HOST" "http://127.0.0.1:$PORT/debug/healthz" 2>/dev/null && break
  sleep 1
done

# zapi METHOD PATH: JSON body on stdin (GET: none). stdin carries the body, so the PAT goes to
# curl through a header file in a private temp folder (0600), never on the command line.
HDR="$(mktemp -d)"
trap 'kill $pf 2>/dev/null || true; rm -rf "$HDR"' EXIT
printf 'Authorization: Bearer %s\n' "$PAT" >"$HDR/auth"
chmod 600 "$HDR/auth"
zapi() {
  local method="$1" path="$2" data=(--data-binary @-)
  [[ "$method" == GET ]] && data=()
  curl -fsS -X "$method" "http://127.0.0.1:$PORT$path" \
    -H "Host: $HOST" -H "X-Forwarded-Proto: https" -H "@$HDR/auth" \
    -H "Content-Type: application/json" ${data[@]+"${data[@]}"}
}

# Review 1 B-7: no automatic linking for any provider; the signed-in person links.
OPTIONS='{"isLinkingAllowed":true,"isCreationAllowed":true,"isAutoCreation":false,"isAutoUpdate":true,"autoLinking":"AUTO_LINKING_OPTION_UNSPECIFIED"}'

idp_id() {
  jq -n --arg name "$1" '{queries: [{idpNameQuery: {name: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}' |
    zapi POST /admin/v1/idps/templates/_search | jq -r '.result[0]?.id // empty'
}

upsert_idp() { # KIND NAME BODY
  local kind="$1" name="$2" body="$3" id
  id="$(idp_id "$name")"
  if [[ -z "$id" ]]; then
    id="$(printf '%s' "$body" | zapi POST "/admin/v1/idps/$kind" | jq -r .id)"
    log "added the provider $name"
  else
    printf '%s' "$body" | zapi PUT "/admin/v1/idps/$kind/$id" >/dev/null 2>&1 || true
    log "updated the provider $name"
  fi
  if ! echo '{}' | zapi POST /admin/v1/policies/login/idps/_search | jq -e --arg id "$id" '.result // [] | any(.idpId == $id)' >/dev/null; then
    jq -n --arg id "$id" '{idpId: $id}' | zapi POST /admin/v1/policies/login/idps >/dev/null
  fi
}

apply() {
  # Review 1 B-8: no password form, no local registration, external providers only.
  zapi GET /admin/v1/policies/login | jq '.policy | {
      allowUsernamePassword: false, allowRegister: false, allowExternalIdp: true,
      forceMfa, passwordlessType: "PASSWORDLESS_TYPE_NOT_ALLOWED", hidePasswordReset: true,
      ignoreUnknownUsernames: true, defaultRedirectUri: "'"$PUBLIC"'/",
      passwordCheckLifetime, externalLoginCheckLifetime, mfaInitSkipLifetime,
      secondFactorCheckLifetime, multiFactorCheckLifetime, allowDomainDiscovery: false,
      disableLoginWithEmail: true, disableLoginWithPhone: true, forceMfaLocalOnly
    } | with_entries(select(.value != null))' | zapi PUT /admin/v1/policies/login >/dev/null 2>&1 || true
  log "login policy: external providers only"

  # The OIDC app of the Ruum server (public client, PKCE), as on the laptop.
  local project app client
  project="$(echo '{"queries":[{"nameQuery":{"name":"Ruum","method":"TEXT_QUERY_METHOD_EQUALS"}}]}' |
    zapi POST /management/v1/projects/_search | jq -r '.result[0].id // empty')"
  [[ -n "$project" ]] || project="$(echo '{"name":"Ruum","projectRoleAssertion":true}' | zapi POST /management/v1/projects | jq -r .id)"
  client="$(kubectl -n ruum get secret ruum-oidc -o jsonpath='{.data.client-id}' 2>/dev/null | base64 -d || true)"
  if [[ -z "$client" ]]; then
    app="$(jq -n --arg pub "$PUBLIC" '{
        name: "ruum-web",
        redirectUris: [($pub + "/v1/auth/callback")],
        postLogoutRedirectUris: [($pub + "/")],
        responseTypes: ["OIDC_RESPONSE_TYPE_CODE"],
        grantTypes: ["OIDC_GRANT_TYPE_AUTHORIZATION_CODE", "OIDC_GRANT_TYPE_REFRESH_TOKEN"],
        appType: "OIDC_APP_TYPE_USER_AGENT",
        authMethodType: "OIDC_AUTH_METHOD_TYPE_NONE",
        accessTokenType: "OIDC_TOKEN_TYPE_JWT",
        idTokenUserinfoAssertion: true,
        devMode: false
      }' | zapi POST "/management/v1/projects/$project/apps/oidc")"
    client="$(jq -r .clientId <<<"$app")"
    printf 'client-id=%s\n' "$client" |
      kubectl create secret generic ruum-oidc -n ruum --from-env-file=/dev/stdin --dry-run=client -o yaml |
      kubectl apply -f - >/dev/null
    log "OIDC app ruum-web; client ID in ruum/ruum-oidc"
  fi

  [[ -n "${GOOGLE_OAUTH_CLIENT_ID:-}" && -n "${GOOGLE_OAUTH_CLIENT_SECRET:-}" ]] || die "Google keys are missing"
  [[ -n "${MS_OAUTH_CLIENT_ID:-}" && -n "${MS_OAUTH_CLIENT_SECRET:-}" ]] || die "Microsoft keys are missing"
  upsert_idp google Google "$(CLIENT_ID="$GOOGLE_OAUTH_CLIENT_ID" CLIENT_SECRET="$GOOGLE_OAUTH_CLIENT_SECRET" \
    jq -n --argjson opts "$OPTIONS" '{name: "Google", clientId: env.CLIENT_ID, clientSecret: env.CLIENT_SECRET,
      scopes: ["openid", "email", "profile"], providerOptions: $opts}')"
  # Microsoft `common`: the email of a work account is set by its tenant admin and nobody checks
  # it (nOAuth), so Zitadel must not trust it (emailVerified false, review 2 section 7).
  upsert_idp azure Microsoft "$(CLIENT_ID="$MS_OAUTH_CLIENT_ID" CLIENT_SECRET="$MS_OAUTH_CLIENT_SECRET" \
    jq -n --argjson opts "$OPTIONS" '{name: "Microsoft", clientId: env.CLIENT_ID, clientSecret: env.CLIENT_SECRET,
      tenant: {tenantType: "AZURE_AD_TENANT_TYPE_COMMON"}, emailVerified: false,
      scopes: ["openid", "email", "profile", "User.Read"], providerOptions: $opts}')"
  check
}

check() {
  local drift=0 policy humans
  policy="$(zapi GET /admin/v1/policies/login | jq -c .policy)"
  jq -e '.allowUsernamePassword != true' <<<"$policy" >/dev/null || { echo "DRIFT: password login is allowed"; drift=1; }
  jq -e '.allowRegister != true' <<<"$policy" >/dev/null || { echo "DRIFT: local registration is allowed"; drift=1; }
  jq -e '.allowExternalIdp == true' <<<"$policy" >/dev/null || { echo "DRIFT: external providers are off"; drift=1; }
  echo '{}' | zapi POST /admin/v1/idps/templates/_search | jq -r '.result[]? | [.name, (.providerOptions.autoLinking // "AUTO_LINKING_OPTION_UNSPECIFIED"), (.azureAd.emailVerified // false | tostring)] | @tsv' |
    while IFS=$'\t' read -r name link verified; do
      if [[ "$link" != AUTO_LINKING_OPTION_UNSPECIFIED ]]; then echo "DRIFT: $name links automatically ($link)"; exit 1; fi
      if [[ "$name" == Microsoft && "$verified" == true ]]; then echo "DRIFT: Microsoft emails are trusted as verified"; exit 1; fi
      echo "provider $name: no automatic linking${verified:+, email verified=$verified}"
    done || drift=1
  humans="$(echo '{"queries":[{"typeQuery":{"type":"TYPE_HUMAN"}}]}' | zapi POST /management/v1/users/_search | jq '.result // [] | length')"
  echo "human users: $humans (before the person's test this must be 0; after it, only users that the providers created)"
  echo "machine PAT expiry: $(kubectl -n "$NS" get secret iam-admin-pat -o jsonpath='{.metadata.creationTimestamp}') + 90 days (record it in the digest)"
  [[ $drift == 0 ]] && echo "zitadel: no drift" || { echo "zitadel: DRIFT"; return 1; }
}

case "$mode" in
  apply) apply ;;
  check) check ;;
  *) die "usage: zitadel-bootstrap.sh apply|check" ;;
esac
