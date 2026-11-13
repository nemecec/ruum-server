#!/usr/bin/env bash
# Tests of the staging code that need no cloud account and create nothing (CI job "staging",
# and on the Mac):
#
#   deploy/staging/tests/run.sh            all tests that the installed tools allow
#
# - check-plan.sh accepts a good plan and refuses each bad variant (zone resource, labels,
#   firewall, user_data, DNS names, build server, size, unknown type, variables in the JSON),
#   each mutation of code review F-4 (firewall rules, deletes and replaces, attachments,
#   load balancer targets, object lock, child modules) and of code review 2 G-1 (a foreign
#   firewall ID, apply_to without rules, unknown rules, primary IP assignee), only the build
#   server's /32 as the admin source, apply_to only unchanged on an existing firewall that is
#   updated in place, and (Kimi review) no firewall attachment and no in-place firewall_ids change;
# - plan-scrub.jq removes the prior state and every sensitive value (F-5);
# - the sweep deletes only the staging names (F-6);
# - deploy-chart.sh refuses bad arguments (F-3); the admin address must be a public IPv4;
# - the Mac-only layout: no Mac IP detection, ssh only for the admin tunnel, no build-server
#   address or agent credential path, the checkout read-only in the tools container, rw steps
#   refuse without RUUM_STAGING_APPLY=yes;
# - the admin tunnel: targets, ssh forwards and options, talosconfig endpoints, the kubeconfig
#   rewrite (with kubectl), the restricted key line and the sshd config of the build server;
#   (Kimi review) the control socket fingerprint, the forward checks, the rollback of
#   buildbox-tunnel-user.sh and the relay checks of tunnel-relay.sh;
# - price.sh prices the plan and refuses above the budget;
# - the secret lists: nothing forbidden goes to the cluster, the two lists agree;
# - one Zitadel and SpiceDB pin for the laptop and staging (review 2 U-5);
# - Zitadel's SMTP (backlog O-20): Scaleway on 587 with TLS, the login from auth/zitadel-smtp
#   (no password in the values), the verified sender domain, the same values in the bootstrap,
#   the SMTP key comes from the person (not secrets-init.sh) and up.sh stops without it; with
#   helm and the chart repository: the rendered Zitadel setup job and deployment read it;
# - check-plan.sh --repo; up.sh --apply refuses without RUUM_STAGING_APPLY=yes;
# - with helm: lint and render both charts, digests required, never production Let's Encrypt;
#   the ruum-server env of the chart and of ruum-server-env matches what the server reads
#   (tests/server-env.sh against crates/ruum-server/env-names.txt);
# - updates without downtime (backlog O-25, O-26): ruum-server two replicas, rolling update with
#   maxUnavailable 0, RUUM_MIGRATE=false, preStop pause, drain, disruption budget; the migration
#   Job (only when deploy-chart.sh asks for it; ruum-server migrate; not behind the Service; only
#   DATABASE_URL); the edge DaemonSet with surge, certificates in Kubernetes Secrets, the edge's
#   namespaced rights; the deployer may run Jobs in ruum; deploy-chart.sh migrates first;
# - the Kimi review of O-25: the migration Job reads the owner's DSN from ruum-migrate-db and the
#   serving pods never do; ruum-server-env holds ruum_app's DSN, written after the grants; the
#   grants (with docker: against postgres:17) give ruum_app data rights only and no write on
#   _sqlx_migrations; --rollback goes only to the previous release (ruum-release `previous`);
#   the deploy lock (a fake kubectl) and its place around the read, the migration and helm;
# - with docker: the rendered Caddyfile adapts (caddy adapt --validate; without the storage
#   block, which only the edge image has);
# - with tofu: fmt and validate of both roots; with tofu and talosctl: the machine config of
#   every node, rendered by the Talos provider, passes talosctl validate (tests/talos-validate.sh).
# helm and tofu come from PATH (CI: setup.sh) or, when they are not there and Docker is, from the
# tools image (tools-image.sh, the same pins).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
ROOT="$(cd "$STG/../.." && pwd)"
FIX="$HERE/fixtures"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export PATH="$HOME/.local/bin:$PATH"
failed=0
# The build server's public IPv4 as steps.sh gives it to check-plan.sh (a documentation address here).
export STG_ADMIN_CIDRS='["203.0.113.10/32"]'
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
expect 1 "refuses a plan JSON that holds the variables" bash "$STG/check-plan.sh" "$(mutate vars '. + {"variables":{"scrub_probe_0":{"value":"SCRUB-PROBE-0"}}}')"
expect 0 "check-plan --repo: no HCLOUD_TOKEN, Stripe key or full Scaleway key in cluster or CI files" bash "$STG/check-plan.sh" --repo

echo "== check-plan.sh: code review F-4"
cp="$STG/check-plan.sh"
FW='(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after.rule)'
expect 3 "refuses a firewall rule from 0.0.0.0/0" bash "$cp" "$(mutate fw4 "${FW}[1].source_ips += [\"0.0.0.0/0\"]")"
expect 3 "refuses a firewall rule from ::/0" bash "$cp" "$(mutate fw6 "${FW}[2].source_ips = [\"::/0\"]")"
expect 3 "refuses a firewall rule from 0.0.0.0/1" bash "$cp" "$(mutate fw1 "${FW}[0].source_ips = [\"0.0.0.0/1\"]")"
RULE_ANY='{"direction":"in","protocol":"tcp","port":"any","source_ips":["203.0.113.10/32"],"destination_ips":[]}'
expect 3 "refuses a new rule with port any" bash "$cp" "$(mutate fwany "$FW += [$RULE_ANY]")"
expect 3 "refuses a udp rule" bash "$cp" "$(mutate fwudp "${FW}[1].protocol = \"udp\"")"
expect 3 "refuses another tcp port" bash "$cp" "$(mutate fw22 "${FW}[1].port = \"22\"")"
expect 3 "refuses an outbound rule" bash "$cp" "$(mutate fwout "${FW}[0].direction = \"out\"")"
expect 3 "refuses firewall sources unknown at plan time" bash "$cp" "$(mutate fwunk '(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after_unknown.rule[0].source_ips) = true')"
expect 3 "refuses firewall apply_to" bash "$cp" "$(mutate fwapply '(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after.apply_to) = [{"server":1}]')"
expect 3 "refuses firewall rules without STG_ADMIN_CIDRS" env -u STG_ADMIN_CIDRS bash "$cp" "$FIX/plan-good.json"
expect 3 "refuses firewall rules from another address than the build server" env STG_ADMIN_CIDRS='["203.0.113.11/32"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS of 0.0.0.0/0" env STG_ADMIN_CIDRS='["0.0.0.0/0"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS wider than one host (/31)" env STG_ADMIN_CIDRS='["203.0.113.10/31"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS without a prefix" env STG_ADMIN_CIDRS='["203.0.113.10"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS with an IPv6 /64" env STG_ADMIN_CIDRS='["2a01:4f8:c015:3c76::/64"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS with an IPv6 /128" env STG_ADMIN_CIDRS='["2a01:4f8::1/128"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS with two addresses" env STG_ADMIN_CIDRS='["203.0.113.10/32","203.0.113.11/32"]' bash "$cp" "$FIX/plan-good.json"
expect 1 "refuses STG_ADMIN_CIDRS with an octet above 255" env STG_ADMIN_CIDRS='["203.0.113.300/32"]' bash "$cp" "$FIX/plan-good.json"
expect 3 "refuses a rule with the build server and a second /32" bash "$cp" "$(mutate fwtwo "${FW}[1].source_ips += [\"203.0.113.11/32\"]")"
expect 3 "refuses a rule with the build server's /24" bash "$cp" "$(mutate fw24 "${FW}[1].source_ips = [\"203.0.113.0/24\"]")"
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
# Kimi review F1: no standalone firewall attachment at all, in any action, to any firewall.
FW77='(.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.actions = ["update"] | .before = (.after + {"id":"77"}) | .after.id = "77")
  | (.resource_changes[] | select(.type == "hcloud_server") | .change) |= (.after.firewall_ids = [77] | .after_unknown.firewall_ids = [false])'
expect 3 "refuses a firewall attachment to a server of the plan" bash "$cp" "$(mutate attin "$FW77 | (.resource_changes[] | select(.address == \"hcloud_server.node[\\\"stg-1\\\"]\") | .change) |= (.actions = [\"no-op\"] | .before = (.after + {\"id\":4243}) | .after.id = 4243) | .resource_changes += [${ATT/ATTAFTER/{\"server_ids\":[4243],\"label_selectors\":null}}]")"
ATTX='{"address":"hcloud_firewall_attachment.x","mode":"managed","type":"hcloud_firewall_attachment","change":{"actions":ATTACTS,"before":ATTBEFORE,"after":ATTAFTER,"after_unknown":{}}}'
ATTB='{"id":5,"firewall_id":999999,"server_ids":[4243],"label_selectors":null}'
ATTA='{"id":5,"firewall_id":999999,"server_ids":[4243],"label_selectors":null}'
a="${ATTX/ATTACTS/[\"create\"]}"; a="${a/ATTBEFORE/null}"; a="${a/ATTAFTER/$ATTA}"
expect 3 "refuses a firewall attachment of a foreign firewall ID (create)" bash "$cp" "$(mutate attfw "$FW77 | .resource_changes += [$a]")"
a="${ATTX/ATTACTS/[\"update\"]}"; a="${a/ATTBEFORE/${ATTB/4243/4241}}"; a="${a/ATTAFTER/$ATTA}"
expect 3 "refuses a firewall attachment update" bash "$cp" "$(mutate attupd "$FW77 | .resource_changes += [$a]")"
a="${ATTX/ATTACTS/[\"delete\"]}"; a="${a/ATTBEFORE/$ATTB}"; a="${a/ATTAFTER/null}"
expect 3 "refuses a firewall attachment delete" bash "$cp" "$(mutate attdel "$FW77 | .resource_changes += [$a]")"
a="${ATTX/ATTACTS/[\"no-op\"]}"; a="${a/ATTBEFORE/$ATTB}"; a="${a/ATTAFTER/$ATTB}"
expect 0 "accepts an unchanged (no-op) firewall attachment" bash "$cp" "$(mutate attnoop "$FW77 | .resource_changes += [$a]")"
# Kimi review F1: the firewall_ids of a server that exists never change in place.
# refuses_with PATTERN DESCRIPTION PLAN [OPTIONS]: exit 3 and a refusal line that matches PATTERN.
refuses_with() {
  local pat="$1" what="$2"
  shift 2
  expect 3 "$what" bash "$cp" "$@"
  grep -qE -- "$pat" "$TMP/out" || bad "$what: no refusal that matches '$pat'"
}
UPD1='(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change) |= (.actions = ["update"] | .before = (.after + {"id": 4241}) | .after.id = 4241)'
SRV1='(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change)'
refuses_with 'stg-1.*in place' "refuses removing the firewall from an existing server" "$(mutate fwrm "$FW77 | $UPD1 | $SRV1.after.firewall_ids = []")"
refuses_with 'stg-1.*in place' "refuses adding the firewall again to an existing server ([77] -> [77, 77])" "$(mutate fwadd2 "$FW77 | $UPD1 | $SRV1.after.firewall_ids = [77, 77]")"
refuses_with 'stg-1.*in place' "refuses moving an existing server to the staging firewall ([76] -> [77])" "$(mutate fwmove "$FW77 | $UPD1 | $SRV1.before.firewall_ids = [76]")"
refuses_with 'stg-1.*in place' "refuses an existing server whose firewall_ids become unknown" "$(mutate fwunk2 "$UPD1 | $SRV1.after_unknown.firewall_ids = true")"
refuses_with 'stg-1.*creates no firewall' "refuses a created server with an unknown firewall ID when the plan creates no firewall" "$(mutate fwunk3 "$FW77 | $SRV1.after_unknown.firewall_ids = true")"
expect 0 "accepts an existing server updated in place with the same firewall_ids" bash "$cp" "$(mutate fwsame "$FW77 | $UPD1 | $SRV1.before.labels.role = \"old\"")"
expect 0 "accepts replacing a server with --allow-replace (created anew with the staging firewall)" bash "$cp" "$(mutate srvrep77 "$FW77 | $UPD1 | $SRV1 |= (.actions = [\"delete\",\"create\"] | .after.id = null)")" --allow-replace
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

echo "== check-plan.sh: code review 2 G-1"
expect 3 "refuses a server with a known firewall ID that is not the staging firewall" bash "$cp" "$(mutate fwforeign '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change) |= (.after.firewall_ids = [42] | .after_unknown.firewall_ids = [false])')"
expect 3 "refuses a server with a known foreign firewall ID next to an unknown one" bash "$cp" "$(mutate fwmix '(.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change) |= (.after.firewall_ids = [null, 42] | .after_unknown.firewall_ids = [true, false])')"
expect 0 "accepts servers with the ID of the staging firewall of this plan" bash "$cp" "$(mutate fwown "$FW77")"
expect 3 "refuses a firewall with no rules and apply_to" bash "$cp" "$(mutate fwnorules '(.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.after.rule = [] | .after.apply_to = [{"label_selector":"env=prod"}] | .after_unknown.rule = [])')"
expect 3 "refuses firewall apply_to unknown at plan time" bash "$cp" "$(mutate fwapplyu '(.resource_changes[] | select(.type == "hcloud_firewall") | .change.after_unknown.apply_to) = true')"
expect 3 "refuses firewall rules wholly unknown at plan time" bash "$cp" "$(mutate fwruleu '(.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.after.rule = [] | .after_unknown.rule = true)')"
expect 3 "refuses one firewall rule unknown at plan time" bash "$cp" "$(mutate fwrule1u '(.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.after.rule += [null] | .after_unknown.rule += [true])')"
expect 3 "refuses a primary IP assigned to the build server" bash "$cp" "$(mutate pipbb '(.resource_changes[] | select(.address == "hcloud_primary_ip.v4[\"stg-1\"]") | .change.after.assignee_id) = 99999')"
expect 3 "refuses a primary IP assigned to a server outside the plan" bash "$cp" "$(mutate pipout '(.resource_changes[] | select(.address == "hcloud_primary_ip.v4[\"stg-1\"]") | .change.after.assignee_id) = 4242')"
expect 0 "accepts a primary IP assigned to a server of this plan" bash "$cp" "$(mutate pipin "$FW77"' | (.resource_changes[] | select(.address == "hcloud_server.node[\"stg-1\"]") | .change) |= (.actions = ["update"] | .before = (.after + {"id":4243}) | .after.id = 4243) | (.resource_changes[] | select(.address == "hcloud_primary_ip.v4[\"stg-1\"]") | .change.after.assignee_id) = 4243')"

