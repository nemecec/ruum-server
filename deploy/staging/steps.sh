#!/usr/bin/env bash
# The container half of the staging scripts. The Mac scripts (up.sh, deploy.sh, status.sh,
# destroy.sh, secrets-push.sh) run it in the tools container (lib.sh stg_run, tools-image.sh)
# and send the secrets on stdin as NAME=value lines that end with a line "__END__". Only the
# names of the step's allow-list are taken; nothing is printed.
#
# Steps (read-only unless marked rw):
#   admin-cidrs          print the firewall's admin source: the Mac's public IPv4 as a /32
#   plan                 tofu plan of both roots, check-plan, price
#   apply          (rw)  image upload if missing, plan, check-plan, price, apply the exact plan,
#                        user_data check, kubeconfig; needs RUUM_STAGING_APPLY=yes
#   talosconfig    (rw)  write the talosconfig and kubeconfig from the state (0600)
#   platform       (rw)  helmfile sync of the platform (needs the cluster secrets on stdin);
#                        needs RUUM_STAGING_APPLY=yes
#   zitadel-bootstrap (rw) OIDC app, Google and Microsoft, login policy; `check` reports drift
#   check-backup   (rw)  WAL push and a base backup into the locked bucket work (bring-up check)
#   deploy <commit> <server-ref> <edge-ref> [--backup] [--acme ISSUER --acme-email EMAIL] (rw)
#                        helm upgrade of charts/ruum by digest (ops/deploy-chart.sh validates)
#   status               what exists: state, hcloud by label, nodes, pods, backups
#   destroy-plan         three lists: removed, kept on purpose, never touched; state vs labels
#   destroy        (rw)  tofu destroy of the cluster root, snapshot, checks
#   sweep          (rw)  delete by label when the state is lost (review 2 D-2)
#
# The work folder (RUUM_STAGING_WORK, lib.sh) is the person's ~/.config/ruum/staging on the Mac,
# mounted into the container; it holds the kubeconfig, the talosconfig and the plans. The copies
# of the OpenTofu roots and their providers live in the container's /tmp and go with it.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=versions.env
. "$HERE/versions.env"

STEP="${1:-}"
shift || true

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

WORK="${RUUM_STAGING_WORK:?RUUM_STAGING_WORK is not set; run the staging scripts on the Mac (lib.sh starts this in the tools container)}"
umask 077
[[ -d "$WORK" && -w "$WORK" ]] || die "the work folder $WORK is missing or not writable"
KUBECONFIG_FILE="$WORK/kubeconfig"
TALOSCONFIG_FILE="$WORK/talosconfig"
TREE="${TMPDIR:-/tmp}/ruum-staging-tree"

# The steps that create or change cloud resources run only when the person or the orchestrator
# has set RUUM_STAGING_APPLY=yes on the Mac (lib.sh passes it only to rw steps).
require_apply() {
  [[ "${RUUM_STAGING_APPLY:-}" == yes ]] || die "step '$STEP' creates or changes staging; it needs RUUM_STAGING_APPLY=yes"
}

# Read NAME=value lines from stdin until __END__. Only names in the allow-list are exported.
read_secrets() {
  local allowed=" $* " line name
  while IFS= read -r line; do
    [[ "$line" == __END__ ]] && break
    name="${line%%=*}"
    [[ "$name" =~ ^[A-Z0-9_]+$ ]] || continue
    if [[ "$allowed" == *" $name "* ]]; then
      export "$name=${line#*=}"
    fi
  done
}

TOFU_NAMES="RUUM_STAGING_DESTROY HCLOUD_TOKEN HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY SCW_ACCESS_KEY SCW_SECRET_KEY SCW_DEFAULT_PROJECT_ID SCW_DEFAULT_ORGANIZATION_ID TOFU_STATE_PASSPHRASE"

