#!/usr/bin/env bash
# The build-server half of destroy.sh (review 2 D-1, D-2, D-3). Environment: WORK, HERE and the
# tofu settings of remote.sh (tofu_env has run), HCLOUD_TOKEN.
#
#   destroy-remote.sh destroy-plan          three lists + state vs labels; changes nothing
#   destroy-remote.sh destroy               the same plan, then apply it; snapshot; checks
#   destroy-remote.sh sweep                 delete env=staging resources by label (lost state)
set -euo pipefail
step="$1"
shift || true
# shellcheck source=../versions.env
. "$HERE/versions.env"
log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
hapi() {
  printf 'header = "Authorization: Bearer %s"\n' "$HCLOUD_TOKEN" | curl -fsS -K - "https://api.hetzner.cloud/v1/$1"
}

# The staging DNS names (check-plan.sh has the same list).
STAGING_RRSETS='ruum.space staging A|ruum.space staging AAAA|ruum.space staging CAA|ruum.space auth.staging A|ruum.space auth.staging AAAA|ruum.space dav.staging A|ruum.space dav.staging AAAA|ruumapps.net content.staging A|ruumapps.net content.staging AAAA|ruumapps.net content.staging CAA|ruumapps.net *.content.staging A|ruumapps.net *.content.staging AAAA'

# What exists in Hetzner with env=staging (servers, IPs, LB, network, firewall, placement
# group, snapshots) and the staging rrsets, as "kind name" lines.
labelled() {
  local kind
  for kind in servers primary_ips load_balancers networks firewalls placement_groups; do
    hapi "$kind?label_selector=env%3Dstaging&per_page=50" | jq -r --arg k "$kind" '.[$k][] | "\($k) \(.name)"'
  done
  hapi "images?type=snapshot&label_selector=env%3Dstaging" | jq -r '.images[] | "snapshot \(.description)"'
  local zone name type
  IFS='|' read -ra sets <<<"$STAGING_RRSETS"
  for s in "${sets[@]}"; do
    read -r zone name type <<<"$s"
    hapi "zones/$zone/rrsets/$name/$type" >/dev/null 2>&1 && echo "rrset $name.$zone $type"
  done
  return 0
}

plan_destroy() {
  local dir="$WORK/tree/tofu/cluster"
  [[ -d "$dir/.terraform" ]] || die "no initialised cluster root in $dir"
  # D-1: a normal plan first; when the nodes do not answer, -refresh=false.
  if ! (cd "$dir" && tofu plan -destroy -input=false -no-color -lock-timeout=60s -out="$WORK/destroy.tfplan" >"$WORK/destroy-plan.log" 2>&1); then
    log "the normal destroy plan failed (nodes down?); retrying with -refresh=false"
    (cd "$dir" && tofu plan -destroy -refresh=false -input=false -no-color -lock-timeout=60s -out="$WORK/destroy.tfplan" >"$WORK/destroy-plan.log" 2>&1) ||
      { tail -30 "$WORK/destroy-plan.log" >&2; die "tofu plan -destroy failed also without refresh; see --sweep"; }
  fi
  (cd "$dir" && tofu show -json "$WORK/destroy.tfplan" | jq 'del(.variables)' >"$WORK/destroy.json")
}

