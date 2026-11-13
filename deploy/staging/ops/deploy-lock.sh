# The deploy lock of staging (Kimi review of O-25, finding 3). Sourced by deploy-chart.sh;
# tests/run.sh tests it with a fake kubectl.
#
# Two deploys at the same time could both read the same live release, and the older one could
# then roll out after the newer one, without --rollback. So deploy-chart.sh holds a Lease
# ruum/ruum-deploy from before it reads the live release until helm upgrade has finished: the
# read, the checks, the backup, the migration step and the rollout. A second deploy refuses
# while the Lease is held and not expired.
#
# - Taking the lock: `kubectl create` of the Lease (the API server refuses a second create), or,
#   when the Lease exists but is free (released, or its holder did not renew in time), a
#   `kubectl replace` with the resourceVersion that was read, so two deploys cannot both take it.
# - Renewing between the steps: the same compare-and-swap. A deploy that lost the lock (it
#   expired and another deploy took it) stops before its next step.
# - Releasing: a replace that clears the holder (no delete, so a late release never removes a
#   lock that another deploy holds).
# Limits: expiry uses the clock of the tools container; the steps are not fenced inside (a step
# that runs longer than the Lease could overlap with a deploy that took over). Each step is
# bounded below RUUM_DEPLOY_LOCK_SECONDS (backup 20 min, migration step 17 min, helm 10 min).
# shellcheck shell=bash

RUUM_DEPLOY_LOCK_NAME=ruum-deploy
RUUM_DEPLOY_LOCK_NS=ruum
RUUM_DEPLOY_LOCK_SECONDS="${RUUM_DEPLOY_LOCK_SECONDS:-1800}"

ruum_lock_now() { date -u +%Y-%m-%dT%H:%M:%S.000000Z; }

# ruum_lock_doc HOLDER ACQUIRED SECONDS [RESOURCE_VERSION]: the Lease as JSON.
ruum_lock_doc() {
  jq -n --arg name "$RUUM_DEPLOY_LOCK_NAME" --arg ns "$RUUM_DEPLOY_LOCK_NS" --arg holder "$1" \
    --arg acquired "$2" --arg renewed "$(ruum_lock_now)" --argjson seconds "$3" --arg rv "${4:-}" '
    { apiVersion: "coordination.k8s.io/v1", kind: "Lease",
      metadata: ({ name: $name, namespace: $ns } + (if $rv != "" then { resourceVersion: $rv } else {} end)),
      spec: { holderIdentity: $holder, leaseDurationSeconds: $seconds, acquireTime: $acquired, renewTime: $renewed } }'
}

# ruum_lock_read: the Lease as JSON on stdout, or exit 1.
ruum_lock_read() { kubectl -n "$RUUM_DEPLOY_LOCK_NS" get lease "$RUUM_DEPLOY_LOCK_NAME" -o json 2>/dev/null; }

# ruum_lock_expiry JSON: the Unix time when the Lease ends (0 without a holder).
ruum_lock_expiry() {
  jq -r 'if (.spec.holderIdentity // "") == "" then 0 else
    ((.spec.renewTime // .spec.acquireTime // "1970-01-01T00:00:00Z") | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601)
    + (.spec.leaseDurationSeconds // 0) end' <<<"$1"
}

# ruum_lock_acquire HOLDER: 0 when HOLDER now holds the lock; 1 when another deploy holds it
# (the holder and the end are printed); 2 on another error.
ruum_lock_acquire() {
  local holder="$1" err cur rv until
  if err="$(ruum_lock_doc "$holder" "$(ruum_lock_now)" "$RUUM_DEPLOY_LOCK_SECONDS" | kubectl -n "$RUUM_DEPLOY_LOCK_NS" create -f - 2>&1 >/dev/null)"; then
    return 0
  fi
  [[ "$err" == *AlreadyExists* || "$err" == *"already exists"* ]] || { echo "deploy lock: $err" >&2; return 2; }
  cur="$(ruum_lock_read)" || { echo "deploy lock: cannot read lease $RUUM_DEPLOY_LOCK_NS/$RUUM_DEPLOY_LOCK_NAME" >&2; return 2; }
  until="$(ruum_lock_expiry "$cur")"
  if (( $(date -u +%s) < until )); then
    echo "deploy lock: another deploy holds lease $RUUM_DEPLOY_LOCK_NS/$RUUM_DEPLOY_LOCK_NAME: $(jq -r '.spec.holderIdentity' <<<"$cur"), until $(jq -rn --argjson t "$until" '$t | todate')" >&2
    return 1
  fi
  rv="$(jq -r '.metadata.resourceVersion' <<<"$cur")"
  if err="$(ruum_lock_doc "$holder" "$(ruum_lock_now)" "$RUUM_DEPLOY_LOCK_SECONDS" "$rv" | kubectl -n "$RUUM_DEPLOY_LOCK_NS" replace -f - 2>&1 >/dev/null)"; then
    return 0
  fi
  echo "deploy lock: another deploy took the lock first ($err)" >&2
  return 1
}

# ruum_lock_renew HOLDER: 0 when HOLDER still holds the lock and renewed it; 1 when it lost it.
ruum_lock_renew() {
  local holder="$1" cur rv acquired
  cur="$(ruum_lock_read)" || { echo "deploy lock: the lease is gone" >&2; return 1; }
  [[ "$(jq -r '.spec.holderIdentity // ""' <<<"$cur")" == "$holder" ]] ||
    { echo "deploy lock: lost to $(jq -r '.spec.holderIdentity // "nobody"' <<<"$cur")" >&2; return 1; }
  rv="$(jq -r '.metadata.resourceVersion' <<<"$cur")"
  acquired="$(jq -r '.spec.acquireTime // ""' <<<"$cur")"
  ruum_lock_doc "$holder" "${acquired:-$(ruum_lock_now)}" "$RUUM_DEPLOY_LOCK_SECONDS" "$rv" |
    kubectl -n "$RUUM_DEPLOY_LOCK_NS" replace -f - >/dev/null 2>&1 ||
    { echo "deploy lock: lost while renewing (the lease changed)" >&2; return 1; }
}

# ruum_lock_release HOLDER: clears the holder when HOLDER still holds the lock; else does nothing.
ruum_lock_release() {
  local holder="$1" cur rv
  cur="$(ruum_lock_read)" || return 0
  [[ "$(jq -r '.spec.holderIdentity // ""' <<<"$cur")" == "$holder" ]] || return 0
  rv="$(jq -r '.metadata.resourceVersion' <<<"$cur")"
  ruum_lock_doc "" "$(ruum_lock_now)" 1 "$rv" | kubectl -n "$RUUM_DEPLOY_LOCK_NS" replace -f - >/dev/null 2>&1 || true
}