echo "== check-plan.sh: apply_to of an existing firewall (2026-11-11)"
# The servers exist (IDs 4241-4243) and carry the existing firewall 77; Hetzner reports them in
# apply_to. The firewall rules change in place (the admin source moves to the build server).
EXIST='(.resource_changes[] | select(.type == "hcloud_server")) |= (.change |= (.actions = ["no-op"] | .after.id = (4240 + (.after.name | ltrimstr("stg-") | tonumber)) | .before = .after | .after_unknown = {}))
  | (.resource_changes[] | select(.type == "hcloud_firewall") | .change) |= (.actions = ["update"] | .after.id = "77"
      | .after.apply_to = [{"label_selector":"","server":4241},{"label_selector":"","server":4242},{"label_selector":"","server":4243}]
      | .before = (.after | .rule = [.rule[] | .source_ips = ["198.51.100.20/32"]]) | .after_unknown = {})'
expect 0 "accepts an existing firewall whose apply_to stays the same and lists the plan's servers" bash "$cp" "$(mutate apkept "$EXIST")"
FWC='(.resource_changes[] | select(.type == "hcloud_firewall") | .change)'
S4244='{"label_selector":"","server":4244}'
SOUT='{"label_selector":"","server":4242424}'
SEL='{"label_selector":"env=staging,role=node","server":null}'
expect 3 "refuses apply_to that adds a server" bash "$cp" "$(mutate apadd "$EXIST | $FWC.after.apply_to += [$S4244]")"
expect 3 "refuses unchanged apply_to with a server outside the plan" bash "$cp" "$(mutate apout "$EXIST | $FWC |= (.before.apply_to += [$SOUT] | .after.apply_to += [$SOUT])")"
expect 3 "refuses unchanged apply_to that lists the build server" env STG_BUILDBOX_ID=4243 bash "$cp" "$TMP/apkept.json"
expect 3 "refuses unchanged apply_to with a label selector" bash "$cp" "$(mutate apsel "$EXIST | $FWC |= (.before.apply_to += [$SEL] | .after.apply_to += [$SEL])")"
expect 3 "refuses apply_to on a new firewall (no before)" bash "$cp" "$(mutate apnew "$EXIST | $FWC |= (.actions = [\"create\"] | .before = null)")"
expect 3 "refuses apply_to of an existing firewall that is unknown at plan time" bash "$cp" "$(mutate apunk "$EXIST | $FWC.after_unknown.apply_to = [{\"server\":true}]")"
expect 3 "refuses the existing firewall's rules from the old address" bash "$cp" "$(mutate apold "$EXIST | $FWC.after.rule[0].source_ips = [\"198.51.100.20/32\"]")"
# Kimi review F2: apply_to that loses servers, or is cleared, is refused (checked before or after).
expect 3 "refuses apply_to that removes one server" bash "$cp" "$(mutate aprm1 "$EXIST | $FWC.after.apply_to |= .[1:]")"
expect 3 "refuses clearing apply_to ([])" bash "$cp" "$(mutate apclr "$EXIST | $FWC.after.apply_to = []")"
expect 3 "refuses clearing apply_to (null)" bash "$cp" "$(mutate apnull "$EXIST | $FWC.after.apply_to = null")"
# Kimi review F3: a replace is a new firewall, so its apply_to is refused even with --allow-replace.
expect 3 "refuses apply_to on a replaced firewall (--allow-replace)" bash "$cp" "$(mutate aprep "$EXIST | $FWC |= (.actions = [\"delete\",\"create\"] | .after.id = null | .after_unknown = {\"id\":true})")" --allow-replace
expect 3 "refuses apply_to on a replaced firewall (create,delete, --allow-replace)" bash "$cp" "$(mutate aprep2 "$EXIST | $FWC |= (.actions = [\"create\",\"delete\"] | .after.id = null | .after_unknown = {\"id\":true})")" --allow-replace

bash "$STG/price.sh" "$FIX/pricing.json" "$TMP/child.json" >"$TMP/out" 2>&1
grep -qE "server cx23 \(hel1\) +5 " "$TMP/out" && ok "price.sh counts the servers of child modules" || { bad "price.sh child modules"; cat "$TMP/out"; }

echo "== plan-scrub.jq (code review F-5)"
# Probe values in the places where a real plan holds secrets. The key names and values are
# plain probes (scrub_probe_N, SCRUB-PROBE-N), so secret scanners do not read them as credentials.
jq '. + {"variables":{"scrub_probe_1":{"value":"SCRUB-PROBE-1"}},
         "prior_state":{"values":{"root_module":{"resources":[{"type":"talos_machine_secrets","values":{"scrub_probe_2":"SCRUB-PROBE-2"}}]}}},
         "configuration":{"provider_config":{"x":{"expressions":{"scrub_probe_3":{"constant_value":"SCRUB-PROBE-3"}}}}},
         "output_changes":{"talosconfig":{"actions":["no-op"],"before":"SCRUB-PROBE-4","after":"SCRUB-PROBE-4","before_sensitive":true,"after_sensitive":true}}}
    | .resource_changes += [
        {"address":"talos_machine_secrets.this","mode":"managed","type":"talos_machine_secrets","change":{"actions":["no-op"],"before":{"scrub_probe_5":{"certs":{"etcd":{"probe":"SCRUB-PROBE-5"}}}},"after":{"scrub_probe_5":{"certs":{"etcd":{"probe":"SCRUB-PROBE-5"}}}}}},
        {"address":"data.talos_client_configuration.this","mode":"data","type":"talos_client_configuration","change":{"actions":["read"],"before":null,"after":{"scrub_probe_6":"SCRUB-PROBE-6"}}},
        {"address":"terraform_data.x","mode":"managed","type":"terraform_data","change":{"actions":["update"],"before":{"input":{"a":"SCRUB-PROBE-7","b":"keep"}},"after":{"input":{"a":"SCRUB-PROBE-7","b":"keep"}},"before_sensitive":{"input":{"a":true}},"after_sensitive":{"input":{"a":true}}}}]
    | .planned_values.root_module.resources += [{"type":"terraform_data","values":{"input":{"a":"SCRUB-PROBE-8"}},"sensitive_values":{"input":{"a":true}}}]
    | .planned_values.outputs = {"talosconfig":{"sensitive":true,"value":"SCRUB-PROBE-9"}}' \
  "$FIX/plan-good.json" >"$TMP/probe-plan.json"
jq -f "$STG/plan-scrub.jq" "$TMP/probe-plan.json" >"$TMP/scrubbed.json"
if grep -q SCRUB-PROBE "$TMP/scrubbed.json"; then bad "plan-scrub.jq leaves a probe value: $(grep -o 'SCRUB-PROBE-[0-9]' "$TMP/scrubbed.json" | sort -u | tr '\n' ' ')"; else ok "plan-scrub.jq removes the variables, prior state, configuration, sensitive, Talos, data source and output values"; fi
jq -e '[.resource_changes[] | select(.type == "terraform_data") | .change.after.input.b] | index("keep")' "$TMP/scrubbed.json" >/dev/null && ok "plan-scrub.jq keeps the values that are not sensitive" || bad "plan-scrub.jq drops plain values"
expect 0 "check-plan.sh accepts the scrubbed plan" bash "$cp" "$TMP/scrubbed.json"
expect 1 "check-plan.sh refuses the unscrubbed plan" bash "$cp" "$TMP/probe-plan.json"
if grep -nE 'tofu show -json "' "$STG/steps.sh" "$STG/ops/destroy-steps.sh" | grep -v 'jq -f "$HERE/plan-scrub.jq"' | grep -q .; then
  bad "a 'tofu show -json' in steps.sh or destroy-steps.sh does not pipe through plan-scrub.jq"
else
  ok "every 'tofu show -json' pipes through plan-scrub.jq before it is written"
fi

echo "== sweep (code review F-6)"
printf '%s\n' "servers stg-1" "servers stg-5" "servers ruum-buildbox" "servers web" "primary_ips stg-2-v4" "primary_ips other-v4" \
  "load_balancers ruum-staging" "load_balancers ruum-prod" "networks ruum-staging" "firewalls ruum-staging-nodes" "firewalls ruum-buildbox" \
  "placement_groups ruum-staging-nodes" "rrset ruum.space staging A" "rrset ruumapps.net *.content.staging AAAA" "rrset ruum.space @ A" \
  "rrset ruum.space notify.staging MX" "rrset ruum.space www A" "rrset ruumapps.net staging A" "snapshot talos" >"$TMP/sweep-in.txt"
HERE="$STG" bash "$STG/ops/destroy-steps.sh" sweep-filter <"$TMP/sweep-in.txt" >"$TMP/sweep-out.txt" 2>/dev/null
want="$(printf '%s\n' "servers stg-1" "primary_ips stg-2-v4" "load_balancers ruum-staging" "networks ruum-staging" "firewalls ruum-staging-nodes" \
  "placement_groups ruum-staging-nodes" "rrset ruum.space staging A" "rrset ruumapps.net *.content.staging AAAA")"
if [[ "$(cat "$TMP/sweep-out.txt")" == "$want" ]]; then ok "sweep keeps only the staging names (rrsets.json), never the build server, the apex or notify"; else bad "sweep filter"; diff <(echo "$want") "$TMP/sweep-out.txt"; fi
jq -e --slurpfile r "$STG/rrsets.json" 'length == ($r[0] | length)' "$STG/rrsets.json" >/dev/null &&
  ! grep -q 'STAGING_RRSETS=' "$STG/ops/destroy-steps.sh" && ok "one rrset list for check-plan and sweep (rrsets.json)" || bad "rrset lists"

echo "== deploy-chart.sh arguments (code review F-3)"
dc="$STG/ops/deploy-chart.sh"
C=0123456789abcdef0123456789abcdef01234567
SV="ghcr.io/nemecec/ruum-server@sha256:$(printf '%064d' 0)"
ED="ghcr.io/nemecec/ruum-edge@sha256:$(printf '%064d' 1)"
expect 0 "accepts valid arguments" bash "$dc" "$C" "$SV" "$ED" --expect-deployed "$C" --acme zerossl --acme-email ops+stg@example.com --validate-only
expect 0 "accepts a first deploy (--expect-deployed none)" bash "$dc" "$C" "$SV" "$ED" --expect-deployed none --validate-only
expect 0 "accepts a rollback to a live release" bash "$dc" "$C" "$SV" "$ED" --expect-deployed "$C" --rollback --validate-only
expect 1 "refuses a deploy without --expect-deployed (the live release that deploy.sh checked)" bash "$dc" "$C" "$SV" "$ED" --validate-only
expect 1 "refuses a short --expect-deployed" bash "$dc" "$C" "$SV" "$ED" --expect-deployed 0123456 --validate-only
expect 1 "refuses a rollback without a live release" bash "$dc" "$C" "$SV" "$ED" --expect-deployed none --rollback --validate-only
expect 1 "refuses --rollback with --rollback-older" bash "$dc" "$C" "$SV" "$ED" --expect-deployed "$C" --rollback --rollback-older --validate-only
expect 1 "refuses a short commit" bash "$dc" 0123456 "$SV" "$ED" --validate-only
expect 1 "refuses a commit with a --set injection" bash "$dc" "x,images.server=evil.example/a@sha256:$(printf '%064d' 0)" "$SV" "$ED" --validate-only
expect 1 "refuses a server image from another registry" bash "$dc" "$C" "evil.example/nemecec/ruum-server@sha256:$(printf '%064d' 0)" "$ED" --validate-only
expect 1 "refuses the edge image in the server slot" bash "$dc" "$C" "$ED" "$ED" --validate-only
expect 1 "refuses an issuer outside the list" bash "$dc" "$C" "$SV" "$ED" --acme letsencrypt --validate-only
expect 1 "refuses an email with a comma" bash "$dc" "$C" "$SV" "$ED" --acme zerossl --acme-email "a@b.io,adminSources={0.0.0.0/0}" --validate-only
expect 1 "refuses zerossl without an email" bash "$dc" "$C" "$SV" "$ED" --acme zerossl --validate-only
grep -vE '^[[:space:]]*#' "$dc" | grep -qE -- '--set[ =]' && bad "deploy-chart.sh passes values with --set" || ok "deploy-chart.sh gives helm a values file, never --set"
grep -q 'cosign_verify_provenance' "$dc" && ok "deploy-chart.sh verifies the provenance in the tools container" || bad "deploy-chart.sh skips the provenance check"

echo "== the Mac only (decision of the person, 2026-11-11)"
st="$STG/steps.sh"
expect 0 "a public IPv4 becomes one /32" env RUUM_STAGING_WORK="$TMP" bash "$st" ipv4-check 8.8.8.8
[[ "$(tr -d '\n' <"$TMP/out")" == '["8.8.8.8/32"]' ]] && ok "ipv4-check prints [\"8.8.8.8/32\"]" || { bad "ipv4-check output"; cat "$TMP/out"; }
for ip in 10.1.2.3 192.168.1.10 172.16.0.1 100.64.0.1 127.0.0.1 169.254.1.1 0.0.0.0 224.0.0.1 203.0.113.5 8.8.8.256 08.8.8.8 8.8.8.8/24 2a01:4f8::1; do
  expect 1 "refuses the admin address $ip" env RUUM_STAGING_WORK="$TMP" bash "$st" ipv4-check "$ip"
done
expect 1 "admin-cidrs needs HCLOUD_TOKEN (the build server's address from the Hetzner API)" env RUUM_STAGING_WORK="$TMP" bash "$st" admin-cidrs </dev/null
grep -q "servers?name=ruum-buildbox" "$st" && grep -q 'public_net.ipv4.ip' "$st" && ok "admin-cidrs reads ruum-buildbox's public IPv4 from the Hetzner API" || bad "admin-cidrs source"
hits="$(grep -nE 'ipify|icanhazip|ifconfig\.me|checkip|RUUM_ADMIN_IP' "$STG"/*.sh "$STG"/ops/*.sh "$STG"/k8s/*.sh "$STG"/tofu/*/*.tf "$STG/README.md" 2>/dev/null || true)"
[[ -z "$hits" ]] && ok "no Mac IP detection and no RUUM_ADMIN_IP left" || { bad "Mac IP logic is left"; echo "$hits"; }
for step in apply platform; do
  expect 1 "steps.sh $step refuses without RUUM_STAGING_APPLY=yes" env -u RUUM_STAGING_APPLY RUUM_STAGING_WORK="$TMP" bash "$st" "$step" </dev/null
  grep -q 'needs RUUM_STAGING_APPLY=yes' "$TMP/out" || bad "steps.sh $step: the refusal names RUUM_STAGING_APPLY"
done
hits="$(grep -nE '(^|[^a-z])(rsync|scp)( |$)|ruum-ops|buildbox_ed25519|staging_ops_ed25519|RUUM_STAGING_REMOTE|RUUM_BUILD_HOST|/opt/ruum-staging|2\.31\.194\.2' \
  "$STG"/*.sh "$STG"/ops/*.sh "$STG"/k8s/*.sh "$STG"/charts/ruum/values.yaml "$STG"/tofu/*/*.tf 2>/dev/null | grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' || true)"
[[ -z "$hits" ]] && ok "no rsync or scp, no hard-coded build-server address, no agent credential path" || { bad "build-server paths are left"; echo "$hits"; }
hits="$(grep -lE '(^|[^a-z-])ssh( |$)' "$STG"/*.sh "$STG"/ops/*.sh "$STG"/k8s/*.sh 2>/dev/null | xargs -n1 basename 2>/dev/null | grep -vxE 'lib.sh|tunnel-setup.sh|buildbox-tunnel-user.sh|tunnel.sh|tunnel-relay.sh' || true)"
[[ -z "$hits" ]] && ok "ssh only in the admin tunnel (lib.sh, ops/tunnel-setup.sh)" || { bad "ssh outside the admin tunnel: $hits"; }
[[ ! -e "$STG/ops/install-ops-user.sh" && ! -e "$STG/ops/ssh-entry.sh" && ! -e "$STG/remote.sh" ]] && ok "no ruum-ops installer, ssh entry or remote mode" || bad "remote mode files are left"
grep -q -- '-v "$STG_ROOT:$STG_ROOT:ro"' "$STG/lib.sh" && ok "the checkout is mounted read-only in the tools container" || bad "lib.sh mounts the checkout writable"
grep -q -- '--cap-drop ALL --security-opt no-new-privileges' "$STG/lib.sh" && ok "the tools container runs without capabilities" || bad "lib.sh: capabilities"
grep -qE 'rw\) \[\[ "\$\{RUUM_STAGING_APPLY:-\}" != yes \]\] \|\| env\+=\(-e RUUM_STAGING_APPLY=yes\)' "$STG/lib.sh" && ok "only rw steps get RUUM_STAGING_APPLY" || bad "lib.sh passes RUUM_STAGING_APPLY to every step"
. "$STG/versions.env"
[[ "$TOOLS_BASE_IMAGE" =~ ^alpine:[0-9.]+@sha256:[0-9a-f]{64}$ ]] && ok "the tools image base is pinned by digest" || bad "TOOLS_BASE_IMAGE is not pinned by digest"
[[ "$(bash "$STG/tools-image.sh" name 2>/dev/null)" =~ ^ruum-staging-tools:[0-9a-f]{16}$ ]] && ok "the tools image name is a hash of its pins" || bad "tools-image.sh name"
grep -vE '^[[:space:]]*#' "$STG/tools-image.sh" | grep -q 'docker build' && bad "tools-image.sh uses docker build (build cache)" || ok "tools-image.sh leaves no build cache (run and commit)"

echo "== admin tunnel (2026-11-11)"
. "$STG/tunnel.sh"
T1=$'bastion 5.9.0.2\nnode stg-1 5.9.0.11 10.20.1.11\nnode stg-2 5.9.0.12 10.20.1.12\nnode stg-3 5.9.0.13 10.20.1.13'
if tunnel_parse "$T1" >"$TMP/out"; then ok "tunnel targets parse"; else bad "tunnel targets parse"; cat "$TMP/out"; fi
[[ "$(tunnel_forward_args | tr '\n' ' ')" == "-L 127.0.0.1:16443:5.9.0.11:6443 -L 127.0.0.1:15001:5.9.0.11:50000 -L 127.0.0.1:15002:5.9.0.12:50000 -L 127.0.0.1:15003:5.9.0.13:50000 " ]] &&
  ok "ssh forwards: 16443 -> stg-1:6443, 1500N -> stg-N:50000, all on 127.0.0.1" || { bad "ssh forwards"; tunnel_forward_args; }
[[ "$(tunnel_ports)" == "16443 15001 15002 15003" ]] && ok "tunnel ports" || bad "tunnel ports: $(tunnel_ports)"
[[ "$(tunnel_talos_endpoints)" == "127.0.0.1:15001 127.0.0.1:15002 127.0.0.1:15003" && "$(tunnel_talos_nodes)" == "10.20.1.11 10.20.1.12 10.20.1.13" ]] &&
  ok "talosconfig: endpoints through the tunnel, nodes by private address" || bad "talosconfig endpoints or nodes"
tbad() { if tunnel_parse "$2" >/dev/null; then bad "tunnel targets accept $1"; else ok "tunnel targets refuse $1"; fi; }
tbad "a private build server address" "${T1/bastion 5.9.0.2/bastion 10.0.0.2}"
tbad "a private node public address" "${T1/node stg-2 5.9.0.12/node stg-2 192.168.1.12}"
tbad "a node private address outside 10.20.1.0/24" "${T1/10.20.1.13/10.20.2.13}"
tbad "a node that is the build server" "${T1/node stg-3 5.9.0.13/node stg-3 5.9.0.2}"
tbad "another server name" "${T1/stg-3/web}"
tbad "no stg-1" "$(grep -v stg-1 <<<"$T1")"
tbad "no build server" "$(grep -v bastion <<<"$T1")"
tbad "a node twice" "$T1"$'\nnode stg-2 5.9.0.14 10.20.1.14'
tbad "an extra field" "${T1/10.20.1.12/10.20.1.12 x}"
for o in ExitOnForwardFailure=yes StrictHostKeyChecking=yes 'UserKnownHostsFile=$STG_TUNNEL_KNOWN_HOSTS' GlobalKnownHostsFile=/dev/null \
  ForwardAgent=no ForwardX11=no RequestTTY=no IdentitiesOnly=yes BatchMode=yes; do
  grep -qF -- "-o $o" "$STG/lib.sh" || grep -qF -- "-o \"$o\"" "$STG/lib.sh" || bad "lib.sh tunnel ssh option $o is missing"
done
ok "the tunnel ssh options: ExitOnForwardFailure, a fixed known-hosts file, no agent, X11 or tty"
grep -qE 'ssh "\$\{STG_TUNNEL_OPTS\[@\]\}" -o ControlMaster=yes -f -N "\$\{fwd\[@\]\}"' "$STG/lib.sh" && ok "the tunnel runs no command (-N) and only the forwards" || bad "the tunnel ssh command line"
grep -q 'STG_TUNNEL_KEY="$HOME/.config/ruum/staging_tunnel_ed25519"' "$STG/lib.sh" && ok "the tunnel key is ~/.config/ruum/staging_tunnel_ed25519" || bad "tunnel key path"
grep -q "ssh-keygen -q -t ed25519" "$STG/ops/tunnel-setup.sh" && grep -q 'if \[\[ ! -e "$STG_TUNNEL_KEY" \]\]' "$STG/ops/tunnel-setup.sh" &&
  ! grep -qE 'cat "\$STG_TUNNEL_KEY"( |$|\))' "$STG/ops/tunnel-setup.sh" && ok "tunnel-setup.sh makes the key only when it is missing and never prints it" || bad "tunnel-setup.sh key handling"
bt="$STG/ops/buildbox-tunnel-user.sh"
KEY="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPrObEPrObEPrObEPrObEPrObEPrObEPrObEPrObEPrO ruum-staging-tunnel"
expect 0 "the restricted key line renders" bash "$bt" --print-authorized-keys "$KEY" 5.9.0.11 5.9.0.12 5.9.0.13
[[ "$(cat "$TMP/out")" == 'restrict,port-forwarding,permitopen="5.9.0.11:6443",permitopen="5.9.0.11:50000",permitopen="5.9.0.12:6443",permitopen="5.9.0.12:50000",permitopen="5.9.0.13:6443",permitopen="5.9.0.13:50000",command="/bin/false" '"$KEY" ]] &&
  ok "authorized_keys: restrict, port-forwarding, permitopen per node port, command /bin/false" || { bad "authorized_keys line"; cat "$TMP/out"; }
expect 1 "the key line refuses a private node address" bash "$bt" --print-authorized-keys "$KEY" 10.20.1.11
expect 1 "the key line refuses an RSA key" bash "$bt" --print-authorized-keys "ssh-rsa AAAAB3NzaC1yc2E= x" 5.9.0.11
expect 1 "the key line refuses a key with options" bash "$bt" --print-authorized-keys "command=\"/bin/sh\" $KEY" 5.9.0.11
expect 1 "the key line refuses a quote in an address" bash "$bt" --print-authorized-keys "$KEY" '5.9.0.11",permitopen="*:*'
expect 0 "the sshd config renders" bash "$bt" --print-sshd-config 5.9.0.11 5.9.0.12 5.9.0.13
cp "$TMP/out" "$TMP/sshd.conf"
for want in '^AllowTcpForwarding no$' '^Match User ruumtunnel$' '^    AllowTcpForwarding local$' '^    PermitOpen 5\.9\.0\.11:6443 5\.9\.0\.11:50000 5\.9\.0\.12:6443 5\.9\.0\.12:50000 5\.9\.0\.13:6443 5\.9\.0\.13:50000$' \
  '^    X11Forwarding no$' '^    AllowAgentForwarding no$' '^    PermitTTY no$' '^    ForceCommand /bin/false$' '^    PermitListen none$' '^    AuthorizedKeysFile /etc/ssh/authorized_keys/ruumtunnel$' '^Match all$'; do
  grep -qE "$want" "$TMP/sshd.conf" || bad "sshd config: no line $want"
done
ok "sshd: forwarding only for ruumtunnel, only to the node ports; no X11, agent or tty; ForceCommand /bin/false"
if [[ -x /usr/sbin/sshd ]] && command -v ssh-keygen >/dev/null; then
  ssh-keygen -q -t ed25519 -N '' -f "$TMP/hk" </dev/null >/dev/null
  if /usr/sbin/sshd -t -f "$TMP/sshd.conf" -h "$TMP/hk" 2>"$TMP/out"; then ok "sshd -t accepts the sshd config"; else bad "sshd -t"; cat "$TMP/out"; fi
  eff="$(/usr/sbin/sshd -T -f "$TMP/sshd.conf" -h "$TMP/hk" -C user=ruumtunnel,host=x,addr=192.0.2.1 2>/dev/null)"
  grep -qx 'allowtcpforwarding local' <<<"$eff" && grep -qx 'forcecommand /bin/false' <<<"$eff" && grep -qx 'permittty no' <<<"$eff" &&
    /usr/sbin/sshd -T -f "$TMP/sshd.conf" -h "$TMP/hk" -C user=ruumadmin,host=x,addr=192.0.2.1 2>/dev/null | grep -qx 'allowtcpforwarding no' &&
    ok "sshd -T: ruumtunnel forwards locally only, every other user not at all" || bad "sshd -T effective settings"
fi
grep -q 'sshd -t' "$bt" && grep -q 'systemctl reload' "$bt" && grep -q "chown root:root" "$bt" && grep -q 'install -d -o root -g root -m 0755' "$bt" &&
  ok "buildbox-tunnel-user.sh validates with sshd -t before the reload; root-owned key file" || bad "buildbox-tunnel-user.sh order or ownership"
grep -q -- '--shell /usr/sbin/nologin' "$bt" && grep -q "usermod --password '\*'" "$bt" && ok "ruumtunnel: shell nologin, no password" || bad "ruumtunnel user"
grep -q 'refuse_talos_changes' "$STG/steps.sh" && ok "apply refuses a plan that changes the Talos machine config (closed to the tools container)" || bad "refuse_talos_changes"

echo "== admin tunnel: control socket and forward checks (Kimi review F4)"
F4="$TMP/f4"
mkdir -p "$F4/home/.config/ruum" "$F4/work" "$F4/work2"
ssh-keygen -q -t ed25519 -N '' -C t -f "$F4/home/.config/ruum/staging_tunnel_ed25519" </dev/null >/dev/null
echo "5.9.0.2 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPrObEPrObEPrObEPrObEPrObEPrObEPrObEPrObEPrO" >"$F4/home/.config/ruum/staging_tunnel_known_hosts"
# inlib SCRIPT [ARGS...]: run SCRIPT with lib.sh sourced, a test HOME and work folder.
inlib() { local c="$1"; shift; HOME="$F4/home" RUUM_STAGING_WORK="$F4/work" bash -c "set -uo pipefail; . '$STG/lib.sh'; $c" _ "$@"; }
fp() { inlib 'tunnel_parse "$1" >/dev/null && stg_tunnel_fingerprint' "$1"; }
fp1="$(fp "$T1")"
[[ "$fp1" =~ ^[0-9a-f]{16}$ && "$(fp "$T1")" == "$fp1" ]] && ok "the tunnel fingerprint is 16 hex and stable" || bad "tunnel fingerprint: '$fp1'"
[[ "$(fp "${T1/5.9.0.13/5.9.0.23}")" != "$fp1" ]] && ok "a changed node address gives another control socket" || bad "fingerprint ignores the forwards"
[[ "$(fp "$(grep -v stg-3 <<<"$T1")")" != "$fp1" ]] && ok "a removed node gives another control socket" || bad "fingerprint ignores a removed node"
[[ "$(fp "${T1/bastion 5.9.0.2/bastion 5.9.0.3}")" != "$fp1" ]] && ok "another build server gives another control socket" || bad "fingerprint ignores the build server"
cp "$F4/home/.config/ruum/staging_tunnel_known_hosts" "$F4/kh"
echo "5.9.0.2 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherOtherOtherOtherOtherOtherOtherOtherOthe" >"$F4/home/.config/ruum/staging_tunnel_known_hosts"
[[ "$(fp "$T1")" != "$fp1" ]] && ok "a new host key gives another control socket" || bad "fingerprint ignores the host key"
cp "$F4/kh" "$F4/home/.config/ruum/staging_tunnel_known_hosts"
mv "$F4/home/.config/ruum/staging_tunnel_ed25519" "$F4/key.old"
ssh-keygen -q -t ed25519 -N '' -C t -f "$F4/home/.config/ruum/staging_tunnel_ed25519" </dev/null >/dev/null
[[ "$(fp "$T1")" != "$fp1" ]] && ok "a new tunnel key gives another control socket" || bad "fingerprint ignores the key"
grep -q 'STG_TUNNEL_SOCK="$STG_WORK/tunnel-$line.sock"' "$STG/lib.sh" && ok "the control socket is named after the fingerprint" || bad "control socket name"
# Other control sockets (the old name, another fingerprint) are closed; ours stays, unless "all".
if command -v python3 >/dev/null; then
  python3 -c 'import os, socket, sys
os.chdir(sys.argv[1])
for n in sys.argv[2:]:
    socket.socket(socket.AF_UNIX).bind(n)' "$F4/work" tunnel.sock tunnel-aaaaaaaaaaaaaaaa.sock tunnel-bbbbbbbbbbbbbbbb.sock
  inlib 'log="$1"; ssh() { printf "%s\n" "$*" >>"$log"; }; TUNNEL_BASTION=5.9.0.2; STG_TUNNEL_SOCK="$STG_WORK/tunnel-bbbbbbbbbbbbbbbb.sock"; stg_tunnel_close_others 2>/dev/null' "$F4/ssh.log"
  [[ ! -e "$F4/work/tunnel.sock" && ! -e "$F4/work/tunnel-aaaaaaaaaaaaaaaa.sock" && -S "$F4/work/tunnel-bbbbbbbbbbbbbbbb.sock" ]] &&
    grep -q 'tunnel.sock -O exit' "$F4/ssh.log" && grep -q 'tunnel-aaaaaaaaaaaaaaaa.sock -O exit' "$F4/ssh.log" && ! grep -q bbbbbbbb "$F4/ssh.log" &&
    ok "stg_tunnel_close_others closes the tunnels of other control sockets and keeps the current one" || { bad "stg_tunnel_close_others"; cat "$F4/ssh.log"; }
  inlib 'log="$1"; ssh() { printf "%s\n" "$*" >>"$log"; }; TUNNEL_BASTION=5.9.0.2; STG_TUNNEL_SOCK="$STG_WORK/tunnel-bbbbbbbbbbbbbbbb.sock"; stg_tunnel_close_others all 2>/dev/null' "$F4/ssh.log"
  [[ ! -e "$F4/work/tunnel-bbbbbbbbbbbbbbbb.sock" ]] && grep -q 'tunnel-bbbbbbbbbbbbbbbb.sock -O exit' "$F4/ssh.log" &&
    ok "stg_tunnel_close_others all closes the current tunnel too (tunnel-setup.sh)" || { bad "stg_tunnel_close_others all"; cat "$F4/ssh.log"; }
  # stg_tunnel_verify: every local port must listen, and ssh -W to every node port must succeed.
  python3 -c 'import socket, sys, time
ss = []
for p in sys.argv[1:]:
    s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", int(p))); s.listen(8); ss.append(s)
time.sleep(30)' 38443 38001 38002 38003 &
  lpid=$!
  sleep 1
  V='TUNNEL_KUBE_PORT=38443; TUNNEL_TALOS_BASE=38000; STG_TUNNEL_OPTS=(-F /dev/null); tunnel_parse "$1" >/dev/null'
  if inlib "$V; ssh() { [[ \"\$*\" == *' -W '* ]]; }; stg_tunnel_verify" "$T1" >"$TMP/out" 2>&1; then ok "stg_tunnel_verify passes when every forward listens and opens"; else bad "stg_tunnel_verify (all good)"; cat "$TMP/out"; fi
  if inlib "$V; ssh() { [[ \"\$*\" != *'-W 5.9.0.12:50000 '* ]]; }; stg_tunnel_verify" "$T1" >"$TMP/out" 2>&1; then bad "stg_tunnel_verify passes when the build server cannot open a node port"; else grep -q '5.9.0.12:50000' "$TMP/out" && ok "stg_tunnel_verify fails when the build server cannot open a node port" || { bad "stg_tunnel_verify message"; cat "$TMP/out"; }; fi
  if inlib "TUNNEL_KUBE_PORT=38443; TUNNEL_TALOS_BASE=38010; STG_TUNNEL_OPTS=(-F /dev/null); tunnel_parse \"\$1\" >/dev/null; ssh() { return 0; }; stg_tunnel_verify" "$T1" >"$TMP/out" 2>&1; then bad "stg_tunnel_verify passes when a local port does not listen"; else grep -q 'nothing listens on 127.0.0.1:38011' "$TMP/out" && ok "stg_tunnel_verify fails when a local port does not listen" || { bad "stg_tunnel_verify listener message"; cat "$TMP/out"; }; fi
  kill "$lpid" 2>/dev/null; wait "$lpid" 2>/dev/null
else
  echo "== admin tunnel: control socket and listener tests skipped (no python3)"
fi
awk '/^stg_tunnel_up\(\)/,/^}/' "$STG/lib.sh" >"$TMP/up.txt"
grep -q 'stg_tunnel_verify ||' "$TMP/up.txt" && [[ "$(grep -n 'stg_tunnel_verify ||' "$TMP/up.txt" | cut -d: -f1)" -lt "$(grep -n 'STG_TUNNEL_PORTS="$(tunnel_ports)"' "$TMP/up.txt" | cut -d: -f1)" ]] &&
  ok "stg_tunnel_up checks every forward before it sets STG_TUNNEL_PORTS" || bad "stg_tunnel_up does not verify the forwards first"
grep -q 'if \[\[ "$STG_TUNNEL_FRESH" != 1 \]\] && ssh' "$TMP/up.txt" && grep -q 'stg_tunnel_close_others all' "$TMP/up.txt" &&
  awk '/^STG_TUNNEL_FRESH=1$/ {f=1} f && /^stg_tunnel_up$/ {ok=1} END {exit !ok}' "$STG/ops/tunnel-setup.sh" &&
  ok "tunnel-setup.sh closes every tunnel and tests a new one (never joins)" || bad "tunnel-setup.sh fresh tunnel"

echo "== buildbox-tunnel-user.sh rollback (Kimi review F5)"
R="$TMP/f5"
mkdir -p "$R/stub" "$R/etc/keys" "$R/etc/sshd_config.d"
printf 'Include /etc/ssh/sshd_config.d/*.conf\n' >"$R/etc/sshd_config"
echo "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPrObEPrObEPrObEPrObEPrObEPrObEPrObEPrObEPrO root@bb" >"$R/etc/hostkey.pub"
for c in useradd usermod chown; do printf '#!/bin/sh\nexit 0\n' >"$R/stub/$c"; done
printf '#!/bin/sh\ncase "$1" in -u) echo 0 ;; -nG) echo ruumtunnel ;; esac\nexit 0\n' >"$R/stub/id"
printf '#!/bin/sh\nfor a; do last="$a"; done\nmkdir -p "$last"\n' >"$R/stub/install"
printf '#!/bin/sh\necho "root:root 644"\n' >"$R/stub/stat"
printf '#!/bin/sh\ncase "$1" in is-active) [ "$3" = ssh ] ;; reload) exit "${FAKE_RELOAD_RC:-0}" ;; esac\n' >"$R/stub/systemctl"
cat >"$R/stub/sshd" <<'SSHD'
#!/bin/sh
[ "$1" = -t ] && exit "${FAKE_T_RC:-0}"
case "$3" in
  user=ruumtunnel,*) cat "$FAKE_EFF" ;;
  *) echo "allowtcpforwarding no" ;;
esac
SSHD
chmod +x "$R"/stub/*
sed -e "s|^KEYS_DIR=.*|KEYS_DIR=$R/etc/keys|" -e "s|^DROPIN=.*|DROPIN=$R/etc/sshd_config.d/10-ruumtunnel.conf|" \
  -e "s|^SSHD=.*|SSHD=$R/stub/sshd|" -e "s|^SSHD_CONFIG=.*|SSHD_CONFIG=$R/etc/sshd_config|" -e "s|^HOSTKEY_PUB=.*|HOSTKEY_PUB=$R/etc/hostkey.pub|" \
  -e '/-x \/usr\/sbin\/nologin/d' "$bt" >"$R/bt.sh"
if [[ "$(grep -c "$R" "$R/bt.sh")" == 5 ]] && ! grep -q '/usr/sbin/nologin && ' "$R/bt.sh"; then
  KF="$R/etc/keys/ruumtunnel" DF="$R/etc/sshd_config.d/10-ruumtunnel.conf"
  printf '%s\n' "allowtcpforwarding local" "permitopen 5.9.0.11:6443 5.9.0.11:50000" "x11forwarding no" "allowagentforwarding no" "permittty no" \
    "permitlisten none" "allowstreamlocalforwarding no" "permittunnel no" "forcecommand /bin/false" "authorizedkeysfile $KF" >"$R/eff-good"
  sed 's/^permittty no$/permittty yes/' "$R/eff-good" >"$R/eff-bad"
  # bt5 DESCRIPTION WANT_EXIT ENV...: run the patched script with the stubs and ENV.
  bt5() { local what="$1" want="$2"; shift 2; expect "$want" "$what" env PATH="$R/stub:$PATH" "$@" bash "$R/bt.sh" "$KEY" 5.9.0.11; }
  old() { echo "old key line" >"$KF"; echo "# old drop-in" >"$DF"; }
  is_old() { [[ "$(cat "$KF")" == "old key line" && "$(cat "$DF")" == "# old drop-in" ]]; }
  old; bt5 "a failed sshd -T check exits non-zero" 1 FAKE_EFF="$R/eff-bad"
  is_old && ok "a failed sshd -T check puts the previous key file and drop-in back" || bad "sshd -T failure leaves the new config"
  rm -f "$KF" "$DF"; bt5 "a failed sshd -T check on a first install exits non-zero" 1 FAKE_EFF="$R/eff-bad"
  [[ ! -e "$KF" && ! -e "$DF" ]] && ok "a failed sshd -T check on a first install removes the new key file and drop-in" || bad "first-install rollback"
  old; bt5 "a failed sshd -t exits non-zero" 1 FAKE_EFF="$R/eff-good" FAKE_T_RC=1
  is_old && ok "a failed sshd -t puts the previous files back" || bad "sshd -t failure rollback"
  old; bt5 "a failed reload exits non-zero" 1 FAKE_EFF="$R/eff-good" FAKE_RELOAD_RC=1
  is_old && ok "a failed reload puts the previous files back" || bad "reload failure rollback"
  old; bt5 "all checks and the reload pass" 0 FAKE_EFF="$R/eff-good"
  grep -q '^hostkey ssh-ed25519 ' "$TMP/out" && grep -q 'permitopen="5.9.0.11:6443"' "$KF" && grep -q '^Match User ruumtunnel$' "$DF" &&
    ok "on success the new key file and drop-in stay and the host key is printed" || { bad "success path"; cat "$TMP/out"; }
  left="$(find "$R/etc/keys" "$R/etc/sshd_config.d" -type f ! -name ruumtunnel ! -name 10-ruumtunnel.conf)"
  [[ -z "$left" ]] && ok "no temporary files are left" || bad "temporary files are left: $left"
else
  bad "the rollback test could not patch the paths of buildbox-tunnel-user.sh"
fi

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
step_names="$(sed -n 's/^CLUSTER_SECRET_NAMES="\(.*\)"$/\1/p' "$STG/steps.sh" | tr ' ' '\n' | sort)"
if [[ "$(sort <<<"$list")" == "$step_names" ]]; then ok "secrets-push.sh and steps.sh list the same cluster names"; else bad "secrets-push.sh and steps.sh lists differ"; fi
if grep -rqE 'STRIPE_|HCLOUD_TOKEN' "$STG/k8s/apply.sh"; then bad "k8s/apply.sh names HCLOUD_TOKEN or a Stripe key"; else ok "k8s/apply.sh never puts HCLOUD_TOKEN or a Stripe key into a Secret"; fi

echo "== Zitadel SMTP (backlog O-20)"
ZV="$STG/k8s/values/zitadel.yaml"
ZB="$STG/ops/zitadel-bootstrap.sh"
# The SMTPConfiguration block of the values (until the next key at its indent or less).
zsmtp="$(awk '/^      SMTPConfiguration:/ {on=1; next} on && /^ {0,6}[^ #]/ {on=0} on' "$ZV")"
zval() { sed -n "s/^ *$1: *//p" <<<"$zsmtp" | head -n 1; }
zb() { sed -n "s/^$1=//p" "$ZB" | tr -d "'\""; }
[[ "$(zval Host)" == smtp.tem.scaleway.com:587 ]] && ok "Zitadel SMTP host is Scaleway Transactional Email on 587" || bad "Zitadel SMTP host is '$(zval Host)'"
[[ "$(zval TLS)" == true ]] && ok "Zitadel SMTP TLS is on (STARTTLS on 587)" || bad "Zitadel SMTP TLS is '$(zval TLS)'"
[[ "$(zval From)" == *@notify.staging.ruum.space ]] && ok "the sender is in the verified domain notify.staging.ruum.space" || bad "the sender '$(zval From)' is not in notify.staging.ruum.space"
grep -qiE 'password|user' <<<"$zsmtp" && bad "the values hold an SMTP user or password" || ok "no SMTP user or password literal in the values"
grep -v "^ *#" "$ZV" | grep -qi mailpit && bad "Zitadel still names Mailpit" || ok "Zitadel does not use Mailpit"
for k in user password; do
  n="ZITADEL_DEFAULTINSTANCE_SMTPCONFIGURATION_SMTP_$(tr '[:lower:]' '[:upper:]' <<<"$k")"
  grep -A4 -E "name: $n\$" "$ZV" | tr -d ' \n' | grep -qF "secretKeyRef:name:zitadel-smtpkey:$k" &&
    ok "the values take $n from the Secret zitadel-smtp key $k" || bad "the values do not take $n from zitadel-smtp $k"
done
[[ "$(zb SMTP_HOST)" == "$(zval Host)" && "$(zb SMTP_FROM)" == "$(zval From)" && "$(zb SMTP_FROM_NAME)" == "$(zval FromName)" ]] &&
  ok "zitadel-bootstrap.sh sets the same host, sender and name as the values" || bad "zitadel-bootstrap.sh and the values differ in the SMTP host or sender"
grep -qF '/admin/v1/email/$id/_activate' "$ZB" && grep -qF 'smtp_check || drift=1' "$ZB" &&
  ok "zitadel-bootstrap.sh activates the provider and check reports its drift" || bad "zitadel-bootstrap.sh does not activate or check the SMTP provider"
grep -qxF "printf 'user=%s\\npassword=%s\\n' \"\$SCW_DEFAULT_PROJECT_ID\" \"\$SCW_SMTP_SECRET_KEY\" | secret auth zitadel-smtp" "$STG/k8s/apply.sh" &&
  ok "k8s/apply.sh writes auth/zitadel-smtp from the project ID and SCW_SMTP_SECRET_KEY" || bad "k8s/apply.sh does not write auth/zitadel-smtp"
[[ "$(grep -c 'zitadel-smtp' "$STG/k8s/apply.sh")" == 1 ]] && ok "zitadel-smtp is written in one namespace only" || bad "zitadel-smtp is written more than once"
grep -q SCW_SMTP "$STG/secrets-init.sh" && bad "secrets-init.sh names the SMTP key (it comes from the person)" || ok "secrets-init.sh never generates the SMTP key"
grep -qx SCW_SMTP_SECRET_KEY <<<"$list" && ! grep -qx SCW_SMTP_ACCESS_KEY <<<"$list" &&
  ok "secrets-push.sh sends SCW_SMTP_SECRET_KEY (not the access key, which SMTP does not need)" || bad "secrets-push.sh SMTP names"
grep -qF 'missing_smtp="$(stg_missing SCW_SMTP_SECRET_KEY SCW_DEFAULT_PROJECT_ID)"' "$STG/up.sh" && grep -qF -- '-n "$missing_smtp"' "$STG/up.sh" &&
  ok "up.sh preflight stops when the SMTP key is missing" || bad "up.sh preflight does not check the SMTP key"

echo "== pins (review 2 U-5)"
# shellcheck source=../versions.env
. "$STG/versions.env"
grep -q "ghcr.io/zitadel/zitadel:$ZITADEL_VERSION" "$ROOT/deploy/dev/docker-compose.yml" && ok "Zitadel $ZITADEL_VERSION on the laptop and staging" || bad "deploy/dev uses another Zitadel than $ZITADEL_VERSION"
grep -q "authzed/spicedb:$SPICEDB_VERSION" "$ROOT/deploy/dev/docker-compose.yml" && ok "SpiceDB $SPICEDB_VERSION on the laptop and staging" || bad "deploy/dev uses another SpiceDB than $SPICEDB_VERSION"

echo "== up.sh guard"
expect 1 "up.sh --apply refuses without RUUM_STAGING_APPLY=yes" env -u RUUM_STAGING_APPLY bash "$STG/up.sh" --apply
expect 1 "up.sh refuses a malformed --restore-from" bash "$STG/up.sh" --restore-from 'x;id'

# helm and tofu: from PATH (CI), else from the tools image (the Mac).
have_docker=0
command -v docker >/dev/null && docker info >/dev/null 2>&1 && have_docker=1
if { ! command -v helm >/dev/null || ! command -v tofu >/dev/null; } && [[ $have_docker == 1 ]]; then
  if TOOLS_IMG="$(bash "$STG/tools-image.sh" ensure 2>"$TMP/tools-image.log")"; then
    intools() { docker run --rm --user "$(id -u):$(id -g)" --cap-drop ALL --tmpfs /tmp:rw,exec -e HOME=/tmp -v "$ROOT:$ROOT:ro" -w "$ROOT" "$TOOLS_IMG" "$@"; }
    command -v helm >/dev/null || helm() { intools helm "$@"; }
    command -v tofu >/dev/null || tofu() { intools tofu "$@"; }
    echo "== helm and tofu from the tools image $TOOLS_IMG"
  else
    cat "$TMP/tools-image.log"
  fi
fi
has() { command -v "$1" >/dev/null || declare -F "$1" >/dev/null; }

if has helm; then
  echo "== charts (helm)"
  d="ghcr.io/nemecec/ruum-server@sha256:$(printf '%064d' 0)"
  e="ghcr.io/nemecec/ruum-edge@sha256:$(printf '%064d' 1)"
  expect 0 "helm lint charts/ruum" helm lint "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e"
  expect 0 "helm lint charts/platform" helm lint "$STG/charts/platform" --set generation=202611111200
  expect 0 "render charts/ruum by digest" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e"
  cp "$TMP/out" "$TMP/ruum.yaml"
  # The server and the chart agree on the settings (tests/server-env.sh, crates/ruum-server/env-names.txt).
  se="$HERE/server-env.sh"
  expect 0 "charts/ruum and ruum-server-env set exactly settings the server reads, and all it needs" bash "$se" "$TMP/ruum.yaml"
  sed 's/name: RUUM_SPICEDB_HTTP_URL,/name: RUUM_SPICEDB_ENDPOINT,/' "$TMP/ruum.yaml" >"$TMP/ruum-drift.yaml"
  expect 1 "server-env.sh refuses a renamed setting (RUUM_SPICEDB_ENDPOINT, the first deploy's crash)" bash "$se" "$TMP/ruum-drift.yaml"
  sed 's/name: RUUM_SPICEDB_HTTP_URL,/name: RUUM_SUITE_RLS,/' "$TMP/ruum.yaml" >"$TMP/ruum-drift.yaml"
  expect 1 "server-env.sh refuses a test-only setting in the chart" bash "$se" "$TMP/ruum-drift.yaml"
  sed 's/RUUM_SPICEDB_PRESHARED_KEY=/RUUM_SPICEDB_KEY=/' "$STG/k8s/apply.sh" >"$TMP/apply-drift.sh"
  expect 1 "server-env.sh refuses an unknown key in ruum-server-env" bash "$se" "$TMP/ruum.yaml" "$TMP/apply-drift.sh"
  # The sign-in hint: the IdP IDs come from ruum/ruum-oidc, required (never optional), and the
  # bootstrap writes exactly those keys.
  for k in google microsoft; do
    n="RUUM_OIDC_IDP_$(tr '[:lower:]' '[:upper:]' <<<"$k")"
    if grep -A1 -E "name: $n\$" "$TMP/ruum.yaml" | grep -qE "secretKeyRef: \{ name: ruum-oidc, key: idp-$k \}"; then
      ok "charts/ruum takes $n from ruum-oidc idp-$k"
    else bad "charts/ruum does not take $n from ruum-oidc idp-$k"; fi
    grep -qF "idp-$k=%s" "$STG/ops/zitadel-bootstrap.sh" &&
      ok "zitadel-bootstrap.sh writes idp-$k to ruum-oidc" || bad "zitadel-bootstrap.sh does not write idp-$k to ruum-oidc"
  done
  grep -E 'ruum-oidc' "$TMP/ruum.yaml" | grep -q 'optional' && bad "charts/ruum reads ruum-oidc as optional" || ok "charts/ruum requires every key of ruum-oidc"
  mkdir -p "$TMP/crates/ruum-core/src"
  { grep -vE '^[[:space:]]*(#|$)' "$ROOT/crates/ruum-server/env-names.txt" | cut -d' ' -f1; echo RUUM_NEW_SETTING; } |
    sed 's/.*/"&"/' >"$TMP/crates/ruum-core/src/lib.rs"
  expect 1 "server-env.sh refuses a setting the code reads but env-names.txt does not list" bash "$se" "$TMP/ruum.yaml" "$STG/k8s/apply.sh" "$TMP/crates"
  [[ "$(cat "$TMP/out")" == "the server reads RUUM_NEW_SETTING, but env-names.txt does not list it" ]] || bad "server-env.sh: the only problem is the new setting"
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
  # Updates without downtime (design 12.7, backlog O-25 and O-26).
  echo "== updates without downtime (O-25, O-26)"
  has_line() { grep -qF -- "$2" "$1"; }
  want() { # want <file> <description> <fixed string>
    if has_line "$1" "$3"; then ok "$2"; else bad "$2 (no '$3')"; fi
  }
  expect 0 "render the ruum-server templates" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --show-only templates/server.yaml
  cp "$TMP/out" "$TMP/server.yaml"
  want "$TMP/server.yaml" "ruum-server runs two replicas" "replicas: 2"
  want "$TMP/server.yaml" "ruum-server uses a rolling update" "type: RollingUpdate"
  want "$TMP/server.yaml" "the rolling update never goes below the replicas (maxUnavailable 0)" "maxUnavailable: 0"
  has_line "$TMP/server.yaml" "Recreate" && bad "ruum-server still uses Recreate" || ok "no Recreate strategy for ruum-server"
  want "$TMP/server.yaml" "the serving pods do not migrate (RUUM_MIGRATE=false)" '{ name: RUUM_MIGRATE, value: "false" }'
  want "$TMP/server.yaml" "the server drains for RUUM_SHUTDOWN_GRACE_SECONDS" '{ name: RUUM_SHUTDOWN_GRACE_SECONDS, value: "35" }'
  want "$TMP/server.yaml" "a preStop pause takes the pod out of the Service before SIGTERM" "sleep: { seconds: 10 }"
  want "$TMP/server.yaml" "ruum-server has a disruption budget" "kind: PodDisruptionBudget"
  want "$TMP/server.yaml" "the disruption budget keeps one server" "minAvailable: 1"
  want "$TMP/server.yaml" "ruum-server pods spread over the nodes" "topologyKey: kubernetes.io/hostname"
  tg="$(awk '/terminationGracePeriodSeconds:/ {print $2; exit}' "$TMP/server.yaml")"
  [[ "$tg" =~ ^[0-9]+$ && "$tg" -gt 45 ]] && ok "terminationGracePeriodSeconds ($tg) is longer than the pause plus the drain (45)" ||
    bad "terminationGracePeriodSeconds '${tg:-none}' is not longer than the pause plus the drain"
  expect 1 "charts/ruum refuses one server replica" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set server.replicas=1
  grep -q "kind: Job" "$TMP/ruum.yaml" && bad "the release renders the migration Job" || ok "the release itself has no migration Job (deploy-chart.sh runs it first)"
  J=(--set "images.server=$d" --set "images.edge=$e" --set-string commit=0123456789abcdef0123456789abcdef01234567 --set migrationJob=true)
  expect 0 "render the migration Job" helm template ruum "$STG/charts/ruum" "${J[@]}" --set-string migrationJobName=ruum-migrate-0123456789ab-202611121200 --show-only templates/migrate-job.yaml
  cp "$TMP/out" "$TMP/job.yaml"
  want "$TMP/job.yaml" "the migration step is a Job" "kind: Job"
  want "$TMP/job.yaml" "the Job runs ruum-server migrate" "args: [migrate]"
  want "$TMP/job.yaml" "the Job runs the new server image by digest" "image: $d"
  want "$TMP/job.yaml" "a failed migration is not retried (backoffLimit 0)" "backoffLimit: 0"
  want "$TMP/job.yaml" "the Job pods carry app: ruum-migrate" "labels: { app: ruum-migrate }"
  has_line "$TMP/job.yaml" "app: ruum-server" && bad "the Job pods carry app: ruum-server (the Service would send them traffic)" || ok "the Job pods are not behind the ruum-server Service"
  # Kimi review of O-25, findings 1 and 5: the owner's DSN only in the Job, from a Secret of its own.
  want "$TMP/job.yaml" "the Job reads the owner's DSN from ruum-migrate-db" "secretKeyRef: { name: ruum-migrate-db, key: DATABASE_URL }"
  has_line "$TMP/job.yaml" "ruum-server-env" && bad "the Job reads ruum-server-env" || ok "the Job does not read ruum-server-env"
  has_line "$TMP/job.yaml" "envFrom" && bad "the Job takes every key of a Secret" || ok "the Job takes no other secret"
  [[ "$(grep -c 'secretKeyRef' "$TMP/job.yaml")" == 1 ]] && ok "the Job reads exactly one secret value" || bad "the Job reads more than one secret value"
  want "$TMP/job.yaml" "the Job restricts the runtime role ruum_app after the migrations" '{ name: RUUM_DB_RUNTIME_ROLE, value: "ruum_app" }'
  expect 1 "charts/ruum refuses a runtime role that is not a plain name" helm template ruum "$STG/charts/ruum" "${J[@]}" --set-string migrationJobName=ruum-migrate-0123456789ab-202611121200 --set-string 'database.runtimeRole=x"; drop' --show-only templates/migrate-job.yaml
  has_line "$TMP/ruum.yaml" "ruum-migrate-db" && bad "the release (serving pods) mounts ruum-migrate-db" || ok "the serving pods never mount the owner's DSN (ruum-migrate-db)"
  # The rollback rule (finding 2): ruum-release records the previous release.
  awk '/^  name: ruum-release$/ {on=1} on && /^---/ {exit} on' "$TMP/ruum.yaml" | grep -qE '^  previous: ""$' &&
    ok "ruum-release records the previous release (empty by default)" || bad "ruum-release has no previous key"
  expect 0 "render ruum-release with a previous commit" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set-string previousCommit=0123456789abcdef0123456789abcdef01234567 --show-only templates/server.yaml
  has_line "$TMP/out" 'previous: "0123456789abcdef0123456789abcdef01234567"' && ok "ruum-release keeps the previous commit" || bad "ruum-release lost the previous commit"
  expect 1 "charts/ruum refuses a previous commit that is not 40 hex" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set-string 'previousCommit=abc,x' --show-only templates/server.yaml
  expect 1 "charts/ruum refuses a migration Job name outside the pattern" helm template ruum "$STG/charts/ruum" "${J[@]}" --set-string 'migrationJobName=x;y' --show-only templates/migrate-job.yaml
  expect 1 "charts/ruum refuses a migration Job without a name" helm template ruum "$STG/charts/ruum" "${J[@]}" --show-only templates/migrate-job.yaml
  expect 0 "render the edge templates" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --show-only templates/edge.yaml
  cp "$TMP/out" "$TMP/edge.yaml"
  want "$TMP/edge.yaml" "the edge is a DaemonSet (an edge on every node behind the load balancer)" "kind: DaemonSet"
  has_line "$TMP/edge.yaml" "kind: Deployment" && bad "the edge is still a Deployment" || ok "no edge Deployment"
  want "$TMP/edge.yaml" "an edge update starts the new pod first (maxSurge 1)" "maxSurge: 1"
  want "$TMP/edge.yaml" "an edge update never stops a node's edge first (maxUnavailable 0)" "maxUnavailable: 0"
  want "$TMP/edge.yaml" "the edge runs as the ServiceAccount ruum-edge" "serviceAccountName: ruum-edge"
  want "$TMP/edge.yaml" "the edge has a preStop pause" "sleep: { seconds: 5 }"
  want "$TMP/edge.yaml" "certificates are in Kubernetes Secrets (storage kubernetes_secret)" "storage kubernetes_secret {"
  want "$TMP/edge.yaml" "the certificate storage is the edge namespace" "namespace edge"
  want "$TMP/edge.yaml" "Caddy drains open connections for 30 s" "grace_period 30s"
  has_line "$TMP/edge.yaml" "file_system" && bad "the edge still keeps certificates in a file system" || ok "no file system storage for certificates"
  expect 0 "render the platform for the edge and deployer rights" helm template platform "$STG/charts/platform" --set generation=202611111200
  cp "$TMP/out" "$TMP/platform.yaml"
  awk '/^# Source: [a-z-]*\/templates\/edge-rbac.yaml/ {on=1; next} /^# Source:/ {on=0} on' "$TMP/platform.yaml" >"$TMP/edge-rbac.yaml"
  want "$TMP/edge-rbac.yaml" "the platform makes the ServiceAccount ruum-edge" "name: ruum-edge"
  if grep -qE 'kind: Cluster(Role|RoleBinding)' "$TMP/edge-rbac.yaml"; then bad "the edge gets cluster-wide rights"; else ok "the edge's rights are namespaced"; fi
  [[ "$(grep -cE '^  namespace: edge$' "$TMP/edge-rbac.yaml")" == 3 ]] && ok "the edge's ServiceAccount, Role and RoleBinding are in edge" || bad "edge-rbac.yaml objects outside the edge namespace"
  [[ "$(grep -E 'resources: \[' "$TMP/edge-rbac.yaml" | tr -d ' ' | sort | tr '\n' ' ')" == "resources:[leases] resources:[secrets] " ]] &&
    ok "the edge may touch only Secrets and Leases" || bad "the edge Role names other resources: $(grep -E 'resources:' "$TMP/edge-rbac.yaml" | tr '\n' ' ')"
  awk '/^  name: pg-clients$/ {on=1} on && /^---/ {exit} on' "$TMP/platform.yaml" | grep -qF "matchLabels: { app: ruum-migrate }" &&
    ok "the network policy lets the migration Job reach Postgres" || bad "the migration Job cannot reach Postgres (pg-clients)"
  awk '/^# Source: [a-z-]*\/templates\/deployer-rbac.yaml/ {on=1; next} /^# Source:/ {on=0} on' "$TMP/platform.yaml" |
    awk 'BEGIN{RS="---"} /kind: Role\n/ && /name: deployer\n/ && /namespace: ruum\n/' | grep -qF "resources: [jobs]" &&
    ok "the deployer may run Jobs in ruum" || bad "the deployer cannot run the migration Job in ruum"
  awk '/^# Source: [a-z-]*\/templates\/deployer-rbac.yaml/ {on=1; next} /^# Source:/ {on=0} on' "$TMP/platform.yaml" |
    awk 'BEGIN{RS="---"} /kind: Role\n/ && /name: deployer\n/ && /namespace: edge\n/' | grep -qF "jobs" &&
    bad "the deployer may run Jobs in edge" || ok "the deployer runs Jobs only in ruum"
  awk '/^# Source: [a-z-]*\/templates\/cnpg.yaml/ {on=1; next} /^# Source:/ {on=0} on' "$TMP/platform.yaml" >"$TMP/cnpg.yaml"
  grep -A4 -E '^ +- name: ruum_app$' "$TMP/cnpg.yaml" | grep -qF 'passwordSecret: { name: pg-role-ruum-app }' &&
    ok "CNPG manages the runtime role ruum_app" || bad "CNPG has no managed role ruum_app"
  grep -A4 -E '^ +- name: ruum_app$' "$TMP/cnpg.yaml" | grep -qE 'inRoles|superuser: true|createdb: true|createrole: true' &&
    bad "ruum_app gets a membership or a special right" || ok "ruum_app has no membership and no special right"
  awk '/^# Source: [a-z-]*\/templates\/deployer-rbac.yaml/ {on=1; next} /^# Source:/ {on=0} on' "$TMP/platform.yaml" |
    awk 'BEGIN{RS="---"} /kind: Role\n/ && /name: deployer\n/ && /namespace: ruum\n/' >"$TMP/deployer-ruum.yaml"
  grep -B1 -A2 -F 'resourceNames: [ruum-deploy]' "$TMP/deployer-ruum.yaml" | grep -qF 'verbs: [get, update]' &&
    ok "the deployer may read and update only the Lease ruum-deploy (the deploy lock)" || bad "the deployer has no rights on the deploy lock"
  grep -qE 'resources: \[secrets\]' "$TMP/deployer-ruum.yaml" && bad "the deployer has a rule on Secrets" || ok "the deployer has no rule on Secrets"
  jl="$(grep -nF -- '--show-only templates/migrate-job.yaml' "$dc" | head -1 | cut -d: -f1)"
  hl="$(grep -nE '^helm upgrade' "$dc" | head -1 | cut -d: -f1)"
  [[ -n "$jl" && -n "$hl" && "$jl" -lt "$hl" ]] && ok "deploy-chart.sh runs the migration Job before the rollout" || bad "deploy-chart.sh: the migration Job is not before helm upgrade"
  grep -qF 'not rolling out, the old release keeps serving' "$dc" && ok "deploy-chart.sh stops when the migration step fails" || bad "deploy-chart.sh rolls out after a failed migration step"
  # The ask limit is per process: each replica gets its share (review of O-25, change 4).
  want "$TMP/server.yaml" "each of two replicas gets half of the ask limit (20 / 2)" '{ name: RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR, value: "10" }'
  expect 0 "render ruum-server with three replicas and an ask limit of 2" helm template ruum "$STG/charts/ruum" --set "images.server=$d" --set "images.edge=$e" --set server.replicas=3 --set server.tlsAskNewHostsPerHour=2 --show-only templates/server.yaml
  has_line "$TMP/out" '{ name: RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR, value: "1" }' && ok "the ask limit per replica is at least 1" || bad "the ask limit per replica fell below 1"
  # The Zitadel chart from its repository (network; skipped when it cannot be fetched).
  if helm template zitadel zitadel --repo https://charts.zitadel.com --version "$ZITADEL_CHART_VERSION" \
    --namespace auth -f "$ZV" >"$TMP/zitadel.yaml" 2>"$TMP/zitadel.err"; then
    n="$(grep -cE 'name: ZITADEL_DEFAULTINSTANCE_SMTPCONFIGURATION_SMTP_(USER|PASSWORD)$' "$TMP/zitadel.yaml")"
    [[ "$n" -ge 4 ]] && ok "the rendered Zitadel chart passes the SMTP user and password to the setup job and the deployment ($n)" ||
      bad "the rendered Zitadel chart has $n SMTP env entries (want the setup job and the deployment)"
    grep -A5 -E 'name: ZITADEL_DEFAULTINSTANCE_SMTPCONFIGURATION_SMTP_PASSWORD$' "$TMP/zitadel.yaml" | grep -q 'name: zitadel-smtp' &&
      ok "the rendered password comes from the Secret zitadel-smtp" || bad "the rendered password is not from zitadel-smtp"
    grep -qE 'Host: smtp\.tem\.scaleway\.com:587' "$TMP/zitadel.yaml" && grep -qE 'From: no-reply@notify\.staging\.ruum\.space' "$TMP/zitadel.yaml" &&
      ok "the rendered Zitadel config has the Scaleway host and the sender" || bad "the rendered Zitadel config lacks the Scaleway host or the sender"
    grep -qiE 'mailpit' "$TMP/zitadel.yaml" && bad "the rendered Zitadel chart names Mailpit" || ok "the rendered Zitadel chart does not name Mailpit"
  else
    echo "== Zitadel chart render: skipped (charts.zitadel.com not reachable: $(head -c 200 "$TMP/zitadel.err"))"
  fi
  if [[ $have_docker == 1 ]]; then
    echo "== Caddyfile (caddy adapt --validate)"
    awk '/^  Caddyfile: \|/ {on=1; next} on && /^[^ ]/ {on=0} on && /^---/ {on=0} on {sub(/^    /, ""); print}' "$TMP/ruum.yaml" >"$TMP/Caddyfile"
    # The stock image has no kubernetes_secret storage module (the edge image builds it in and
    # adapts it there, deploy/images/ruum-edge.Dockerfile); drop that block here.
    awk '/^\tstorage kubernetes_secret \{/ {skip=1; next} skip && /^\t\}/ {skip=0; next} !skip' "$TMP/Caddyfile" >"$TMP/Caddyfile.stock"
    mv "$TMP/Caddyfile.stock" "$TMP/Caddyfile"
    grep -q kubernetes_secret "$TMP/Caddyfile" && bad "the storage block is still in the Caddyfile for the stock image"
    expect 0 "the rendered Caddyfile adapts" docker run --rm -v "$TMP/Caddyfile:/etc/caddy/Caddyfile:ro" "caddy:$CADDY_VERSION-alpine" caddy adapt --config /etc/caddy/Caddyfile --validate
    # Every server, port 80 (8080) included, reads the load balancer's PROXY header: a server
    # that automatic HTTPS adds at runtime would not, and port 80 would answer 400.
    docker run --rm -v "$TMP/Caddyfile:/etc/caddy/Caddyfile:ro" "caddy:$CADDY_VERSION-alpine" caddy adapt --config /etc/caddy/Caddyfile 2>/dev/null >"$TMP/caddy.json"
    jq -e '[.apps.http.servers[]] as $s | ($s | map(.listen[]) | index(":8080")) != null and ($s | all((.listener_wrappers // []) | map(.wrapper) | index("proxy_protocol") != null))' "$TMP/caddy.json" >/dev/null &&
      ok "port 80 is an explicit server, and every server has the proxy_protocol wrapper" ||
      bad "a Caddy server without the proxy_protocol wrapper, or no explicit port 80 server"
  fi
else
  echo "== charts: skipped (no helm; setup.sh installs it, or Docker for the tools image)"
fi

if has tofu; then
  echo "== OpenTofu"
  for r in cluster persistent; do
    expect 0 "tofu fmt $r" tofu -chdir="$STG/tofu/$r" fmt -check -recursive
    # The same layout as the checkout (the patches are read from ../../talos), in a temp folder
    # of wherever tofu runs.
    v="t=\$(mktemp -d) && mkdir -p \$t/tofu && cp -R '$STG/tofu/$r' \$t/tofu/$r && cp -R '$STG/talos' \$t/talos"
    v="$v && cd \$t/tofu/$r && tofu init -backend=false -input=false >/dev/null && tofu validate -no-color; rc=\$?; rm -rf \$t; exit \$rc"
    if type -P tofu >/dev/null; then
      expect 0 "tofu validate $r" bash -c "$v"
    else
      expect 0 "tofu validate $r (tools image)" intools bash -c "$v"
    fi
  done
  # The rendered machine configs of the Talos provider pass talosctl validate (Talos v1.14 moved
  # settings into their own documents; a setting in both places fails only at apply time).
  if type -P tofu >/dev/null && type -P talosctl >/dev/null; then
    expect 0 "talosctl validate of every rendered node config" bash "$STG/tests/talos-validate.sh"
  elif declare -F intools >/dev/null; then
    expect 0 "talosctl validate of every rendered node config (tools image)" intools bash "$STG/tests/talos-validate.sh"
  else
    echo "== talosctl validate: skipped (no talosctl; setup.sh installs it)"
  fi
else
  echo "== OpenTofu: skipped (no tofu; setup.sh installs it, or Docker for the tools image)"
fi

# The kubeconfig rewrite of the admin tunnel, with the kubectl of the tools image (or PATH).
KRW="cat >/tmp/k <<'KC'
apiVersion: v1
kind: Config
clusters:
- name: ruum-staging
  cluster:
    server: https://10.20.1.11:6443
    certificate-authority-data: Y2E=
contexts:
- name: admin@ruum-staging
  context: {cluster: ruum-staging, user: admin@ruum-staging}
current-context: admin@ruum-staging
users:
- name: admin@ruum-staging
  user: {}
KC
. '$STG/tunnel.sh' && tunnel_kube_rewrite /tmp/k && kubectl --kubeconfig /tmp/k config view --raw -o jsonpath='{.clusters[0].cluster.server} {.clusters[0].cluster.tls-server-name} {.clusters[0].cluster.certificate-authority-data}'"
if declare -F intools >/dev/null; then
  echo "== kubeconfig through the tunnel (kubectl of the tools image)"
  expect 0 "tunnel_kube_rewrite runs" intools bash -c "$KRW"
elif command -v kubectl >/dev/null; then
  echo "== kubeconfig through the tunnel (kubectl)"
  expect 0 "tunnel_kube_rewrite runs" bash -c "${KRW//\/tmp\/k/$TMP/k}"
fi
if [[ -s "$TMP/out" ]] && grep -q 16443 "$TMP/out"; then
  [[ "$(cat "$TMP/out")" == "https://127.0.0.1:16443 kubernetes Y2E=" ]] &&
    ok "kubeconfig: server https://127.0.0.1:16443, tls-server-name kubernetes, the CA kept" || { bad "kubeconfig rewrite"; cat "$TMP/out"; }
fi

# tunnel-relay.sh (Kimi review F6): the command runs only when every relay listens.
TR="cd '$STG' && export STG_TUNNEL_PORTS='15001 15002'"
TR_GOOD="$TR && bash tunnel-relay.sh echo RAN"
TR_EXIT="$TR && mkdir -p /tmp/fs && printf '#!/bin/sh\nexit 1\n' >/tmp/fs/socat && chmod +x /tmp/fs/socat && PATH=/tmp/fs:\$PATH bash tunnel-relay.sh echo RAN"
TR_TAKEN="$TR && (socat TCP-LISTEN:15002,bind=127.0.0.1 /dev/null &) && sleep 0.5 && bash tunnel-relay.sh echo RAN"
relay() { if [[ -r /proc/net/tcp ]] && command -v socat >/dev/null; then bash -c "$1"; elif declare -F intools >/dev/null; then intools bash -c "$1"; else return 99; fi; }
relay "$TR_GOOD" >"$TMP/out" 2>&1
case $? in
  99) echo "== tunnel-relay.sh: skipped (no socat and /proc here, no tools image)" ;;
  0)
    grep -qx RAN "$TMP/out" && ok "tunnel-relay.sh runs the command when every relay listens" || bad "tunnel-relay.sh good path"
    relay "$TR_EXIT" >"$TMP/out" 2>&1 && bad "tunnel-relay.sh exits 0 when a relay fails" || { grep -q RAN "$TMP/out" && bad "tunnel-relay.sh runs the command after a relay failed" || ok "tunnel-relay.sh exits non-zero, without the command, when a relay exits"; }
    relay "$TR_TAKEN" >"$TMP/out" 2>&1 && bad "tunnel-relay.sh exits 0 when a port is taken" || { grep -q RAN "$TMP/out" && bad "tunnel-relay.sh runs the command when a port is taken" || ok "tunnel-relay.sh exits non-zero, without the command, when a port is taken"; }
    ;;
  *) bad "tunnel-relay.sh good path"; cat "$TMP/out" ;;