tofu_env() {
  [[ -n "${TOFU_STATE_PASSPHRASE:-}" ]] || die "TOFU_STATE_PASSPHRASE is missing (secrets.env); OpenTofu state is never written unencrypted"
  [[ -n "${HCLOUD_TOKEN:-}" ]] || die "HCLOUD_TOKEN is missing"
  export TF_VAR_state_passphrase="$TOFU_STATE_PASSPHRASE"
  export AWS_ACCESS_KEY_ID="${SCW_ACCESS_KEY:-}" AWS_SECRET_ACCESS_KEY="${SCW_SECRET_KEY:-}"
  export MINIO_USER="${HETZNER_S3_ACCESS_KEY:-}" MINIO_PASSWORD="${HETZNER_S3_SECRET_KEY:-}"
  export TF_IN_AUTOMATION=1 TF_INPUT=0
  export TF_VAR_location="$HCLOUD_LOCATION" TF_VAR_server_type="$HCLOUD_SERVER_TYPE"
  export TF_VAR_node_count="$NODE_COUNT" TF_VAR_lb_type="$HCLOUD_LB_TYPE"
  export TF_VAR_talos_version="$TALOS_VERSION" TF_VAR_kubernetes_version="${KUBERNETES_VERSION#v}"
  export TF_VAR_talos_installer_image="$TALOS_INSTALLER_IMAGE"
  export TF_VAR_retention_days="$BACKUP_LOCK_DAYS"
}

# GET on the Hetzner API; the token goes to curl on stdin (-K -), never on the command line.
hapi() {
  printf 'header = "Authorization: Bearer %s"\n' "$HCLOUD_TOKEN" |
    curl -fsS -K - "https://api.hetzner.cloud/v1/$1"
}

