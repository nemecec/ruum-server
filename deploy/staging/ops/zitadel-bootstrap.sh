#!/usr/bin/env bash
# Zitadel on staging (plan 5.3, review 1 B-7, B-8), in the tools container with the admin
# kubeconfig. It talks to Zitadel through `kubectl port-forward` (in-cluster), with the machine
# user's PAT from the Secret auth/iam-admin-pat; the PAT never leaves this process.
#
#   zitadel-bootstrap.sh apply   login policy, OIDC app, Google, Microsoft, the SMTP provider;
#                                writes the Secret ruum/ruum-oidc: client-id, idp-google,
#                                idp-microsoft (the IdP IDs for the sign-in hint, RUUM_OIDC_IDP_*)
#   zitadel-bootstrap.sh smtp    only the SMTP provider (needs no Google or Microsoft key)
#   zitadel-bootstrap.sh check   report drift: password login, registration, auto linking,
#                                Microsoft email trust, IdP IDs in ruum/ruum-oidc, the active SMTP
#                                provider, human users, the PAT's expiry
#   zitadel-bootstrap.sh test-email ADDRESS
#                                Zitadel sends one test email to ADDRESS through the SMTP provider
#
# The Google and Microsoft keys come in the environment (steps.sh reads them from stdin) and go
# to curl on stdin; they are never printed or stored outside Zitadel's database. The SMTP login
# comes from the Secret auth/zitadel-smtp (k8s/apply.sh) and goes the same way.
set -euo pipefail
mode="${1:-check}"
NS=auth
HOST=auth.staging.ruum.space
PUBLIC=https://staging.ruum.space
PORT="${ZITADEL_LOCAL_PORT:-18080}"
# Outgoing email (backlog O-20): Scaleway Transactional Email over SMTP, the same values as
# DefaultInstance.SMTPConfiguration in k8s/values/zitadel.yaml (tests/run.sh compares them).
# DefaultInstance applies only when Zitadel creates an instance, so the bootstrap sets the
# provider of the instance that exists through the admin API (/admin/v1/email/...). Port 587:
# Hetzner blocks outgoing 25 and 465 on new accounts. tls true: Zitadel tries implicit TLS and,
# when the server answers in plain text, upgrades with STARTTLS and checks the host name.
SMTP_HOST=smtp.tem.scaleway.com:587
SMTP_FROM=no-reply@notify.staging.ruum.space
SMTP_FROM_NAME='Ruum (staging)'
SMTP_DESCRIPTION='Scaleway Transactional Email (staging)'
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

# upsert_idp KIND NAME BODY: finds the provider by name or adds it, and puts it in the login
# policy. Sets IDP_ID (a global, so that `set -e` still stops the script on an error inside).
IDP_ID=
upsert_idp() {
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
  [[ "$id" =~ ^[0-9]{1,32}$ ]] || die "the provider $name has no valid ID"
  IDP_ID="$id"
}

# The IdP ID of a key of ruum/ruum-oidc (empty when the Secret or the key is missing).
oidc_key() {
  kubectl -n ruum get secret ruum-oidc -o "jsonpath={.data.$1}" 2>/dev/null | base64 -d || true
}

# A key of the Secret auth/zitadel-smtp (empty when the Secret or the key is missing).
smtp_key() {
  kubectl -n "$NS" get secret zitadel-smtp -o "jsonpath={.data.$1}" 2>/dev/null | base64 -d || true
}

# The JSON of the email provider for SMTP_HOST (the first), or nothing.
smtp_provider() {
  echo '{}' | zapi POST /admin/v1/email/_search |
    jq -c --arg host "$SMTP_HOST" '[.result[]? | select(.smtp.host == $host)] | first // empty'
}

