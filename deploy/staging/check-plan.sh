#!/usr/bin/env bash
# The guard of every staging plan (plan section 7 and 11.2, review 1 B-1, B-2, B-9, review 2 D-3).
#
#   check-plan.sh <plan.json>           check a plan (the JSON of `tofu show -json <planfile>`)
#   check-plan.sh --repo                check the repository: no HCLOUD_TOKEN in any cluster file
#
# It refuses (exit 3) a plan that:
#   - has any hcloud_zone resource, in any action (the zones are data sources only);
#   - has a resource that can carry labels or tags without env=staging and managed-by=tofu;
#   - has a resource type it does not know (fail closed: a new type needs a rule here first);
#   - has an hcloud_server without firewall_ids, or with user_data;
#   - creates, changes or deletes a DNS record outside the staging names, or a notify.staging one;
#   - touches anything named ruum-buildbox, or a bucket outside ruum-staging-*;
#   - has more than 4 servers, or a server type other than the pinned one.
# It reads only the plan; it never contacts a provider. The plan JSON must not hold the
# variables (remote.sh removes them before it writes the file: they include the passphrase).
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

plan="${1:-}"
[[ -r "$plan" ]] || die "usage: check-plan.sh <plan.json> | --repo"
command -v jq >/dev/null || die "jq is required"
jq -e 'has("variables") | not' "$plan" >/dev/null || die "the plan JSON holds the variables (with the passphrase); remove them first (jq 'del(.variables)')"

# The staging DNS names (zone, name, type). Plan section 7.
ALLOWED_RRSETS='[
  ["ruum.space","staging","A"],["ruum.space","staging","AAAA"],["ruum.space","staging","CAA"],
  ["ruum.space","auth.staging","A"],["ruum.space","auth.staging","AAAA"],
  ["ruum.space","dav.staging","A"],["ruum.space","dav.staging","AAAA"],
  ["ruumapps.net","content.staging","A"],["ruumapps.net","content.staging","AAAA"],["ruumapps.net","content.staging","CAA"],
  ["ruumapps.net","*.content.staging","A"],["ruumapps.net","*.content.staging","AAAA"]
]'

violations="$(jq -r \
  --argjson rrsets "$ALLOWED_RRSETS" \
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
  def val: (.change.after // .change.before // {});
  def acts: (.change.actions | join(","));
  def labels_ok(l): (l != null) and (l.env == "staging") and (l["managed-by"] == "tofu");
  [ .resource_changes[]? | select(.mode == "managed") | select(.change.actions != ["no-op"] and .change.actions != ["read"]) ] as $rc
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
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.actions | index("delete") | not)
        | select(((.change.after.firewall_ids // []) | length) == 0 and ((.change.after_unknown.firewall_ids // false) == false))
        | "server \(.address) has no firewall_ids: the firewall must be on from the first boot" ]
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.actions | index("delete") | not)
        | select(((.change.after.user_data // "") != "") or ((.change.after_unknown.user_data // false) == true))
        | "server \(.address) has user_data: the Talos config must not go through the metadata service" ]
    + [ $rc[] | select(.type == "hcloud_server") | select(.change.actions | index("delete") | not)
        | select(.change.after.server_type != $server_type)
        | "server \(.address) type \(.change.after.server_type) is not the pinned \($server_type)" ]
    # 5. Never the build server.
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
    + [ $rc[] | select(.type == "scaleway_object_bucket") | select(.change.actions | index("delete"))
        | "bucket \(val.name) would be deleted: backup buckets are kept on purpose" ]
    # 8. Size: at most 4 servers in the end state (review 2 C-1).
    + ( [ .planned_values.root_module.resources[]? | select(.type == "hcloud_server") ] | length
        | if . > 4 then ["\(.) servers planned: at most 4 within the budget"] else [] end )
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