esac

# The rollback floor of deploy.sh (review of O-25, change 1): a commit without the migration step
# or the edge storage module is refused.
echo "== deploy.sh release floor (O-25)"
if command -v git >/dev/null; then
  RF="$TMP/floor"
  mkdir -p "$RF" && git -C "$RF" init -q && git -C "$RF" config user.email t@example.invalid && git -C "$RF" config user.name t
  mkdir -p "$RF/crates/ruum-server/src" "$RF/deploy/images/caddy-secret-storage"
  echo old >"$RF/crates/ruum-server/src/main.rs" && git -C "$RF" add -A && git -C "$RF" commit -qm old
  old_sha="$(git -C "$RF" rev-parse HEAD)"
  echo m >"$RF/crates/ruum-server/src/migrate.rs" && git -C "$RF" add -A && git -C "$RF" commit -qm half
  half_sha="$(git -C "$RF" rev-parse HEAD)"
  echo s >"$RF/deploy/images/caddy-secret-storage/storage.go" && git -C "$RF" add -A && git -C "$RF" commit -qm o25
  new_sha="$(git -C "$RF" rev-parse HEAD)"
  floor() { bash -c ". '$STG/ops/release-floor.sh' && ruum_release_floor_ok \"\$1\" \"\$2\"" _ "$@"; }
  expect 0 "the release floor accepts a commit with the migration step and the edge storage" floor "$RF" "$new_sha"
  expect 1 "the release floor refuses a commit before O-25" floor "$RF" "$old_sha"
  expect 1 "the release floor refuses a commit without the edge storage module" floor "$RF" "$half_sha"
  if git -C "$ROOT" rev-parse -q --verify HEAD >/dev/null 2>&1; then
    expect 0 "the release floor accepts this checkout's HEAD" floor "$ROOT" HEAD
  fi
  grep -qF 'ruum_release_floor_ok "$STG_ROOT" "$sha"' "$STG/deploy.sh" && ok "deploy.sh checks the release floor" || bad "deploy.sh does not check the release floor"