# Adds the provider, or updates it (the password too), and activates it. Activating it makes the
# provider that was active before (Mailpit) inactive. Idempotent.
smtp() {
  local user password provider id body
  user="$(smtp_key user)"
  password="$(smtp_key password)"
  [[ -n "$user" && -n "$password" ]] ||
    die "no user or password in $NS/zitadel-smtp (secrets-push.sh platform writes it from SCW_DEFAULT_PROJECT_ID and SCW_SMTP_SECRET_KEY)"
  body="$(SMTP_USER="$user" SMTP_PASSWORD="$password" jq -n \
    --arg host "$SMTP_HOST" --arg from "$SMTP_FROM" --arg name "$SMTP_FROM_NAME" --arg desc "$SMTP_DESCRIPTION" \
    '{senderAddress: $from, senderName: $name, tls: true, host: $host, user: env.SMTP_USER,
      replyToAddress: "", description: $desc, plain: {password: env.SMTP_PASSWORD}}')"
  provider="$(smtp_provider)"
  if [[ -z "$provider" ]]; then
    id="$(printf '%s' "$body" | zapi POST /admin/v1/email/smtp | jq -r '.id // empty')"
    log "added the SMTP provider $SMTP_HOST"
  else
    id="$(jq -r .id <<<"$provider")"
    [[ "$id" =~ ^[0-9]{1,32}$ ]] || die "the SMTP provider has no valid ID"
    printf '%s' "$body" | zapi PUT "/admin/v1/email/smtp/$id" >/dev/null
    log "updated the SMTP provider $SMTP_HOST"
  fi
  [[ "$id" =~ ^[0-9]{1,32}$ ]] || die "the SMTP provider has no valid ID"
  if [[ "$(smtp_provider | jq -r '.state // empty')" != EMAIL_PROVIDER_ACTIVE ]]; then
    echo '{}' | zapi POST "/admin/v1/email/$id/_activate" >/dev/null
    log "activated the SMTP provider $SMTP_HOST (the one before is now inactive)"
  fi
}

# Prints the state of the active email provider; returns 1 on drift.
smtp_check() {
  local active user drift=0
  active="$(echo '{}' | zapi POST /admin/v1/email/_search | jq -c '[.result[]? | select(.state == "EMAIL_PROVIDER_ACTIVE")] | first // empty')"
  if [[ -z "$active" ]]; then
    echo "DRIFT: no active email provider (Zitadel sends no email)"
    return 1
  fi
  user="$(smtp_key user)"
  jq -e --arg h "$SMTP_HOST" '.smtp.host == $h' <<<"$active" >/dev/null ||
    { echo "DRIFT: the active email provider is not $SMTP_HOST (it is $(jq -r '.smtp.host // .http.endpoint // "unknown"' <<<"$active"))"; drift=1; }
  jq -e '.smtp.tls == true' <<<"$active" >/dev/null || { echo "DRIFT: the active email provider has TLS off"; drift=1; }
  jq -e --arg f "$SMTP_FROM" '.smtp.senderAddress == $f' <<<"$active" >/dev/null ||
    { echo "DRIFT: the sender is not $SMTP_FROM"; drift=1; }
  jq -e --arg n "$SMTP_FROM_NAME" '.smtp.senderName == $n' <<<"$active" >/dev/null ||
    { echo "DRIFT: the sender name is not '$SMTP_FROM_NAME'"; drift=1; }
  jq -e '.smtp | has("plain")' <<<"$active" >/dev/null || { echo "DRIFT: the active email provider has no password login"; drift=1; }
  if [[ -z "$user" ]]; then echo "DRIFT: no user in $NS/zitadel-smtp"; drift=1
  elif ! jq -e --arg u "$user" '.smtp.user == $u' <<<"$active" >/dev/null; then
    echo "DRIFT: the SMTP user is not the one in $NS/zitadel-smtp"; drift=1
  fi
  [[ $drift == 0 ]] && echo "email: $SMTP_HOST is active, sender $SMTP_FROM_NAME <$SMTP_FROM>"
  return "$drift"
}

