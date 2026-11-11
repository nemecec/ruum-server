#!/usr/bin/env bash
# The build-server half of the staging scripts. The laptop scripts (up.sh, deploy.sh, status.sh,
# destroy.sh) call it over ssh and send the secrets on stdin as NAME=value lines that end with
# a line "__END__". Only the names of the step's allow-list are taken; nothing is printed.
#
# Steps (read-only unless marked rw):
#   plan                 tofu plan of both roots, check-plan, price
#   apply          (rw)  image upload if missing, plan, check-plan, price, apply the exact plan,
#                        user_data check, kubeconfig
#   talosconfig    (rw)  write the talosconfig and kubeconfig from the state (0600, ruum-ops)
#   platform       (rw)  helmfile apply of the platform (needs the cluster secrets on stdin)
#   zitadel-bootstrap (rw) OIDC app, Google and Microsoft, login policy; `check` reports drift
#   deploy <server-ref> <edge-ref> [--rollback] (rw)  helm upgrade of charts/ruum by digest
#   status               what exists: state, hcloud by label, nodes, pods, backups
#   destroy-plan         three lists: removed, kept on purpose, never touched; state vs labels
#   destroy        (rw)  tofu destroy of the cluster root, snapshot, checks
#   sweep          (rw)  delete by label when the state is lost (review 2 D-2)
#
# Read-only steps run in "direct" mode too (as user ruum); rw steps refuse it and must run as
# ruum-ops from /opt/ruum-staging/current (lib.sh).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=versions.env
. "$HERE/versions.env"
export PATH="$HOME/.local/bin:$PATH"

STEP="${1:-}"
shift || true

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# The work folder: plans, kubeconfig, talosconfig. 0700, in the home of the user that runs.
WORK="${RUUM_STAGING_WORK:-$HOME/.ruum-staging}"
umask 077
mkdir -p "$WORK"
chmod 700 "$WORK"
KUBECONFIG_FILE="$WORK/kubeconfig"
TALOSCONFIG_FILE="$WORK/talosconfig"