fi

# The rollback rule (Kimi review of O-25, finding 2): a plain --rollback goes only to the previous
# release; an older one needs --rollback-older.
echo "== deploy.sh rollback rule (Kimi review of O-25)"
rb() { bash -c ". '$STG/ops/release-floor.sh' && ruum_rollback_target_ok \"\$1\" \"\$2\" \"\$3\"" _ "$@"; }
P1=1111111111111111111111111111111111111111
P2=2222222222222222222222222222222222222222
expect 0 "a rollback to the previous release passes" rb "$P1" "$P1" 0
expect 1 "a rollback two releases back is refused without --rollback-older" rb "$P2" "$P1" 0
expect 1 "a rollback is refused when no previous release is recorded" rb "$P1" "" 0
expect 0 "--rollback-older allows an older release" rb "$P2" "$P1" 1
grep -qF 'RUUM_ROLLBACK_OLDER_WARNING' "$STG/deploy.sh" && grep -qF 'RUUM_ROLLBACK_OLDER_WARNING' "$STG/ops/deploy-chart.sh" &&
  ok "deploy.sh and deploy-chart.sh print the warning for --rollback-older" || bad "no warning for --rollback-older"
grep -qF 'ruum_rollback_target_ok "$commit" "$previous"' "$STG/ops/deploy-chart.sh" &&
  ok "deploy-chart.sh checks the rollback rule against ruum-release, under the lock" || bad "deploy-chart.sh does not check the rollback rule"
