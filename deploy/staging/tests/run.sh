#!/usr/bin/env bash
# Tests of the staging code that need no cloud account and create nothing (CI job "staging",
# and on the laptop or build server):
#
#   deploy/staging/tests/run.sh            all tests that the installed tools allow
#
# - check-plan.sh accepts a good plan and refuses each bad variant (zone resource, labels,
#   firewall, user_data, DNS names, build server, size, unknown type, variables in the JSON);
# - price.sh prices the plan and refuses above the budget;
# - the secret lists: nothing forbidden goes to the cluster, the two lists agree;
# - one Zitadel and SpiceDB pin for the laptop and staging (review 2 U-5);
# - check-plan.sh --repo; up.sh --apply refuses without RUUM_STAGING_APPLY=yes;
# - with helm: lint and render both charts, digests required, never production Let's Encrypt;
# - with docker: the rendered Caddyfile adapts (caddy adapt --validate);
# - with tofu: fmt and validate of both roots.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
ROOT="$(cd "$STG/../.." && pwd)"
FIX="$HERE/fixtures"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export PATH="$HOME/.local/bin:$PATH"
failed=0
ok() { printf 'ok   %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; failed=1; }

# expect CODE DESCRIPTION COMMAND...
expect() {
  local want="$1" what="$2" got
  shift 2
  "$@" >"$TMP/out" 2>&1
  got=$?
  if [[ "$got" == "$want" ]]; then ok "$what"; else bad "$what (exit $got, want $want)"; sed 's/^/     /' "$TMP/out" | head -20; fi
}

# mutate NAME JQ: a variant of the good plan.
mutate() { jq "$2" "$FIX/plan-good.json" >"$TMP/$1.json"; echo "$TMP/$1.json"; }

echo "== check-plan.sh"
expect 0 "a good plan passes" bash "$STG/check-plan.sh" "$FIX/plan-good.json"
expect 3 "refuses an hcloud_zone resource" bash "$STG/check-plan.sh" "$(mutate zone '.resource_changes += [{"address":"hcloud_zone.x","mode":"managed","type":"hcloud_zone","change":{"actions":["create"],"after":{"name":"ruum.space","labels":{"env":"staging","managed-by":"tofu"}}}}]')"
expect 3 "refuses a zone delete" bash "$STG/check-plan.sh" "$(mutate zonedel '.resource_changes += [{"address":"hcloud_zone.x","mode":"managed","type":"hcloud_zone","change":{"actions":["delete"],"before":{"name":"ruum.space"},"after":null}}]')"
expect 3 "refuses an unlabelled server" bash "$STG/check-plan.sh" "$(mutate nolabel '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change.after.labels) = {"role":"node"}')"
expect 3 "refuses an unlabelled DNS record" bash "$STG/check-plan.sh" "$(mutate rrlabel '(.resource_changes[] | select(.type == "hcloud_zone_rrset") | .change.after.labels) = null')"
expect 3 "refuses a server without firewall_ids" bash "$STG/check-plan.sh" "$(mutate nofw '(.resource_changes[] | select(.type == "hcloud_server") | .change.after_unknown) = {}')"
expect 3 "refuses a server with user_data" bash "$STG/check-plan.sh" "$(mutate ud '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-2\"]") | .change.after.user_data) = "machine: {}"')"
expect 3 "refuses unknown user_data" bash "$STG/check-plan.sh" "$(mutate udu '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-2\"]") | .change.after_unknown.user_data) = true')"
expect 3 "refuses a production DNS name" bash "$STG/check-plan.sh" "$(mutate apex '(.resource_changes[] | select(.address == "hcloud_zone_rrset.r[\"staging-A\"]") | .change.after.name) = "@"')"
expect 3 "refuses a notify.staging record" bash "$STG/check-plan.sh" "$(mutate notify '(.resource_changes[] | select(.address == "hcloud_zone_rrset.r[\"staging-A\"]") | .change.after) |= (.name = "notify.staging" | .type = "MX")')"
expect 3 "refuses anything named buildbox" bash "$STG/check-plan.sh" "$(mutate bb '(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after.name) = "ruum-buildbox"')"
expect 3 "refuses five servers" bash "$STG/check-plan.sh" "$(mutate five '.planned_values.root_module.resources += [range(2) | {"type":"hcloud_server","values":{"server_type":"cx23","location":"hel1"}}]')"
expect 3 "refuses another server type" bash "$STG/check-plan.sh" "$(mutate type '(.resource_changes[] | select(.type == "hcloud_server") | .change.after.server_type) = "cpx22"')"
expect 3 "refuses an unknown resource type" bash "$STG/check-plan.sh" "$(mutate unknown '.resource_changes += [{"address":"hcloud_volume_attachment.x","mode":"managed","type":"hcloud_volume_attachment","change":{"actions":["create"],"after":{}}}]')"
expect 3 "refuses a non-staging bucket" bash "$STG/check-plan.sh" "$(mutate bucket '(.resource_changes[] | select(.type == "minio_s3_bucket") | .change.after.bucket) = "ruum-prod-blobs"')"
expect 3 "refuses deleting a backup bucket" bash "$STG/check-plan.sh" "$(mutate bdel '.resource_changes += [{"address":"scaleway_object_bucket.backup[\"pg_backup\"]","mode":"managed","type":"scaleway_object_bucket","change":{"actions":["delete"],"before":{"name":"ruum-staging-pg-backup","tags":{"env":"staging","managed-by":"tofu"}},"after":null}}]')"
expect 1 "refuses a plan JSON that holds the variables" bash "$STG/check-plan.sh" "$(mutate vars '. + {"variables":{"state_passphrase":{"value":"x"}}}')"
expect 0 "check-plan --repo: no HCLOUD_TOKEN or Stripe key in cluster or CI files" bash "$STG/check-plan.sh" --repo

