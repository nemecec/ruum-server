#!/usr/bin/env bash
# The guard of every staging plan (plan section 7 and 11.2, review 1 B-1, B-2, B-9, review 2 D-3).
#
#   check-plan.sh <plan.json> [--allow-delete] [--allow-replace]
#                                       check a plan (the scrubbed JSON of `tofu show -json`)
#   check-plan.sh --repo                check the repository: no HCLOUD_TOKEN in any cluster file
#
# It refuses (exit 3) a plan that:
#   - has any hcloud_zone resource, in any action (the zones are data sources only);
#   - has a resource that can carry labels or tags without env=staging and managed-by=tofu;
#   - has a resource type it does not know (fail closed: a new type needs a rule here first);
#   - has an hcloud_server without firewall_ids, with a known firewall ID that is not the
#     staging firewall of this plan, or with user_data;
#   - creates, changes or deletes a DNS record outside the staging names, or a notify.staging one;
#   - touches anything named ruum-buildbox, or a bucket outside ruum-staging-*;
#   - has more than 4 servers (child modules included), or a server type other than the pinned one;
#   - deletes or replaces a server or the firewall (unless --allow-delete for the destroy plan,
#     or --allow-replace on purpose);
#   - has a firewall rule that is not inbound tcp 50000, tcp 6443 or ICMP from the operator's Mac
#     (STG_ADMIN_CIDRS: a JSON list of exactly one IPv4 /32; without it every firewall rule is
#     refused), has rules or apply_to that are unknown at plan time, or uses apply_to;
#   - attaches the firewall, a load balancer target, a network or a primary IP to a server outside
#     the plan (or the build server, STG_BUILDBOX_ID), or uses a label selector other than
#     env=staging,role=node;
#   - sets an object lock other than COMPLIANCE for BACKUP_LOCK_DAYS days, or removes one.
# It reads only the plan; it never contacts a provider. The plan JSON must not hold the
# variables or the prior state (steps.sh pipes it through plan-scrub.jq before it writes it).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=versions.env
. "$HERE/versions.env"

die() { printf 'check-plan: %s\n' "$*" >&2; exit 1; }

if [[ "${1:-}" == --repo ]]; then
  # Review stgsec M-10: the Hetzner token never goes into the cluster or CI.
  root="$(cd "$HERE/../.." && pwd)"
  hits="$(grep -rIl 'HCLOUD_TOKEN' "$HERE/k8s" "$HERE/charts" "$root/.github/workflows" 2>/dev/null || true)"
  if [[ -n "$hits" ]]; then
    printf 'REFUSED: HCLOUD_TOKEN is named in a cluster or CI file:\n%s\n' "$hits"
    exit 3
  fi
  hits="$(grep -rIlE 'STRIPE_(SECRET|PUBLISHABLE)_KEY' "$HERE/k8s" "$HERE/charts" 2>/dev/null || true)"
  if [[ -n "$hits" ]]; then
    printf 'REFUSED: a Stripe key is named in a cluster file:\n%s\n' "$hits"
    exit 3
  fi
  echo "check-plan --repo: OK (no HCLOUD_TOKEN or Stripe key in cluster or CI files)"
  exit 0
fi

plan=""
allow_delete=0
allow_replace=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --allow-delete) allow_delete=1; shift ;;
    --allow-replace) allow_replace=1; shift ;;
    -*) die "unknown option $1" ;;
    *) [[ -z "$plan" ]] || die "one plan at a time"; plan="$1"; shift ;;
  esac
done
[[ -r "$plan" ]] || die "usage: check-plan.sh <plan.json> [--allow-delete] [--allow-replace] | --repo"
command -v jq >/dev/null || die "jq is required"
jq -e 'has("variables") | not' "$plan" >/dev/null || die "the plan JSON holds the variables (with the passphrase); scrub it first (jq -f plan-scrub.jq)"
jq -e 'has("prior_state") | not' "$plan" >/dev/null || die "the plan JSON holds the prior state (Talos secrets after the first apply); scrub it first (jq -f plan-scrub.jq)"