is_ops() { [[ "$(id -un)" == ruum-ops && "$HERE" == /opt/ruum-staging/* ]]; }
require_ops() {
  is_ops || die "step '$STEP' changes staging and runs only as ruum-ops from /opt/ruum-staging (ops/install-ops-user.sh); this is $(id -un) in $HERE"
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
}

# GET on the Hetzner API; the token goes to curl on stdin (-K -), never on the command line.
hapi() {
  printf 'header = "Authorization: Bearer %s"\n' "$HCLOUD_TOKEN" |
    curl -fsS -K - "https://api.hetzner.cloud/v1/$1"
}

# The build server's own addresses: the only admin source of the firewall (review 2 O-3).
admin_cidrs() {
  local v4 v6
  v4="$(hapi "servers?name=ruum-buildbox" | jq -r '.servers[0].public_net.ipv4.ip // empty')"
  v6="$(hapi "servers?name=ruum-buildbox" | jq -r '.servers[0].public_net.ipv6.ip // empty')"
  [[ -n "$v4" ]] || die "cannot find the build server ruum-buildbox in the Hetzner project"
  if [[ -n "$v6" ]]; then
    printf '["%s/32","%s"]' "$v4" "$v6"
  else
    printf '["%s/32"]' "$v4"
  fi
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

# Prepare a root in a private copy of tofu/ and talos/ (so a local-backend override never lands
# in the checkout, and the patches stay at ../../talos). Prints the root folder.
prepare_root() {
  local root="$1" backend="$2" tree="$WORK/tree" dir
  mkdir -p "$tree/tofu" "$WORK/plugin-cache"
  rm -rf "${tree:?}/tofu/${root:?}" "${tree:?}/talos"
  cp -R "$HERE/tofu/$root" "$tree/tofu/$root"
  cp -R "$HERE/talos" "$tree/talos"
  dir="$tree/tofu/$root"
  if [[ "$backend" == local ]]; then
    cat >"$dir/backend_override.tf" <<EOF
terraform {
  backend "local" {
    path = "$WORK/local-$root.tfstate"
  }
}
EOF
  fi
  (cd "$dir" && TF_PLUGIN_CACHE_DIR="$WORK/plugin-cache" tofu init -input=false -no-color >"$WORK/init-$root.log" 2>&1) ||
    { tail -20 "$WORK/init-$root.log" >&2; die "tofu init failed for $root"; }
  printf '%s' "$dir"
}

# plan_all MODE: writes $WORK/{cluster,persistent}.tfplan, .json (no variables), .txt.
plan_all() {
  local mode="$1" backend=s3 image cidrs dir root
  tofu_env
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
  export TF_VAR_talos_image="$image" TF_VAR_admin_cidrs="$cidrs"
  for root in persistent cluster; do
    dir="$(prepare_root "$root" "$backend")"
    (cd "$dir" && tofu plan -input=false -no-color -lock-timeout=60s -out="$WORK/$root.tfplan" >"$WORK/plan-$root.log" 2>&1) ||
      { tail -40 "$WORK/plan-$root.log" >&2; die "tofu plan failed for $root"; }
    (cd "$dir" && tofu show -json "$WORK/$root.tfplan" | jq 'del(.variables)' >"$WORK/$root.json")
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
    --image-url "$TALOS_IMAGE_URL" --compression xz --architecture x86 \
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
  # The kubeconfig points at the private endpoint; the build server uses the public address.
  sed -i -E "s#server: https://10\.20\.1\.[0-9]+:6443#server: https://$first:6443#" "$KUBECONFIG_FILE"
  chmod 600 "$TALOSCONFIG_FILE" "$KUBECONFIG_FILE"
  log "talosconfig and kubeconfig in $WORK (0600)"
}

k() { kubectl --kubeconfig "$KUBECONFIG_FILE" "$@"; }

CLUSTER_SECRET_NAMES="SCW_BACKUP_ACCESS_KEY SCW_BACKUP_SECRET_KEY HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET STG_ZITADEL_MASTERKEY STG_PG_RUUM_PASSWORD STG_PG_ZITADEL_PASSWORD STG_PG_SPICEDB_PASSWORD STG_SPICEDB_PRESHARED_KEY STG_RUUM_TOKEN_SIGNING_KEY STG_RUUM_CONTENT_SIGNING_KEY STG_RCLONE_CRYPT_PASSWORD STG_RCLONE_CRYPT_SALT STG_NATS_RUUM_PASSWORD STG_ALLOWED_EMAILS STG_ALLOWED_UNVERIFIED_EMAILS STG_ACME_EMAIL"

case "$STEP" in
  plan)
    read_secrets $TOFU_NAMES
    plan_all plan
    report_plan
    ;;
  apply)
    require_ops
    read_secrets $TOFU_NAMES
    tofu_env
    upload_image
    plan_all apply
    report_plan || die "the plan was refused (check-plan or budget); nothing was applied"
    (cd "$WORK/tree/tofu/persistent" && tofu apply -input=false -no-color -lock-timeout=60s "$WORK/persistent.tfplan")
    (cd "$WORK/tree/tofu/cluster" && tofu apply -input=false -no-color -lock-timeout=60s "$WORK/cluster.tfplan")
    check_user_data
    write_configs
    ;;
  talosconfig)
    require_ops
    read_secrets $TOFU_NAMES
    write_configs
    ;;
  platform)
    require_ops
    read_secrets $CLUSTER_SECRET_NAMES $TOFU_NAMES
    tofu_env
    dir="$(prepare_root cluster s3)"
    generation="$(cd "$dir" && tofu output -raw pg_generation)"
    # platform [--restore-from <generation>]: bootstrap pg from an older backup path (review 2 R-2).
    restore_from=""
    [[ "${1:-}" == --restore-from ]] && restore_from="${2:?--restore-from needs a generation}"
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/k8s/apply.sh" "$generation" "$restore_from"
    ;;
  zitadel-bootstrap)
    require_ops
    read_secrets GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET MS_OAUTH_CLIENT_ID MS_OAUTH_CLIENT_SECRET
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/ops/zitadel-bootstrap.sh" "$@"
    ;;
  deploy)
    require_ops
    read_secrets
    KUBECONFIG="$KUBECONFIG_FILE" bash "$HERE/ops/deploy-chart.sh" "$@"
    ;;
  status)
    read_secrets $TOFU_NAMES
    WORK="$WORK" KUBECONFIG_FILE="$KUBECONFIG_FILE" bash "$HERE/ops/status-remote.sh" "$@"
    ;;
  destroy-plan | destroy | sweep)
    [[ "$STEP" == destroy-plan ]] || require_ops
    read_secrets $TOFU_NAMES
    tofu_env
    if state_bucket_exists; then
      prepare_root cluster s3 >/dev/null
      export STG_HAS_STATE=1
    else
      export STG_HAS_STATE=0
    fi
    export WORK HERE
    bash "$HERE/ops/destroy-remote.sh" "$STEP" "$@"
    ;;
  *)
    die "unknown step '$STEP' (plan, apply, talosconfig, platform, zitadel-bootstrap, deploy, status, destroy-plan, destroy, sweep)"
    ;;
esac