grep -qF '1) expect+=(--rollback)' "$STG/deploy.sh" &&
  ok "deploy.sh passes --rollback to deploy-chart.sh" || bad "deploy.sh does not pass --rollback on"
grep -qF '"${expect[@]}"' "$STG/deploy.sh" && ok "deploy.sh passes the live release it checked (--expect-deployed)" || bad "deploy.sh does not pass --expect-deployed"

# The deploy lock (Kimi review of O-25, finding 3), with a fake kubectl that keeps the Lease in a
# file and refuses a create of an existing Lease and a replace with an old resourceVersion, as
# the API server does.
echo "== deploy lock (Lease ruum/ruum-deploy)"
FK="$TMP/fakekube"
mkdir -p "$FK/bin"
cat >"$FK/bin/kubectl" <<'FAKE'
#!/usr/bin/env bash
f="$FAKE_LEASE"
case "$*" in
  "-n ruum create -f -")
    in="$(cat)"
    [[ -s "$f" ]] && { echo 'Error from server (AlreadyExists): leases.coordination.k8s.io "ruum-deploy" already exists' >&2; exit 1; }
    jq '.metadata.resourceVersion = "1"' <<<"$in" >"$f" ;;
  "-n ruum get lease ruum-deploy -o json")
    [[ -s "$f" ]] || { echo 'Error from server (NotFound)' >&2; exit 1; }
    cat "$f" ;;
  "-n ruum replace -f -")
    in="$(cat)"
    [[ -s "$f" ]] || { echo 'Error from server (NotFound)' >&2; exit 1; }
    [[ "$(jq -r .metadata.resourceVersion <<<"$in")" == "$(jq -r .metadata.resourceVersion "$f")" ]] ||
      { echo 'Error from server (Conflict): the object has been modified' >&2; exit 1; }
    jq --arg rv "$(( $(jq -r .metadata.resourceVersion "$f") + 1 ))" '.metadata.resourceVersion = $rv' <<<"$in" >"$f" ;;
  *) echo "fake kubectl: unexpected $*" >&2; exit 3 ;;