# The staging DNS names (zone, name, type). Plan section 7. One list for check-plan and sweep.
ALLOWED_RRSETS="$(jq -c . "$HERE/rrsets.json")"

# The firewall source: the current public IPv4 address of the operator's Mac as one /32 (steps.sh
# admin_cidrs; decision of the person, 2026-11-11). Exactly one IPv4 host, nothing wider, no
# IPv6. Without it a plan that creates or changes a firewall rule is refused (fail closed).
admin_cidrs="${STG_ADMIN_CIDRS:-[]}"
jq -e 'type == "array" and all(.[]; type == "string")' <<<"$admin_cidrs" >/dev/null 2>&1 ||
  die "STG_ADMIN_CIDRS must be a JSON list of CIDR strings"
jq -e 'length <= 1' <<<"$admin_cidrs" >/dev/null || die "STG_ADMIN_CIDRS must hold exactly one address (the Mac's public IPv4 as a /32)"
bad_cidr="$(jq -r '.[] | select(test("^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])/32$") | not)' <<<"$admin_cidrs")"
[[ -z "$bad_cidr" ]] || die "STG_ADMIN_CIDRS must be one IPv4 address as a /32, not $bad_cidr"
# The build server's Hetzner ID (steps.sh), so no attachment or target can name it.
buildbox_id="${STG_BUILDBOX_ID:-}"
[[ -z "$buildbox_id" || "$buildbox_id" =~ ^[0-9]+$ ]] || die "STG_BUILDBOX_ID must be a number"
# The object lock of the backup buckets: COMPLIANCE for exactly these days (versions.env).
lock_days="${BACKUP_LOCK_DAYS:?versions.env sets BACKUP_LOCK_DAYS}"