# ipv4_public ADDRESS: true for a public unicast IPv4 address (no private, loopback, link-local,
# shared (CGNAT), multicast, reserved or documentation range).
ipv4_public() {
  local a b c o octets
  [[ "$1" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  octets=("${BASH_REMATCH[@]:1}")
  for o in "${octets[@]}"; do
    [[ "$o" == 0 || "${o:0:1}" != 0 ]] || return 1
    ((10#$o <= 255)) || return 1
  done
  a=${octets[0]} b=${octets[1]} c=${octets[2]}
  ((a == 0 || a == 10 || a == 127 || a >= 224)) && return 1
  ((a == 100 && b >= 64 && b <= 127)) && return 1
  ((a == 169 && b == 254)) && return 1
  ((a == 172 && b >= 16 && b <= 31)) && return 1
  ((a == 192 && b == 168)) && return 1
  ((a == 192 && b == 0 && (c == 0 || c == 2))) && return 1
  ((a == 198 && (b == 18 || b == 19))) && return 1
  ((a == 198 && b == 51 && c == 100)) && return 1
  ((a == 203 && b == 0 && c == 113)) && return 1
  return 0
}

# The only admin source of the firewall (decision of the person, 2026-11-11): the current public
# IPv4 address of the Mac, as one /32. Two echo services must agree, or RUUM_ADMIN_IP gives it.
# The Talos API (50000), the Kubernetes API (6443) and ICMP are open to this address only.
admin_cidrs() {
  local ip a b
  if [[ -n "${RUUM_ADMIN_IP:-}" ]]; then
    ip="$RUUM_ADMIN_IP"
  else
    a="$(curl -4 -fsS --max-time 10 https://api.ipify.org 2>/dev/null | tr -d '[:space:]' || true)"
    b="$(curl -4 -fsS --max-time 10 https://ipv4.icanhazip.com 2>/dev/null | tr -d '[:space:]' || true)"
    [[ -n "$a" && "$a" == "$b" ]] || die "cannot find the public IPv4 address of the Mac (api.ipify.org '$a', ipv4.icanhazip.com '$b'); set RUUM_ADMIN_IP"
    ip="$a"
  fi
  ipv4_public "$ip" || die "the admin address '$ip' is not a public IPv4 address"
  printf '["%s/32"]' "$ip"
}

# The build server's Hetzner ID: check-plan.sh refuses any attachment or target that names it.
# Empty when it does not exist.
buildbox_id() {
  hapi "servers?name=ruum-buildbox" | jq -r '.servers[0].id // empty'
}

TALOS_PLACEHOLDER="pending-talos-snapshot"
talos_image_id() {
  hapi "images?type=snapshot&label_selector=os%3Dtalos%2Cversion%3D${TALOS_VERSION}%2Cenv%3Dstaging" |
    jq -r '[.images[] | select(.status == "available")] | sort_by(.created) | last | .id // empty'
}

# Does the state bucket exist? (HEAD with the Scaleway key on curl's stdin.)
state_bucket_exists() {
  local code
  code="$(printf 'user = "%s:%s"\n' "$SCW_ACCESS_KEY" "$SCW_SECRET_KEY" |
    curl -sS -o /dev/null -w '%{http_code}' -K - --aws-sigv4 "aws:amz:nl-ams:s3" -I \
      "https://s3.nl-ams.scw.cloud/ruum-staging-tfstate")"
  [[ "$code" == 200 ]]
}

# Prepare a root in a private copy of tofu/ and talos/ in the container's /tmp (so a
# local-backend override never lands in the checkout, and the patches stay at ../../talos).
# `tofu init` takes the providers from the image's mirror (tools-image.sh), checked against the
# lock file. Prints the root folder.
prepare_root() {
  local root="$1" backend="$2" dir
  mkdir -p "$TREE/tofu"
  rm -rf "${TREE:?}/tofu/${root:?}" "${TREE:?}/talos"
  cp -R "$HERE/tofu/$root" "$TREE/tofu/$root"
  cp -R "$HERE/talos" "$TREE/talos"
  dir="$TREE/tofu/$root"
  if [[ "$backend" == local ]]; then
    cat >"$dir/backend_override.tf" <<EOF
terraform {
  backend "local" {
    path = "$WORK/local-$root.tfstate"
  }
}
EOF
  fi
  (cd "$dir" && tofu init -input=false -no-color >"$WORK/init-$root.log" 2>&1) ||
    { tail -20 "$WORK/init-$root.log" >&2; die "tofu init failed for $root"; }
  printf '%s' "$dir"
}

# plan_all MODE: writes $WORK/{cluster,persistent}.tfplan (encrypted by OpenTofu), .json and .txt.
# The JSON goes through plan-scrub.jq in a pipe, so the unscrubbed JSON (the prior state with
# the Talos secrets after the first apply) never lands on disk (code review F-5).
plan_all() {
  local mode="$1" backend=s3 image cidrs dir root lock=()
  tofu_env
  # A plan only (no apply) takes no state lock, so it writes nothing to the state bucket.
  [[ "$mode" == plan ]] && lock=(-lock=false)
  if ! state_bucket_exists; then
    [[ "$mode" == plan ]] || die "the state bucket ruum-staging-tfstate does not exist; run deploy/staging/bootstrap-state.sh first (it creates it)"
    backend=local
    echo "NOTE: the state bucket ruum-staging-tfstate does not exist yet; this plan uses an empty local state (bootstrap-state.sh creates the bucket before --apply)."
  fi
  image="$(talos_image_id)"
  if [[ -z "$image" ]]; then
    [[ "$mode" == plan ]] || die "no Talos snapshot for $TALOS_VERSION"
    image="$TALOS_PLACEHOLDER"
  fi
  cidrs="$(admin_cidrs)"
  log "admin source of the firewall (Talos and Kubernetes API): $cidrs"
  export TF_VAR_talos_image="$image" TF_VAR_admin_cidrs="$cidrs"
  # check-plan.sh compares the firewall rules with this /32 (code review F-4).
  export STG_ADMIN_CIDRS="$cidrs"
  STG_BUILDBOX_ID="$(buildbox_id)"
  export STG_BUILDBOX_ID
  for root in persistent cluster; do
    dir="$(prepare_root "$root" "$backend")"
    (cd "$dir" && tofu plan -input=false -no-color -lock-timeout=60s ${lock[@]+"${lock[@]}"} -out="$WORK/$root.tfplan" >"$WORK/plan-$root.log" 2>&1) ||
      { tail -40 "$WORK/plan-$root.log" >&2; die "tofu plan failed for $root"; }
    (cd "$dir" && set -o pipefail && tofu show -json "$WORK/$root.tfplan" | jq -f "$HERE/plan-scrub.jq" >"$WORK/$root.json") ||
      { rm -f "$WORK/$root.json"; die "tofu show -json failed for $root"; }
    (cd "$dir" && tofu show -no-color "$WORK/$root.tfplan" >"$WORK/$root.txt")
  done
  hapi pricing >"$WORK/pricing.json"
}

report_plan() {
  local rc=0
  for root in persistent cluster; do
    echo "################ tofu plan: $root ################"
    cat "$WORK/$root.txt"
    echo
  done
  echo "################ check-plan ################"
  bash "$HERE/check-plan.sh" "$WORK/persistent.json" || rc=3
  bash "$HERE/check-plan.sh" "$WORK/cluster.json" || rc=3
  bash "$HERE/check-plan.sh" --repo || rc=3
  echo
  echo "################ price (EUR per month) ################"
  bash "$HERE/price.sh" "$WORK/pricing.json" "$WORK/cluster.json" "$WORK/persistent.json" || { [[ $rc -ne 0 ]] || rc=2; }
  return "$rc"
}

# Review 1 B-1: every stg-* server reports empty user data.
check_user_data() {
  local ids id ud
  ids="$(hapi "servers?label_selector=env%3Dstaging%2Crole%3Dnode" | jq -r '.servers[].id')"
  for id in $ids; do
    ud="$(HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud server describe "$id" -o json | jq -r '.user_data // empty' 2>/dev/null || true)"
    [[ -z "$ud" ]] || die "server $id has user_data; the Talos config must not be in the metadata service"
  done
  log "user_data is empty on every staging node"
}

# Review 2 R-4: the image upload leaves no temporary server or key.
check_no_image_builder() {
  local left
  left="$(hapi "servers?label_selector=apricote.de%2Fcreated-by%3Dhcloud-upload-image" | jq -r '.servers | length')"
  [[ "$left" == 0 ]] || die "hcloud-upload-image left $left temporary server(s); run: hcloud-upload-image cleanup"
}

upload_image() {
  [[ -n "$(talos_image_id)" ]] && return
  log "uploading the Talos $TALOS_VERSION snapshot (temporary $HCLOUD_SERVER_TYPE for about 10 minutes)"
  HCLOUD_TOKEN="$HCLOUD_TOKEN" hcloud-upload-image upload \
    --image-url "$TALOS_IMAGE_URL" --compression xz \
    --location "$HCLOUD_LOCATION" --server-type "$HCLOUD_SERVER_TYPE" \
    --description "talos $TALOS_VERSION staging" \
    --labels "env=staging,managed-by=up.sh,project=ruum,os=talos,version=$TALOS_VERSION,role=image"
  check_no_image_builder
  [[ -n "$(talos_image_id)" ]] || die "the snapshot upload did not produce an image"
}

write_configs() {
  local dir first
  tofu_env
  dir="$(prepare_root cluster s3)"
  (cd "$dir" && tofu output -raw talosconfig) >"$TALOSCONFIG_FILE"
  first="$(cd "$dir" && tofu output -json node_ipv4 | jq -r '.["stg-1"]')"
  talosctl --talosconfig "$TALOSCONFIG_FILE" -n "$first" -e "$first" kubeconfig --force "$KUBECONFIG_FILE" >/dev/null
  # The kubeconfig points at the private endpoint; the Mac uses the public address.
  sed -i -E "s#server: https://10\.20\.1\.[0-9]+:6443#server: https://$first:6443#" "$KUBECONFIG_FILE"
  chmod 600 "$TALOSCONFIG_FILE" "$KUBECONFIG_FILE"
  log "talosconfig and kubeconfig in $WORK (0600)"
}

k() { kubectl --kubeconfig "$KUBECONFIG_FILE" "$@"; }

CLUSTER_SECRET_NAMES="SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET STG_ZITADEL_MASTERKEY STG_PG_RUUM_PASSWORD STG_PG_ZITADEL_PASSWORD STG_PG_SPICEDB_PASSWORD STG_SPICEDB_PRESHARED_KEY STG_RUUM_TOKEN_SIGNING_KEY STG_RUUM_CONTENT_SIGNING_KEY STG_RCLONE_CRYPT_PASSWORD STG_RCLONE_CRYPT_SALT STG_NATS_RUUM_PASSWORD STG_ALLOWED_EMAILS STG_ALLOWED_UNVERIFIED_EMAILS STG_ACME_EMAIL"

case "$STEP" in
  admin-cidrs)
    admin_cidrs
    echo
    ;;
  plan)
    read_secrets $TOFU_NAMES
    plan_all plan
    report_plan
    ;;
  apply)
    require_apply
    read_secrets $TOFU_NAMES
    tofu_env
    upload_image
    plan_all apply
    report_plan || die "the plan was refused (check-plan or budget); nothing was applied"
    (cd "$TREE/tofu/persistent" && tofu apply -input=false -no-color -lock-timeout=60s "$WORK/persistent.tfplan")
    (cd "$TREE/tofu/cluster" && tofu apply -input=false -no-color -lock-timeout=60s "$WORK/cluster.tfplan")
    check_user_data
    write_configs
    ;;
  talosconfig)
    read_secrets $TOFU_NAMES
    write_configs
    ;;
  platform)
    require_apply
    read_secrets $CLUSTER_SECRET_NAMES $TOFU_NAMES
    tofu_env
    dir="$(prepare_root cluster s3)"
    generation="$(cd "$dir" && tofu output -raw pg_generation)"
    # platform [--restore-from <generation>]: bootstrap pg from an older backup path (review 2 R-2).
    restore_from=""
    if [[ "${1:-}" == --restore-from ]]; then
      restore_from="${2:-}"
      [[ "$restore_from" =~ ^[0-9]{12}$ ]] || die "--restore-from needs a generation of 12 digits (YYYYMMDDHHMM)"
    fi
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/k8s/apply.sh" "$generation" "$restore_from"
    ;;
  zitadel-bootstrap)
    read_secrets GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/ops/zitadel-bootstrap.sh" "$@"
    ;;
  check-backup)
    # Code review F-7: WAL push and a base backup into the COMPLIANCE-locked bucket work.
    read_secrets
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/ops/check-backup.sh" "$@"
    ;;
  deploy)
    read_secrets
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/ops/deploy-chart.sh" "$@"
    ;;
  status)
    read_secrets $TOFU_NAMES
    WORK="$WORK" KUBECONFIG_FILE="$KUBECONFIG_FILE" bash "$HERE/ops/status-steps.sh" "$@"
    ;;
  destroy-plan | destroy | sweep)
    read_secrets $TOFU_NAMES
    tofu_env
    STG_ADMIN_CIDRS="$(admin_cidrs)"
    STG_BUILDBOX_ID="$(buildbox_id)"
    export STG_ADMIN_CIDRS STG_BUILDBOX_ID
    # The destroy plan needs every root variable; the image does not matter for a destroy.
    TF_VAR_talos_image="$(talos_image_id)"
    export TF_VAR_admin_cidrs="$STG_ADMIN_CIDRS" TF_VAR_talos_image="${TF_VAR_talos_image:-$TALOS_PLACEHOLDER}"
    if state_bucket_exists; then
      prepare_root cluster s3 >/dev/null
      export STG_HAS_STATE=1
    else
      export STG_HAS_STATE=0
    fi
    export WORK HERE TREE
    bash "$HERE/ops/destroy-steps.sh" "$STEP" "$@"
    ;;
  *)
    die "unknown step '$STEP' (admin-cidrs, plan, apply, talosconfig, platform, zitadel-bootstrap, check-backup, deploy, status, destroy-plan, destroy, sweep)"
    ;;
esac