esac
FAKE
chmod +x "$FK/bin/kubectl"
lk() { PATH="$FK/bin:$PATH" FAKE_LEASE="$FK/lease.json" bash -c ". '$STG/ops/deploy-lock.sh' && \"\$@\"" _ "$@"; }
expect 0 "deploy A takes the free lock" lk ruum_lock_acquire deploy-a
expect 1 "deploy B is refused while A holds the lock" lk ruum_lock_acquire deploy-b
grep -q 'deploy-a' "$TMP/out" && ok "the refusal names the holder" || bad "the refusal does not name the holder"
expect 0 "A renews its lock between the steps" lk ruum_lock_renew deploy-a
expect 1 "B cannot renew a lock that A holds" lk ruum_lock_renew deploy-b
expect 0 "B's release leaves A's lock alone" lk ruum_lock_release deploy-b
[[ "$(jq -r .spec.holderIdentity "$FK/lease.json")" == deploy-a ]] && ok "A still holds the lock after B's release" || bad "B's release removed A's lock"
expect 0 "A releases the lock" lk ruum_lock_release deploy-a
expect 0 "B takes the released lock" lk ruum_lock_acquire deploy-b
expect 1 "A, which lost the lock, cannot renew it (it stops before its next step)" lk ruum_lock_renew deploy-a
# An expired lock (its holder died): taken over with a compare-and-swap.
jq '.spec.renewTime = "2020-01-01T00:00:00.000000Z" | .spec.acquireTime = "2020-01-01T00:00:00.000000Z"' "$FK/lease.json" >"$FK/l" && mv "$FK/l" "$FK/lease.json"
expect 0 "C takes over an expired lock" lk ruum_lock_acquire deploy-c
expect 1 "B, whose lock expired, cannot renew it" lk ruum_lock_renew deploy-b
dcl() { grep -nF -- "$1" "$dc" | head -1 | cut -d: -f1; }
la="$(dcl 'ruum_lock_acquire "$holder"')" lr="$(dcl 'configmap ruum-release -o jsonpath='"'"'{.data.commit}'"'"' 2>/dev/null')"
lb="$(dcl 'if [[ $backup == 1 ]]')" lj="$(dcl 'kubectl -n ruum create -f "$tmp/job.yaml"')" lh="$(grep -nE '^helm upgrade' "$dc" | head -1 | cut -d: -f1)"
[[ -n "$la" && -n "$lr" && -n "$lb" && -n "$lj" && -n "$lh" && "$la" -lt "$lr" && "$lr" -lt "$lb" && "$lb" -lt "$lj" && "$lj" -lt "$lh" ]] &&
  ok "deploy-chart.sh takes the lock before it reads the live release, the backup, the migration step and helm upgrade" ||
  bad "deploy-chart.sh: the lock does not cover read, backup, migration and rollout ($la $lr $lb $lj $lh)"