# three_lists [PLAN_JSON ACTION]: from the destroy plan (deletes), or, before staging exists,
# from the last up.sh plan (creates: what destroy would remove after up).
three_lists() {
  local src="${1:-$WORK/destroy.json}" action="${2:-delete}"
  if [[ "$action" == delete ]]; then
    echo "################ REMOVED (tofu destroy of the cluster root) ################"
  else
    echo "################ REMOVED after up.sh (staging does not exist yet: the cluster root's creates) ################"
  fi
  jq -r --arg a "$action" '.resource_changes[] | select(.change.actions | index($a)) | "  - \(.address)"' "$src"
  echo "  - Talos snapshot(s) with labels env=staging,os=talos (hcloud, after the destroy)"
  echo
  echo "################ KEPT ON PURPOSE ################"
  echo "  - Scaleway ruum-staging-pg-backup   (object lock, compliance; tofu/persistent; --purge-backups after the retention)"
  echo "  - Scaleway ruum-staging-blobs-copy  (object lock, compliance; tofu/persistent)"
  echo "  - Scaleway ruum-staging-tfstate     (the encrypted state; bootstrap-state.sh)"
  echo
  echo "################ OUTSIDE STAGING, NEVER TOUCHED ################"
  echo "  - server ruum-buildbox, its firewall ruum-buildbox, its primary IPs, its SSH key"
  echo "  - DNS zones ruum.space and ruumapps.net (data sources only), the apex and every production name"
  echo "  - notify.staging.ruum.space MX/TXT/DKIM/DMARC (Scaleway email)"
  echo
  local zones
  zones="$(jq -r '[.resource_changes[] | select(.type == "hcloud_zone")] | length' "$src")"
  [[ "$zones" == 0 ]] || die "the plan contains a zone resource; refusing"
  bash "$HERE/check-plan.sh" "$src"
  echo
  echo "################ STATE vs LABELS (review 2 D-2) ################"
  if [[ "$action" == delete ]]; then
    jq -r '.resource_changes[] | select(.change.actions | index("delete")) | .change.before
           | select(. != null) | (.name // .bucket // empty)' "$src" | sort -u >"$WORK/in-state.txt"
  else
    : >"$WORK/in-state.txt"
  fi
  labelled | sort -u >"$WORK/labelled.txt"
  local orphans=0 kind name
  while read -r kind name; do
    [[ "$kind" == snapshot || "$kind" == rrset ]] && continue
    if ! grep -qxF "$name" "$WORK/in-state.txt"; then
      echo "  NOT IN STATE (orphan, --sweep removes it): $kind $name"
      orphans=1
    fi
  done <"$WORK/labelled.txt"
  if [[ ! -s "$WORK/labelled.txt" ]]; then
    echo "  no resource with env=staging exists in Hetzner (nothing to sweep)"
  elif [[ $orphans == 0 ]]; then
    echo "  every labelled resource is in the state"
  fi
}

case "$step" in
  destroy-plan)
    if [[ "${STG_HAS_STATE:-1}" == 0 ]]; then
      echo "NOTE: there is no state bucket, so staging was never applied. The lists below come from the last up.sh plan."
      [[ -s "$WORK/cluster.json" ]] || die "no plan in $WORK; run up.sh (plan) first"
      three_lists "$WORK/cluster.json" create
    else
      plan_destroy
      three_lists
    fi
    ;;
  destroy)
    [[ "${RUUM_STAGING_DESTROY:-}" == "destroy ruum-staging" ]] || die "destroy needs RUUM_STAGING_DESTROY='destroy ruum-staging'"
    plan_destroy
    three_lists
    (cd "$WORK/tree/tofu/cluster" && tofu apply -input=false -no-color -lock-timeout=60s "$WORK/destroy.tfplan")
    for id in $(hapi "images?type=snapshot&label_selector=env%3Dstaging%2Cos%3Dtalos" | jq -r '.images[].id'); do
      HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud image delete "$id"
    done
    left="$(hapi "servers?label_selector=env%3Dstaging" | jq '.servers | length')"
    lbs="$(hapi "load_balancers?label_selector=env%3Dstaging" | jq '.load_balancers | length')"
    bb="$(hapi "servers?name=ruum-buildbox" | jq -r '.servers[0].status')"
    echo "after destroy: staging servers $left, load balancers $lbs, ruum-buildbox $bb"
    [[ "$left" == 0 && "$lbs" == 0 && "$bb" == running ]] || die "destroy check failed"
    ;;
  sweep)
    [[ "${RUUM_STAGING_DESTROY:-}" == "destroy ruum-staging" ]] || die "sweep needs RUUM_STAGING_DESTROY='destroy ruum-staging'"
    labelled | while read -r kind name; do
      [[ "$name" == *buildbox* ]] && die "refusing to touch $name"
      case "$kind" in
        servers) HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud server delete "$name" ;;
        load_balancers) HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud load-balancer delete "$name" ;;
        *) : ;;
      esac
    done
    sleep 20
    labelled | while read -r kind name; do
      case "$kind" in
        primary_ips) HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud primary-ip delete "$name" ;;
        networks) HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud network delete "$name" ;;
        firewalls) HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud firewall delete "$name" ;;
        placement_groups) HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud placement-group delete "$name" ;;
        rrset)
          read -r fqdn type <<<"$name"
          zone="${fqdn#*.staging.}"; zone="${zone#staging.}"
          case "$fqdn" in *.ruum.space) zone=ruum.space ;; *.ruumapps.net) zone=ruumapps.net ;; esac
          HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud zone rrset delete "$zone" "${fqdn%.$zone}" "$type"
          ;;
        *) : ;;
      esac
    done
    echo "sweep done; the Hetzner blob bucket ruum-staging-blobs is removed with the S3 API (README)"
    ;;
esac