violations="$(jq -r \
  --argjson rrsets "$ALLOWED_RRSETS" \
  --argjson admin "$admin_cidrs" \
  --arg buildbox "$buildbox_id" \
  --argjson lock_days "$lock_days" \
  --argjson allow_delete "$allow_delete" \
  --argjson allow_replace "$allow_replace" \
  --arg server_type "$HCLOUD_SERVER_TYPE" '
  # Types that carry labels (hcloud) or tags (scaleway): they must be labelled.
  def labelled_types: ["hcloud_server","hcloud_network","hcloud_firewall","hcloud_load_balancer",
    "hcloud_primary_ip","hcloud_placement_group","hcloud_zone_rrset","hcloud_volume",
    "hcloud_floating_ip","hcloud_ssh_key","hcloud_snapshot","hcloud_certificate",
    "hcloud_managed_certificate","hcloud_uploaded_certificate"];
  def tagged_types: ["scaleway_object_bucket"];
  # Types that cannot carry labels: allowed, with the name rules below.
  def unlabelled_types: ["hcloud_network_subnet","hcloud_load_balancer_network",
    "hcloud_load_balancer_target","hcloud_load_balancer_service","hcloud_server_network",
    "hcloud_firewall_attachment","scaleway_object_bucket_lock_configuration",
    "minio_s3_bucket","talos_machine_secrets","talos_machine_configuration_apply",
    "talos_machine_bootstrap","terraform_data"];
  # The only firewall rules: Talos API, Kubernetes API, ICMP, inbound, from the Mac of the operator.
  def allowed_rules: [["tcp","50000"],["tcp","6443"],["icmp",""]];
  # The only label selector of a target or an attachment: the staging nodes.
  def node_selector_ok(s): (s // "" | split(",") | map(gsub("^ +| +$"; "")) | sort) == ["env=staging","role=node"];
  def val: (.change.after // .change.before // {});
  def acts: (.change.actions | join(","));
  def deletes: (.change.actions | index("delete")) != null;
  def replaces: deletes and ((.change.actions | index("create")) != null);
  def labels_ok(l): (l != null) and (l.env == "staging") and (l["managed-by"] == "tofu");
  # Every resource of the end state, child modules included (review 2 P-1 plans modules).
  def all_planned: [ .planned_values.root_module // {} | recurse(.child_modules[]?) | .resources[]? ];
  [ .resource_changes[]? | select(.mode == "managed") | select(.change.actions != ["no-op"] and .change.actions != ["read"]) ] as $rc
  # The Hetzner IDs of the servers of this plan (known only for servers that exist already).
  | ([ .resource_changes[]? | select(.type == "hcloud_server") | (.change.before.id?, .change.after.id?) | select(. != null) | tostring ] | unique) as $plan_server_ids
  # server_id_ok(id; unknown): an ID of a server of this plan, or one that is known only after
  # apply (a server that this plan creates); never the build server.
  | def server_id_ok(id; unknown):
      if unknown == true then true
      elif id == null then false
      else ((id | tostring) as $i | ($plan_server_ids | index($i)) != null and $i != $buildbox) end;
  # The Hetzner IDs of the firewalls of this plan (known once the firewall exists).
  ([ .resource_changes[]? | select(.type == "hcloud_firewall") | (.change.before.id?, .change.after.id?) | select(. != null) | tostring ] | unique) as $plan_firewall_ids
  | (
    # 1. No zone resource at all (review 1 B-9).
    [ $rc[] | select(.type == "hcloud_zone") | "zone resource \(.address) (\(acts)): zones are data sources only" ]
    # 2. Unknown types (fail closed).
    + [ $rc[] | select((.type | IN(labelled_types[], tagged_types[], unlabelled_types[], "hcloud_zone")) | not)
        | "unknown resource type \(.type) at \(.address): add a rule to check-plan.sh first" ]
    # 3. Labels and tags (deletes are checked on the old value).
    + [ $rc[] | select(.type | IN(labelled_types[])) | select(labels_ok(val.labels) | not)
        | "unlabelled \(.address) (\(acts)): needs env=staging and managed-by=tofu" ]
    + [ $rc[] | select(.type | IN(tagged_types[])) | select(labels_ok(val.tags) | not)
        | "untagged \(.address) (\(acts)): needs tags env=staging and managed-by=tofu" ]
    # 4. Servers: firewall at create, no user_data (review 1 B-1, B-2), pinned type.
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.after != null)
        | select(((.change.after.firewall_ids // []) | length) == 0 and ((.change.after_unknown.firewall_ids // false) == false))
        | "server \(.address) has no firewall_ids: the firewall must be on from the first boot" ]
    # Code review 2 G-1: a known firewall ID must be the staging firewall of this plan; only an
    # ID that is unknown at plan time (the firewall is created in the same plan) is left open.
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.after != null) | . as $r
        | select((.change.after_unknown.firewall_ids // false) != true)
        | (.change.after.firewall_ids // []) | to_entries[]
        | select((($r.change.after_unknown.firewall_ids // [])[.key]? // false) != true)
        | select(.value != null)
        | select((.value | tostring) as $f | ($plan_firewall_ids | index($f)) == null)
        | "server \($r.address) firewall \(.value) is not the staging firewall of this plan" ]
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.after != null)
        | select(((.change.after.user_data // "") != "") or ((.change.after_unknown.user_data // false) == true))
        | "server \(.address) has user_data: the Talos config must not go through the metadata service" ]
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.after != null)
        | select(.change.after.server_type != $server_type)
        | "server \(.address) type \(.change.after.server_type) is not the pinned \($server_type)" ]
    # 4b. No server or firewall is deleted or replaced by accident (code review F-4): a replace
    # loses the node data, a firewall delete opens the nodes. destroy-steps.sh passes
    # --allow-delete for the destroy plan; an intended replace needs --allow-replace.
    + [ $rc[] | select(.type | IN("hcloud_server","hcloud_firewall")) | select(replaces)
        | select($allow_replace == 0 and $allow_delete == 0)
        | "\(.address) would be replaced (\(acts)): pass --allow-replace only on purpose" ]
    + [ $rc[] | select(.type | IN("hcloud_server","hcloud_firewall")) | select(deletes and (replaces | not))
        | select($allow_delete == 0)
        | "\(.address) would be deleted (\(acts)): only the destroy plan deletes it (--allow-delete)" ]
    # 5. Never the build server (it stays in the project for builds and tests).
    + [ $rc[] | select((val.name // "") | test("buildbox"))
        | "\(.address) is named \(val.name): the build server is never touched" ]
    # 6. DNS records: only the staging names; never notify.staging.
    + [ $rc[] | select(.type == "hcloud_zone_rrset")
        | select([val.zone, val.name, val.type] as $k | ($rrsets | index([$k])) | not)
        | "DNS record \(val.name).\(val.zone) \(val.type) (\(acts)) is not a staging name" ]
    + [ $rc[] | select(.type == "hcloud_zone_rrset") | select((val.name // "") | test("notify"))
        | "DNS record \(val.name) belongs to Scaleway email and is never touched" ]
    # 7. Buckets: only ruum-staging-*.
    + [ $rc[] | select(.type == "minio_s3_bucket") | select((val.bucket // "") | startswith("ruum-staging-") | not)
        | "bucket \(val.bucket) is not a staging bucket" ]
    + [ $rc[] | select(.type == "scaleway_object_bucket") | select((val.name // "") | startswith("ruum-staging-") | not)
        | "bucket \(val.name) is not a staging bucket" ]
    + [ $rc[] | select(.type == "scaleway_object_bucket") | select(deletes)
        | "bucket \(val.name) would be deleted: backup buckets are kept on purpose" ]
    # 8. Size: at most 4 servers in the end state (review 2 C-1), child modules included.
    + ( [ all_planned[] | select(.type == "hcloud_server") ] | length
        | if . > 4 then ["\(.) servers planned: at most 4 within the budget"] else [] end )
    # 9. Firewall rules (code review F-4): inbound only, only the three rules, only from the
    # Mac of the operator. Unknown sources are refused. Code review 2 G-1: apply_to is checked once per
    # firewall (also with zero rules), and rules unknown at plan time are refused.
    + [ $rc[] | select(.type == "hcloud_firewall") | select(.change.after != null) | . as $r
        | ( (if (($r.change.after.apply_to // []) | length) > 0 then ["\($r.address): apply_to is not used (the firewall goes on at server create)"] else [] end)
          + (if (($r.change.after_unknown.apply_to // false) == true) or (($r.change.after_unknown.apply_to // []) | type == "array" and any(.[]; . != false))
               then ["\($r.address): apply_to unknown at plan time"] else [] end)
          + (if (($r.change.after_unknown.rule // false) == true)
               or (($r.change.after_unknown.rule // []) | type == "array" and any(.[]; . == true))
               then ["\($r.address): firewall rules unknown at plan time"] else [] end)
          ) | .[] ]
    + [ $rc[] | select(.type == "hcloud_firewall") | select(.change.after != null) | . as $r
        | (.change.after.rule // []) | to_entries[] | .key as $i | .value as $rule
        | ($r.change.after_unknown.rule[$i]? | if type == "object" then . else {} end) as $u
        | ( (if ($rule.direction // "") != "in" then ["\($r.address) rule \($i): direction \($rule.direction) (only inbound rules)"] else [] end)
          + (if ([($rule.protocol // ""), ($rule.port // "")] as $pp | allowed_rules | index([$pp])) == null
               then ["\($r.address) rule \($i): \($rule.protocol) port \($rule.port // "") is not Talos (tcp 50000), Kubernetes (tcp 6443) or ICMP"] else [] end)
          + (if (($u.source_ips // false) == true) or (($u.source_ips // []) | type == "array" and any(.[]; . == true))
               then ["\($r.address) rule \($i): source_ips unknown at plan time"] else [] end)
          + [ ($rule.source_ips // [])[] | select(. as $s | ($admin | index($s)) == null)
              | "\($r.address) rule \($i): source \(.) is not the operator address \($admin | join(", ") | if . == "" then "(none given: STG_ADMIN_CIDRS)" else . end)" ]
          + (if (($rule.source_ips // []) | length) == 0 then ["\($r.address) rule \($i): no source_ips"] else [] end)
          + (if (($rule.destination_ips // []) | length) > 0 then ["\($r.address) rule \($i): destination_ips are not used"] else [] end)
          ) | .[] ]
    # 10. Firewall attachments: only servers of this plan, never the build server; a label
    # selector only for the staging nodes.
    + [ $rc[] | select(.type == "hcloud_firewall_attachment") | select(.change.after != null) | . as $r
        | ( [ (.change.after.server_ids // [])[] | select(server_id_ok(.; false) | not)
              | "\($r.address): firewall attachment to server \(.) outside this plan" ]
          + [ (.change.after.label_selectors // [])[] | select(node_selector_ok(.) | not)
              | "\($r.address): firewall attachment by label selector \(.) (only env=staging,role=node)" ]
          + (if ((.change.after_unknown.server_ids // false) == true) and (($rc | map(select(.type == "hcloud_server" and (.change.actions | index("create")))) | length) == 0)
               then ["\($r.address): firewall attachment to servers unknown at plan time, but this plan creates no server"] else [] end)
          ) | .[] ]
    # 11. Load balancer targets: the staging node selector, or a server of this plan; never an IP.
    + [ $rc[] | select(.type == "hcloud_load_balancer_target") | select(.change.after != null) | .change.after as $a | . as $r
        | if $a.type == "label_selector" then
            (if node_selector_ok($a.label_selector) then empty else "\($r.address): target label selector \($a.label_selector) (only env=staging,role=node)" end)
          elif $a.type == "server" then
            (if server_id_ok($a.server_id; ($r.change.after_unknown.server_id // false)) then empty else "\($r.address): target server \($a.server_id) is outside this plan" end)
          else "\($r.address): target type \($a.type) (only the staging nodes)" end ]
    # 12. Server networks: only servers of this plan.
    + [ $rc[] | select(.type == "hcloud_server_network") | select(.change.after != null) | . as $r
        | select(server_id_ok(.change.after.server_id; (.change.after_unknown.server_id // false)) | not)
        | "\($r.address): network attachment of server \(.change.after.server_id) outside this plan" ]
    # 12b. Primary IPs (code review 2 G-1): unassigned, assigned by a server that this plan creates
    # (unknown at plan time), or a known assignee that is a server of this plan.
    + [ $rc[] | select(.type == "hcloud_primary_ip") | select(.change.after != null) | . as $r
        | select((.change.after.assignee_id // 0) != 0)
        | select(server_id_ok(.change.after.assignee_id; false) | not)
        | "\($r.address): primary IP assigned to server \(.change.after.assignee_id) outside this plan" ]
    # 13. Object lock: COMPLIANCE for exactly the planned days, never deleted (review 1 B-3).
    + [ $rc[] | select(.type == "scaleway_object_bucket_lock_configuration") | select(deletes)
        | "\(.address) would be deleted (\(acts)): the compliance lock is never removed" ]
    + [ $rc[] | select(.type == "scaleway_object_bucket_lock_configuration") | select(.change.after != null) | . as $r
        | ([ (.change.after.rule // [])[] | (.default_retention // [])[] ]) as $d
        | if ($d | length) != 1 then "\($r.address): needs exactly one default retention"
          elif ($d[0].mode != "COMPLIANCE") or ($d[0].days != $lock_days) or ($d[0].years != null)
            then "\($r.address): lock \($d[0].mode) \($d[0].days // "-") days \($d[0].years // "-") years; only COMPLIANCE for \($lock_days) days"
          else empty end ]
  ) | .[]
' "$plan")"

warnings="$(jq -r '
  [ .resource_changes[]? | select(.type == "hcloud_server") | select(.change.after.image? == "pending-talos-snapshot")
    | "\(.address): the Talos snapshot does not exist yet; up.sh --apply uploads it first" ] | unique | .[]
' "$plan")"
if [[ -n "$warnings" ]]; then
  while IFS= read -r line; do printf 'note: %s\n' "$line"; done <<<"$warnings"
fi

if [[ -n "$violations" ]]; then
  echo "check-plan: REFUSED"
  while IFS= read -r line; do printf '  - %s\n' "$line"; done <<<"$violations"
  exit 3
fi
counts="$(jq -r '[.resource_changes[]? | select(.mode=="managed") | .change.actions | join(",")] | group_by(.) | map("\(.[0]): \(length)") | join(", ")' "$plan")"
echo "check-plan: OK ($counts)"