lw="$(grep -nF 'ruum_lock_renew "$holder" || die "lost the deploy lock after the migration step' "$dc" | cut -d: -f1)"
[[ -n "$lw" && "$lw" -gt "$lj" && "$lw" -lt "$lh" ]] && ok "deploy-chart.sh renews the lock before helm upgrade and stops when it lost it" || bad "deploy-chart.sh does not renew the lock before helm upgrade"
grep -qF 'another deploy ran in between' "$dc" && ok "deploy-chart.sh refuses when the live release changed since deploy.sh checked it" || bad "deploy-chart.sh does not compare the live release"
grep -qE '^trap cleanup EXIT' "$dc" && grep -qF 'ruum_lock_release "$holder"' "$dc" && ok "deploy-chart.sh releases the lock on exit" || bad "deploy-chart.sh does not release the lock"

# The database roles (Kimi review of O-25, findings 1 and 5).
echo "== database roles of ruum (owner for the migration Job, ruum_app for the serving pods)"
ap="$STG/k8s/apply.sh"
blk() { awk -v want="$1" '/^\{$/ { block = "" } { block = block $0 "\n" } $0 ~ want { printf "%s", block; exit }' "$ap"; }
grep -B1 -F 'secret ruum ruum-migrate-db' "$ap" | grep -qF 'postgres://ruum:%s@' && grep -B1 -F 'secret ruum ruum-migrate-db' "$ap" | grep -qF '"$STG_PG_RUUM_PASSWORD"' &&
  ok "ruum/ruum-migrate-db holds the owner's DSN" || bad "ruum/ruum-migrate-db does not hold the owner's DSN"
