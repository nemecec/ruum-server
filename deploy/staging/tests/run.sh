#!/usr/bin/env bash
# Tests of the staging code that need no cloud account and create nothing (CI job "staging",
# and on the laptop or build server):
#
#   deploy/staging/tests/run.sh            all tests that the installed tools allow
#
# - check-plan.sh accepts a good plan and refuses each bad variant (zone resource, labels,
#   firewall, user_data, DNS names, build server, size, unknown type, variables in the JSON),
#   and each mutation of code review F-4 (firewall rules, deletes and replaces, attachments,
#   load balancer targets, object lock, child modules);
# - plan-scrub.jq removes the prior state and every sensitive value (F-5);
# - the sweep deletes only the staging names (F-6);
# - deploy-chart.sh and ssh-entry.sh refuse bad arguments (F-2, F-3);
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
# The build server's addresses, as remote.sh gives them to check-plan.sh.
export STG_ADMIN_CIDRS='["2.31.194.2/32","2a01:4f8:c015:3c76::/64"]'
export STG_BUILDBOX_ID=99999
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

echo "== check-plan.sh: code review F-4"
cp="$STG/check-plan.sh"
FW='(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after.rule)'
expect 3 "refuses a firewall rule from 0.0.0.0/0" bash "$cp" "$(mutate fw4 "${FW}[1].source_ips += [\"0.0.0.0/0\"]")"
expect 3 "refuses a firewall rule from ::/0" bash "$cp" "$(mutate fw6 "${FW}[2].source_ips = [\"::/0\"]")"
expect 3 "refuses a firewall rule from 0.0.0.0/1" bash "$cp" "$(mutate fw1 "${FW}[0].source_ips = [\"0.0.0.0/1\"]")"
RULE_ANY='{"direction":"in","protocol":"tcp","port":"any","source_ips":["2.31.194.2/32"],"destination_ips":[]}'
expect 3 "refuses a new rule with port any" bash "$cp" "$(mutate fwany "$FW += [$RULE_ANY]")"
expect 3 "refuses a udp rule" bash "$cp" "$(mutate fwudp "${FW}[1].protocol = \"udp\"")"
expect 3 "refuses another tcp port" bash "$cp" "$(mutate fw22 "${FW}[1].port = \"22\"")"
expect 3 "refuses an outbound rule" bash "$cp" "$(mutate fwout "${FW}[0].direction = \"out\"")"
expect 3 "refuses firewall sources unknown at plan time" bash "$cp" "$(mutate fwunk '(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after_unknown.rule[0].source_ips) = true')"
expect 3 "refuses firewall apply_to" bash "$cp" "$(mutate fwapply '(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after.apply_to) = [{"server":1}]')"
expect 3 "refuses firewall rules without STG_ADMIN_CIDRS" env -u STG_ADMIN_CIDRS bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS wider than one host or one /64" env STG_ADMIN_CIDRS='["0.0.0.0/0"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS with an IPv6 /48" env STG_ADMIN_CIDRS='["2a01:4f8::/48"]' bash "$cp" "$FIX/plan-good.json"
expect 3 "refuses deleting the firewall" bash "$cp" "$(mutate fwdel '(.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.actions = ["delete"] | .before = .after | .after = null)')"
expect 3 "refuses replacing the firewall" bash "$cp" "$(mutate fwrep '(.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.actions = ["delete","create"] | .before = .after)')"
expect 0 "accepts replacing the firewall with --allow-replace" bash "$cp" "$TMP/fwrep.json" --allow-replace
expect 3 "refuses replacing a server" bash "$cp" "$(mutate srvrep '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change) |= (.actions = ["create","delete"] | .before = (.after + {"id":"111"}))')"
expect 3 "refuses deleting a server" bash "$cp" "$(mutate srvdel '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-3\"]") | .change) |= (.actions = ["delete"] | .before = (.after + {"id":"113"}) | .after = null)')"
expect 0 "accepts deleting a server with --allow-delete (the destroy plan)" bash "$cp" "$TMP/srvdel.json" --allow-delete
ATT='{"address":"hcloud_firewall_attachment.x","mode":"managed","type":"hcloud_firewall_attachment","change":{"actions":["create"],"before":null,"after":ATTAFTER,"after_unknown":{"id":true,"firewall_id":true}}}'
expect 3 "refuses a firewall attachment to the build server" bash "$cp" "$(mutate attbb ".resource_changes += [${ATT/ATTAFTER/{\"server_ids\":[99999],\"label_selectors\":null}}]")"
expect 3 "refuses a firewall attachment to a server outside the plan" bash "$cp" "$(mutate attout ".resource_changes += [${ATT/ATTAFTER/{\"server_ids\":[4242],\"label_selectors\":null}}]")"
expect 3 "refuses a firewall attachment by another label selector" bash "$cp" "$(mutate attsel ".resource_changes += [${ATT/ATTAFTER/{\"server_ids\":[],\"label_selectors\":[\"env=prod\"]}}]")"
expect 0 "accepts a firewall attachment to a server of the plan" bash "$cp" "$(mutate attin "(.resource_changes[] | select(.address == \"hcloud_server.node[\\\"stg-1\\\"]\") | .change) |= (.actions = [\"update\"] | .before = (.after + {\"id\":4243}) | .after.id = 4243) | .resource_changes += [${ATT/ATTAFTER/{\"server_ids\":[4243],\"label_selectors\":null}}]")"
LBT='(.resource_changes[] | select(.type == "hcloud_load_balancer_target") | .change.after)'
expect 3 "refuses a load balancer target by a wider label selector" bash "$cp" "$(mutate lbsel "$LBT.label_selector = \"env=staging\"")"
expect 3 "refuses a load balancer target by another label selector" bash "$cp" "$(mutate lbsel2 "$LBT.label_selector = \"role=node\"")"
expect 3 "refuses a load balancer target by IP" bash "$cp" "$(mutate lbip "$LBT |= (.type = \"ip\" | .ip = \"203.0.113.7\" | .label_selector = null)")"
expect 3 "refuses a load balancer target on the build server" bash "$cp" "$(mutate lbbb "$LBT |= (.type = \"server\" | .server_id = 99999 | .label_selector = null)")"
LOCK='(.resource_changes[] | select(.type == "scaleway_object_bucket_lock_configuration") | .change.after.rule[0].default_retention[0])'
expect 3 "refuses a GOVERNANCE lock" bash "$cp" "$(mutate lockgov "$LOCK.mode = \"GOVERNANCE\"")"
expect 3 "refuses a 1-day lock" bash "$cp" "$(mutate lock1 "$LOCK.days = 1")"
expect 3 "refuses a longer lock than planned" bash "$cp" "$(mutate lock30 "$LOCK.days = 30")"
expect 3 "refuses a lock in years" bash "$cp" "$(mutate lockyr "$LOCK |= (.days = null | .years = 1)")"
expect 3 "refuses deleting the lock" bash "$cp" "$(mutate lockdel '(.resource_changes[] | select(.type == "scaleway_object_bucket_lock_configuration") | .change) |= (.actions = ["delete"] | .before = .after | .after = null)')"
expect 3 "refuses five servers when two are in a child module" bash "$cp" "$(mutate child '.planned_values.root_module.child_modules = [{"address":"module.extra","resources":[range(2) | {"type":"hcloud_server","values":{"server_type":"cx23","location":"hel1"}}]}]')"
expect 1 "refuses a plan JSON that holds the prior state" bash "$cp" "$(mutate prior '. + {"prior_state":{"values":{}}}')"
bash "$STG/price.sh" "$FIX/pricing.json" "$TMP/child.json" >"$TMP/out" 2>&1
grep -qE "server cx23 \(hel1\) +5 " "$TMP/out" && ok "price.sh counts the servers of child modules" || { bad "price.sh child modules"; cat "$TMP/out"; }

echo "== plan-scrub.jq (code review F-5)"
jq '. + {"variables":{"state_passphrase":{"value":"SECRET-MARKER-1"}},
         "prior_state":{"values":{"root_module":{"resources":[{"type":"talos_machine_secrets","values":{"machine_secrets":"SECRET-MARKER-2"}}]}}},
         "configuration":{"provider_config":{"x":{"expressions":{"token":{"constant_value":"SECRET-MARKER-3"}}}}},
         "output_changes":{"talosconfig":{"actions":["no-op"],"before":"SECRET-MARKER-4","after":"SECRET-MARKER-4","before_sensitive":true,"after_sensitive":true}}}
    | .resource_changes += [
        {"address":"talos_machine_secrets.this","mode":"managed","type":"talos_machine_secrets","change":{"actions":["no-op"],"before":{"machine_secrets":{"certs":{"etcd":{"key":"SECRET-MARKER-5"}}}},"after":{"machine_secrets":{"certs":{"etcd":{"key":"SECRET-MARKER-5"}}}}}},
        {"address":"data.talos_client_configuration.this","mode":"data","type":"talos_client_configuration","change":{"actions":["read"],"before":null,"after":{"talos_config":"SECRET-MARKER-6"}}},
        {"address":"terraform_data.x","mode":"managed","type":"terraform_data","change":{"actions":["update"],"before":{"input":{"a":"SECRET-MARKER-7","b":"keep"}},"after":{"input":{"a":"SECRET-MARKER-7","b":"keep"}},"before_sensitive":{"input":{"a":true}},"after_sensitive":{"input":{"a":true}}}}]
    | .planned_values.root_module.resources += [{"type":"terraform_data","values":{"input":{"a":"SECRET-MARKER-8"}},"sensitive_values":{"input":{"a":true}}}]
    | .planned_values.outputs = {"talosconfig":{"sensitive":true,"value":"SECRET-MARKER-9"}}' \
  "$FIX/plan-good.json" >"$TMP/secret-plan.json"
jq -f "$STG/plan-scrub.jq" "$TMP/secret-plan.json" >"$TMP/scrubbed.json"
if grep -q SECRET-MARKER "$TMP/scrubbed.json"; then bad "plan-scrub.jq leaves a secret: $(grep -o 'SECRET-MARKER-[0-9]' "$TMP/scrubbed.json" | sort -u | tr '\n' ' ')"; else ok "plan-scrub.jq removes the variables, prior state, configuration, sensitive, Talos, data source and output values"; fi
jq -e '[.resource_changes[] | select(.type == "terraform_data") | .change.after.input.b] | index("keep")' "$TMP/scrubbed.json" >/dev/null && ok "plan-scrub.jq keeps the values that are not sensitive" || bad "plan-scrub.jq drops plain values"
expect 0 "check-plan.sh accepts the scrubbed plan" bash "$cp" "$TMP/scrubbed.json"
expect 1 "check-plan.sh refuses the unscrubbed plan" bash "$cp" "$TMP/secret-plan.json"
if grep -nE 'tofu show -json "' "$STG/remote.sh" "$STG/ops/destroy-remote.sh" | grep -v 'jq -f "$HERE/plan-scrub.jq"' | grep -q .; then
  bad "a 'tofu show -json' in remote.sh or destroy-remote.sh does not pipe through plan-scrub.jq"
else
  ok "every 'tofu show -json' pipes through plan-scrub.jq before it is written"
fi

echo "== sweep (code review F-6)"
printf '%s\n' "servers stg-1" "servers stg-5" "servers ruum-buildbox" "servers web" "primary_ips stg-2-v4" "primary_ips other-v4" \
  "load_balancers ruum-staging" "load_balancers ruum-prod" "networks ruum-staging" "firewalls ruum-staging-nodes" "firewalls ruum-buildbox" \
  "placement_groups ruum-staging-nodes" "rrset ruum.space staging A" "rrset ruumapps.net *.content.staging AAAA" "rrset ruum.space @ A" \
  "rrset ruum.space notify.staging MX" "rrset ruum.space www A" "rrset ruumapps.net staging A" "snapshot talos" >"$TMP/sweep-in.txt"
HERE="$STG" bash "$STG/ops/destroy-remote.sh" sweep-filter <"$TMP/sweep-in.txt" >"$TMP/sweep-out.txt" 2>/dev/null
want="$(printf '%s\n' "servers stg-1" "primary_ips stg-2-v4" "load_balancers ruum-staging" "networks ruum-staging" "firewalls ruum-staging-nodes" \
  "placement_groups ruum-staging-nodes" "rrset ruum.space staging A" "rrset ruumapps.net *.content.staging AAAA")"
if [[ "$(cat "$TMP/sweep-out.txt")" == "$want" ]]; then ok "sweep keeps only the staging names (rrsets.json), never the build server, the apex or notify"; else bad "sweep filter"; diff <(echo "$want") "$TMP/sweep-out.txt"; fi
jq -e --slurpfile r "$STG/rrsets.json" 'length == ($r[0] | length)' "$STG/rrsets.json" >/dev/null &&
  ! grep -q 'STAGING_RRSETS=' "$STG/ops/destroy-remote.sh" && ok "one rrset list for check-plan and sweep (rrsets.json)" || bad "rrset lists"

echo "== deploy-chart.sh arguments (code review F-3)"
dc="$STG/ops/deploy-chart.sh"
C=0123456789abcdef0123456789abcdef01234567
SV="ghcr.io/nemecec/ruum-server@sha256:$(printf '%064d' 0)"
ED="ghcr.io/nemecec/ruum-edge@sha256:$(printf '%064d' 1)"
expect 0 "accepts valid arguments" bash "$dc" "$C" "$SV" "$ED" --acme zerossl --acme-email ops+stg@example.com --validate-only
expect 1 "refuses a short commit" bash "$dc" 0123456 "$SV" "$ED" --validate-only
expect 1 "refuses a commit with a --set injection" bash "$dc" "x,images.server=evil.example/a@sha256:$(printf '%064d' 0)" "$SV" "$ED" --validate-only
expect 1 "refuses a server image from another registry" bash "$dc" "$C" "evil.example/nemecec/ruum-server@sha256:$(printf '%064d' 0)" "$ED" --validate-only
expect 1 "refuses the edge image in the server slot" bash "$dc" "$C" "$ED" "$ED" --validate-only
expect 1 "refuses an issuer outside the list" bash "$dc" "$C" "$SV" "$ED" --acme letsencrypt --validate-only
expect 1 "refuses an email with a comma" bash "$dc" "$C" "$SV" "$ED" --acme zerossl --acme-email "a@b.io,adminSources={0.0.0.0/0}" --validate-only
expect 1 "refuses zerossl without an email" bash "$dc" "$C" "$SV" "$ED" --acme zerossl --validate-only
grep -vE '^[[:space:]]*#' "$dc" | grep -qE -- '--set[ =]' && bad "deploy-chart.sh passes values with --set" || ok "deploy-chart.sh gives helm a values file, never --set"
grep -q 'cosign_verify_provenance' "$dc" && ok "deploy-chart.sh verifies the provenance on the build server" || bad "deploy-chart.sh skips the provenance check"

echo "== ssh-entry.sh (code review F-2)"
se="$STG/ops/ssh-entry.sh"
expect 0 "accepts a remote.sh step with plain words" env SSH_ORIGINAL_COMMAND="deploy $C $SV $ED --acme zerossl --acme-email ops+stg@example.com" bash "$se" --parse
expect 2 "refuses shell syntax" env SSH_ORIGINAL_COMMAND='status; id' bash "$se" --parse
expect 2 "refuses command substitution" env SSH_ORIGINAL_COMMAND='status $(id)' bash "$se" --parse
expect 2 "refuses an unknown step" env SSH_ORIGINAL_COMMAND='bash -i' bash "$se" --parse
expect 2 "refuses an empty command" env SSH_ORIGINAL_COMMAND= bash "$se" --parse
grep -q 'sudoers' "$STG/ops/install-ops-user.sh" && grep -qE '^ruum ALL' "$STG/ops/install-ops-user.sh" && bad "install-ops-user.sh writes a sudoers rule" || ok "install-ops-user.sh writes no sudoers rule"
grep -q 'merge-base --is-ancestor "$commit" origin/main' "$STG/ops/install-ops-user.sh" && ok "install-ops-user.sh accepts only commits of main" || bad "install-ops-user.sh accepts branch commits"

echo "== shell syntax"
for f in "$STG"/*.sh "$STG"/ops/*.sh "$STG"/k8s/*.sh; do
  bash -n "$f" 2>"$TMP/out" || { bad "bash -n $f"; cat "$TMP/out"; }
done
ok "bash -n of every script"

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
  expect 1 "charts/ruum refuses another registry (code review F-3)" helm template ruum "$STG/charts/ruum" --set "images.server=evil.example/a@sha256:$(printf '%064d' 0)" --set "images.edge=$e"
  expect 1 "charts/ruum refuses a commit that is not 40 hex" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set-string commit=main
  expect 0 "charts/ruum renders ZeroSSL with a plain email and a full commit" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set acme.issuer=zerossl --set-string acme.email=ops+stg@example.com --set-string commit=0123456789abcdef0123456789abcdef01234567
  expect 1 "charts/ruum refuses an odd acme email" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set acme.issuer=zerossl --set-string "acme.email=a b@c.io"
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