# Zitadel sends one test email through the provider (TestEmailProviderSMTPById). Only on request.
test_email() {
  local to="$1" id
  [[ "$to" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$ ]] || die "usage: zitadel-bootstrap.sh test-email ADDRESS"
  id="$(smtp_provider | jq -r '.id // empty')"
  [[ "$id" =~ ^[0-9]{1,32}$ ]] || die "no SMTP provider for $SMTP_HOST (run zitadel-bootstrap.sh smtp first)"
  jq -n --arg to "$to" '{receiverAddress: $to}' | zapi POST "/admin/v1/email/smtp/$id/_test" >/dev/null
  log "Zitadel sent a test email to $to through $SMTP_HOST"
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
  client="$(oidc_key client-id)"
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
  local google microsoft
  upsert_idp google Google "$(CLIENT_ID="$GOOGLE_OAUTH_CLIENT_ID" CLIENT_SECRET="$GOOGLE_OAUTH_CLIENT_SECRET" \
    jq -n --argjson opts "$OPTIONS" '{name: "Google", clientId: env.CLIENT_ID, clientSecret: env.CLIENT_SECRET,
      scopes: ["openid", "email", "profile"], providerOptions: $opts}')"
  google="$IDP_ID"
  # Microsoft `common`: the email of a work account is set by its tenant admin and nobody checks
  # it (nOAuth), so Zitadel must not trust it (emailVerified false, review 2 section 7).
  upsert_idp azure Microsoft "$(CLIENT_ID="$MS_OAUTH_CLIENT_ID" CLIENT_SECRET="$MS_OAUTH_CLIENT_SECRET" \
    jq -n --argjson opts "$OPTIONS" '{name: "Microsoft", clientId: env.CLIENT_ID, clientSecret: env.CLIENT_SECRET,
      tenant: {tenantType: "AZURE_AD_TENANT_TYPE_COMMON"}, emailVerified: false,
      scopes: ["openid", "email", "profile", "User.Read"], providerOptions: $opts}')"
  microsoft="$IDP_ID"
  # The IdP IDs let ruum-server send /v1/auth/login?idp=google|microsoft straight to the provider
  # (the Zitadel scope urn:zitadel:iam:org:idp:id:<id>). The chart requires both keys. On a re-run
  # the providers are found by name, so the IDs stay the same.
  printf 'client-id=%s\nidp-google=%s\nidp-microsoft=%s\n' "$client" "$google" "$microsoft" |
    kubectl create secret generic ruum-oidc -n ruum --from-env-file=/dev/stdin --dry-run=client -o yaml |
    kubectl apply -f - >/dev/null
  log "IdP IDs of Google and Microsoft in ruum/ruum-oidc (restart ruum-server to read new ones)"
  smtp
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
  local name key want have
  for name in Google Microsoft; do
    key="idp-$(tr '[:upper:]' '[:lower:]' <<<"$name")"
    want="$(idp_id "$name")"
    have="$(oidc_key "$key")"
    if [[ -z "$want" ]]; then echo "DRIFT: no provider $name"; drift=1
    elif [[ "$have" != "$want" ]]; then echo "DRIFT: ruum/ruum-oidc $key is not the ID of the provider $name"; drift=1
    else echo "provider $name: its ID is in ruum/ruum-oidc $key"; fi
  done
  smtp_check || drift=1
  humans="$(echo '{"queries":[{"typeQuery":{"type":"TYPE_HUMAN"}}]}' | zapi POST /management/v1/users/_search | jq '.result // [] | length')"
  echo "human users: $humans (before the person's test this must be 0; after it, only users that the providers created)"
  echo "machine PAT expiry: $(kubectl -n "$NS" get secret iam-admin-pat -o jsonpath='{.metadata.creationTimestamp}') + 90 days (record it in the digest)"
  [[ $drift == 0 ]] && echo "zitadel: no drift" || { echo "zitadel: DRIFT"; return 1; }
}

case "$mode" in
  apply) apply ;;
  smtp) smtp && check ;;
  check) check ;;
  test-email) test_email "${2:-}" ;;
  *) die "usage: zitadel-bootstrap.sh apply|smtp|check|test-email ADDRESS" ;;
esac