echo "== price.sh"
expect 0 "prices the plan under the budget" bash "$STG/price.sh" "$FIX/pricing.json" "$FIX/plan-good.json"
grep -q "Total.* 38\.16" "$TMP/out" && ok "total EUR 38.16 (3 x cx23, lb11, 3 IPv4, snapshot, object storage, Scaleway)" || { bad "total"; cat "$TMP/out"; }
expect 2 "refuses above EUR 50 (cpx22 fallback, review 2 C-2)" bash "$STG/price.sh" "$FIX/pricing.json" "$(mutate dear '(.planned_values.root_module.resources[] | select(.type == "hcloud_server") | .values.server_type) = "cpx22"')"

echo "== secrets"
list="$(bash "$STG/secrets-push.sh" --list)"
if grep -qE '^(HCLOUD_TOKEN|HETZNER_DNS_TOKEN|STRIPE_.*|SCW_ACCESS_KEY|SCW_SECRET_KEY|TOFU_STATE_PASSPHRASE)$' <<<"$list"; then
  bad "secrets-push.sh platform sends a forbidden name"
else
  ok "secrets-push.sh platform never sends HCLOUD_TOKEN, Stripe keys, the full Scaleway key or the passphrase"
fi
remote_names="$(sed -n 's/^CLUSTER_SECRET_NAMES="\(.*\)"$/\1/p' "$STG/remote.sh" | tr ' ' '\n' | sort)"
if [[ "$(sort <<<"$list")" == "$remote_names" ]]; then ok "secrets-push.sh and remote.sh list the same cluster names"; else bad "secrets-push.sh and remote.sh lists differ"; fi
if grep -rqE 'STRIPE_|HCLOUD_TOKEN' "$STG/k8s/apply.sh"; then bad "k8s/apply.sh names HCLOUD_TOKEN or a Stripe key"; else ok "k8s/apply.sh never puts HCLOUD_TOKEN or a Stripe key into a Secret"; fi

