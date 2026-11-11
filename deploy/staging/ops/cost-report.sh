#!/usr/bin/env bash
# The monthly price of what exists in the Hetzner project now, except the build server
# (review 2 C-2). status.sh runs it in the tools container; the digest reads the last line.
# Environment: HCLOUD_TOKEN. Prices gross from GET /pricing.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../versions.env
. "$HERE/../versions.env"
hapi() {
  printf 'header = "Authorization: Bearer %s"\n' "$HCLOUD_TOKEN" | curl -fsS -K - "https://api.hetzner.cloud/v1/$1"
}
pricing="$(hapi pricing)"
servers="$(hapi servers)"
lbs="$(hapi load_balancers)"
ips="$(hapi primary_ips)"
snaps="$(hapi 'images?type=snapshot')"
jq -rn --argjson p "$pricing" --argjson s "$servers" --argjson l "$lbs" --argjson i "$ips" --argjson im "$snaps" --argjson budget "$BUDGET_EUR_PER_MONTH" '
  def price(list; loc): [list[] | select(.location == loc) | .price_monthly.gross | tonumber] | first // 0;
  ($p.pricing) as $P
  | [ $s.servers[] | select(.name != "ruum-buildbox") | . as $x
      | { item: "server \($x.name) (\($x.server_type.name))", eur: price(([$P.server_types[] | select(.name == $x.server_type.name)][0].prices); $x.datacenter.location.name) } ]
  + [ $l.load_balancers[] | . as $x
      | { item: "load balancer \($x.name) (\($x.load_balancer_type.name))", eur: price(([$P.load_balancer_types[] | select(.name == $x.load_balancer_type.name)][0].prices); $x.location.name) } ]
  + [ $i.primary_ips[] | select(.type == "ipv4") | select((.assignee_id // 0) as $a | ($s.servers[] | select(.name == "ruum-buildbox") | .id) != $a) | . as $x
      | { item: "primary ipv4 \($x.name)", eur: price(([$P.primary_ips[] | select(.type == "ipv4")][0].prices); $x.location.name) } ]
  + [ $im.images[] | { item: "snapshot \(.description)", eur: ((.image_size // 0) * ($P.image.price_per_gb_month.gross | tonumber)) } ]
  | (.[] | "  \(.item): EUR \(.eur * 100 | round / 100)"),
    "  total without the build server (Hetzner Cloud only; add EUR 6.19 object storage and about EUR 1 Scaleway when they exist): EUR \((map(.eur) | add // 0) * 100 | round / 100) of \($budget)"
'
