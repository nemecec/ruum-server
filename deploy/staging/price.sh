#!/usr/bin/env bash
# The monthly price of a staging plan (review 2 C-2). Refuses (exit 2) above the budget.
#
#   price.sh <pricing.json> <cluster-plan.json> [<persistent-plan.json>] [--snapshot-gb N]
#
# pricing.json is `GET https://api.hetzner.cloud/v1/pricing` (remote.sh fetches it live). Prices
# are gross (with VAT, as the API reports them), per month. The end state of the plan counts
# (planned_values), not only what is new. Items that the Hetzner API does not price are fixed
# estimates and marked "estimate": the Hetzner Object Storage base fee and Scaleway.
set -euo pipefail
# Decimal points, whatever the locale of the caller.
export LC_ALL=C
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=versions.env
. "$HERE/versions.env"

pricing="${1:-}"
cluster="${2:-}"
[[ -r "$pricing" && -r "$cluster" ]] || { echo "usage: price.sh <pricing.json> <cluster-plan.json> [<persistent-plan.json>] [--snapshot-gb N]" >&2; exit 1; }
shift 2
persistent=""
snapshot_gb=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --snapshot-gb) snapshot_gb="$2"; shift 2 ;;
    *) persistent="$1"; shift ;;
  esac
done
budget="${BUDGET_EUR_PER_MONTH:-50}"

# Published prices that are not in the Cloud API (check on the first invoice, plan R-13).
OBJECT_STORAGE_BASE_GROSS=6.19 # EUR 4.99 net + 24 % VAT, once per project
SCALEWAY_ESTIMATE=1.00         # backups, blob copy, state: under 30 GB at about EUR 0.015/GB

lines="$(jq -r --slurpfile p "$pricing" --argjson snap "$snapshot_gb" \
  --argjson osb "$OBJECT_STORAGE_BASE_GROSS" --argjson scw "$SCALEWAY_ESTIMATE" \
  --slurpfile pers <( [[ -n "$persistent" ]] && cat "$persistent" || echo '{}' ) '
  ($p[0].pricing) as $P
  | def loc_price(prices; loc): ([prices[] | select(.location == loc) | .price_monthly.gross | tonumber] | first)
      // error("no price for location \(loc)");
  [ .planned_values.root_module.resources[]? ] as $r
  | ( [ $r[] | select(.type == "hcloud_server") | .values ] | group_by(.server_type + "@" + .location)
      | map(.[0] as $f | { item: "server \($f.server_type) (\($f.location))", count: length,
              each: loc_price([$P.server_types[] | select(.name == $f.server_type)][0].prices; $f.location) }) )
    + ( [ $r[] | select(.type == "hcloud_load_balancer") | .values ] | group_by(.load_balancer_type)
      | map(.[0] as $f | { item: "load balancer \($f.load_balancer_type) (\($f.location))", count: length,
              each: loc_price([$P.load_balancer_types[] | select(.name == $f.load_balancer_type)][0].prices; $f.location) }) )
    + ( [ $r[] | select(.type == "hcloud_primary_ip") | .values ] | group_by(.type)
      | map(.[0] as $f | { item: "primary \($f.type) (\($f.location))", count: length,
              each: (if $f.type == "ipv6" then 0 else loc_price([$P.primary_ips[] | select(.type == "ipv4")][0].prices; $f.location) end) }) )
    + [ { item: "Talos snapshot (\($snap) GB)", count: 1, each: ($snap * ($P.image.price_per_gb_month.gross | tonumber)) } ]
    + ( if ([ $r[] | select(.type == "minio_s3_bucket") ] | length) > 0
        then [ { item: "Hetzner Object Storage base fee (estimate, not in the API)", count: 1, each: $osb } ] else [] end )
    + ( if ([ ($pers[0].planned_values.root_module.resources // [])[] | select(.type == "scaleway_object_bucket") ] | length) > 0
          or ([ $r[] | select(.type == "minio_s3_bucket") ] | length) > 0
        then [ { item: "Scaleway Object Storage: backups, blob copy, state (estimate)", count: 1, each: $scw } ] else [] end )
  | .[] | [ .item, .count, .each, (.count * .each) ] | @tsv
' "$cluster")"

total=0
printf '%-62s %5s %10s %10s\n' "Item" "Count" "EUR each" "EUR/month"
while IFS=$'\t' read -r item count each sum; do
  [[ -n "$item" ]] || continue
  printf '%-62s %5s %10.2f %10.2f\n' "$item" "$count" "$each" "$sum"
  total="$(awk -v a="$total" -v b="$sum" 'BEGIN { printf "%.4f", a + b }')"
done <<<"$lines"
printf '%-62s %5s %10s %10.2f\n' "Total (gross, VAT included as the API reports it)" "" "" "$total"
echo "Budget: EUR $budget per month. Not counted: ruum-buildbox (the build server)."
echo "Temporary while the snapshot is uploaded: one ${HCLOUD_SERVER_TYPE} for about 10 minutes (under EUR 0.01)."
if awk -v t="$total" -v b="$budget" 'BEGIN { exit !(t > b) }'; then
  echo "REFUSED: the plan costs EUR $total per month, above the budget of EUR $budget (ask the person)."
  exit 2
fi