blk '} \\| secret ruum ruum-server-env' >"$TMP/server-env-block"
grep -qF 'postgres://ruum_app:%s@' "$TMP/server-env-block" && grep -qF 'STG_PG_RUUM_APP_PASSWORD' "$TMP/server-env-block" &&
  ok "ruum/ruum-server-env holds ruum_app's DSN" || bad "ruum/ruum-server-env does not hold ruum_app's DSN"
grep -qF 'STG_PG_RUUM_PASSWORD' "$TMP/server-env-block" && bad "ruum/ruum-server-env holds the owner's password" || ok "ruum/ruum-server-env does not hold the owner's password"
gl="$(grep -nF 'sql/ruum-runtime-grants.sql" >/dev/null' "$ap" | cut -d: -f1)" sl="$(grep -nF '} | secret ruum ruum-server-env' "$ap" | cut -d: -f1)"
[[ -n "$gl" && -n "$sl" && "$gl" -lt "$sl" ]] && ok "apply.sh grants ruum_app its rights before the serving pods get its DSN" || bad "apply.sh writes ruum-server-env before the grants"
grep -qF 'add STG_PG_RUUM_APP_PASSWORD' "$STG/secrets-init.sh" && ok "secrets-init.sh generates STG_PG_RUUM_APP_PASSWORD" || bad "secrets-init.sh does not generate STG_PG_RUUM_APP_PASSWORD"
grep -qF 'STG_PG_RUUM_APP_PASSWORD' <<<"$list" && ok "secrets-push.sh sends STG_PG_RUUM_APP_PASSWORD" || bad "secrets-push.sh does not send STG_PG_RUUM_APP_PASSWORD"
gs="$STG/k8s/sql/ruum-runtime-grants.sql"
grep -vE '^[[:space:]]*--' "$gs" | perl -ne 'print "$1\n" while /\bgrant\s+(.*?)\s+on\s/gi' | grep -iE '\b(create|all|truncate|references|trigger)\b' && bad "the grants give ruum_app DDL-like rights" || ok "the grants give ruum_app data rights only"
if [[ $have_docker == 1 ]]; then
  pgc="ruum-stg-grants-$$"
  if docker run -d --rm --name "$pgc" -e POSTGRES_PASSWORD=probe postgres:17 >/dev/null 2>"$TMP/pg.err"; then
    for _ in $(seq 1 90); do
      [[ "$(docker logs "$pgc" 2>&1 | grep -c 'ready to accept connections')" -ge 2 ]] && break
      sleep 1
    done
    pq() { docker exec -i "$pgc" psql -X -q -tA -v ON_ERROR_STOP=1 -U postgres "$@"; }
    pgrants() { docker exec -i "$pgc" psql -X -q -v ON_ERROR_STOP=1 -U postgres -d ruum -f - <"$gs"; }
    pq -d postgres -c "create role ruum login" -c "create role ruum_app login noinherit" -c "create database ruum owner ruum" >/dev/null
    pq -d ruum -c "set role ruum" -c "create table _sqlx_migrations (version bigint primary key, success boolean not null)" \
      -c "create table nodes (id bigint generated always as identity primary key, name text)" -c "insert into _sqlx_migrations values (1, true)" >/dev/null
    expect 0 "the grants apply" pgrants
    expect 0 "the grants apply again (idempotent)" pgrants
    expect 0 "ruum_app reads and writes the data tables" pq -d ruum -c "set role ruum_app" -c "insert into nodes (name) values ('a')" -c "update nodes set name = 'b'" -c "select count(*) from nodes" -c "delete from nodes"
    expect 0 "ruum_app reads _sqlx_migrations (the start-up check)" pq -d ruum -c "set role ruum_app" -c "select version, success from _sqlx_migrations"
    expect 1 "ruum_app cannot add a migration record" pq -d ruum -c "set role ruum_app" -c "insert into _sqlx_migrations values (2, true)"
    expect 1 "ruum_app cannot change a migration record" pq -d ruum -c "set role ruum_app" -c "update _sqlx_migrations set success = false"
    expect 1 "ruum_app cannot delete migration records" pq -d ruum -c "set role ruum_app" -c "delete from _sqlx_migrations"
    expect 1 "ruum_app cannot create a table" pq -d ruum -c "set role ruum_app" -c "create table x (id int)"
    expect 1 "ruum_app cannot alter a table" pq -d ruum -c "set role ruum_app" -c "alter table nodes add column y int"
    expect 1 "ruum_app cannot drop a table" pq -d ruum -c "set role ruum_app" -c "drop table nodes"
    expect 1 "ruum_app cannot truncate a table" pq -d ruum -c "set role ruum_app" -c "truncate nodes"
    pq -d ruum -c "set role ruum" -c "create table later (id int)" >/dev/null
    expect 0 "a table of a later migration gets ruum_app's data rights (default privileges)" pq -d ruum -c "set role ruum_app" -c "insert into later values (1)" -c "select count(*) from later"
    pq -d ruum -c "set role ruum" -c "grant insert on _sqlx_migrations to ruum_app" >/dev/null
    expect 0 "the grants take back a write right on _sqlx_migrations" pgrants
    expect 1 "... and ruum_app cannot write it again" pq -d ruum -c "set role ruum_app" -c "insert into _sqlx_migrations values (3, true)"
    pq -d postgres -c "grant ruum to ruum_app" >/dev/null
    expect 3 "the grants fail when ruum_app is a member of ruum (it would have the owner's rights)" pgrants
    docker rm -f "$pgc" >/dev/null 2>&1
  else
    echo "== runtime role grants: skipped (postgres:17 did not start: $(head -c 200 "$TMP/pg.err"))"
  fi
else
  echo "== runtime role grants: skipped (no docker)"
fi

if [[ $failed == 0 ]]; then echo "staging tests: all passed"; else echo "staging tests: FAILED"; fi
exit "$failed"