echo "== pins (review 2 U-5)"
# shellcheck source=../versions.env
. "$STG/versions.env"
grep -q "ghcr.io/zitadel/zitadel:$ZITADEL_VERSION" "$ROOT/deploy/dev/docker-compose.yml" && ok "Zitadel $ZITADEL_VERSION on the laptop and staging" || bad "deploy/dev uses another Zitadel than $ZITADEL_VERSION"
grep -q "authzed/spicedb:$SPICEDB_VERSION" "$ROOT/deploy/dev/docker-compose.yml" && ok "SpiceDB $SPICEDB_VERSION on the laptop and staging" || bad "deploy/dev uses another SpiceDB than $SPICEDB_VERSION"

echo "== up.sh guard"
expect 1 "up.sh --apply refuses without RUUM_STAGING_APPLY=yes" env -u RUUM_STAGING_APPLY bash "$STG/up.sh" --apply

if command -v helm >/dev/null; then
  echo "== charts (helm)"
  d="ghcr.io/nemecec/ruum-server@sha256:$(printf '%064d' 0)"
  e="ghcr.io/nemecec/ruum-edge@sha256:$(printf '%064d' 1)"
  expect 0 "helm lint charts/ruum" helm lint "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e"
  expect 0 "helm lint charts/platform" helm lint "$STG/charts/platform" --set generation=202611111200
  expect 0 "render charts/ruum by digest" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e"
  cp "$TMP/out" "$TMP/ruum.yaml"
  expect 1 "charts/ruum refuses an image by tag" helm template ruum "$STG/charts/ruum" --set "images.server=ghcr.io/nemecec/ruum-server:main" --set "images.edge=$e"
  expect 1 "charts/ruum refuses production Let's Encrypt" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set acme.issuer=letsencrypt
  expect 1 "charts/ruum refuses ZeroSSL without an email" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set acme.issuer=zerossl
  expect 0 "render charts/platform" helm template platform "$STG/charts/platform" --set generation=202611111200
  grep -q "serverName: pg-202611111200" "$TMP/out" && ok "the backup path carries the generation (review 2 R-2)" || bad "backup path generation"
  grep -q "retentionPolicy" "$TMP/out" && bad "barman retention is set (the in-cluster key must need no delete)" || ok "no barman retention (review 1 B-3)"
  expect 0 "render charts/platform in restore mode" helm template platform "$STG/charts/platform" --set generation=202612010000 --set restoreFrom=202611111200
  grep -q "serverName: pg-202611111200" "$TMP/out" && grep -q "recovery:" "$TMP/out" && ok "restore mode bootstraps from the old generation" || bad "restore mode"
  expect 1 "charts/platform refuses an empty generation" helm template platform "$STG/charts/platform"
  if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
    echo "== Caddyfile (caddy adapt --validate)"
    awk '/^  Caddyfile: \|/ {on=1; next} on && /^[^ ]/ {on=0} on && /^---/ {on=0} on {sub(/^    /, ""); print}' "$TMP/ruum.yaml" >"$TMP/Caddyfile"
    expect 0 "the rendered Caddyfile adapts" docker run --rm -v "$TMP/Caddyfile:/etc/caddy/Caddyfile:ro" "caddy:$CADDY_VERSION-alpine" caddy adapt --config /etc/caddy/Caddyfile --validate
  fi
else
  echo "== charts: skipped (no helm; setup.sh installs it)"
fi

if command -v tofu >/dev/null; then
  echo "== OpenTofu"
  for r in cluster persistent; do
    expect 0 "tofu fmt $r" tofu -chdir="$STG/tofu/$r" fmt -check -recursive
    # The same layout as the checkout (the patches are read from ../../talos).
    mkdir -p "$TMP/tree/tofu" "$TMP/tree/talos"
    cp -R "$STG/tofu/$r" "$TMP/tree/tofu/$r"
    cp -R "$STG/talos/." "$TMP/tree/talos/"
    expect 0 "tofu validate $r" bash -c "cd '$TMP/tree/tofu/$r' && tofu init -backend=false -input=false >/dev/null && tofu validate -no-color"
  done
else
  echo "== OpenTofu: skipped (no tofu; setup.sh installs it)"
fi

if [[ $failed == 0 ]]; then echo "staging tests: all passed"; else echo "staging tests: FAILED"; fi
exit "$failed"
