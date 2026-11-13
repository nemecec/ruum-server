# The oldest release that deploy.sh can deploy (review of backlog O-25, required change 1), and
# the rollback rule (Kimi review of O-25, finding 2).
# Sourced by deploy.sh; tests/run.sh tests it against a small git repository.
#
# Since O-25 every deploy runs `ruum-server migrate` in a Job with the target's server image, and
# the edge's Caddyfile asks for the storage module `kubernetes_secret`. A commit before O-25 has
# neither: its server image answers `migrate` with a usage error (the Job fails and nothing rolls
# out), and its edge image cannot load the Caddyfile. So `deploy.sh --rollback` cannot go back
# past O-25. The check looks at files of the commit, not at a commit hash, so it holds however
# the card was merged.
# shellcheck shell=bash

RUUM_RELEASE_FLOOR_FILES=(
  crates/ruum-server/src/migrate.rs
  deploy/images/caddy-secret-storage/storage.go
)

# ruum_release_floor_ok REPO SHA: 0 when SHA has the migration step and the edge storage module.
ruum_release_floor_ok() {
  local repo="$1" sha="$2" f
  for f in "${RUUM_RELEASE_FLOOR_FILES[@]}"; do
    git -C "$repo" cat-file -e "$sha:$f" 2>/dev/null || return 1
  done
}

# The rollback rule (design 12.7; Kimi review of O-25, finding 2). The schema rule promises only
# that the previous release runs on the new schema. A release two or more back may read a column
# that a later contract step dropped, so a plain --rollback goes only to the release that was
# live before the current one (`previous` in the ConfigMap ruum-release). An older target needs
# --rollback-older, and deploy.sh and deploy-chart.sh print a warning for it.
#
# ruum_rollback_target_ok TARGET PREVIOUS OLDER: 0 when a rollback to TARGET may go ahead.
# PREVIOUS is the recorded previous release (may be empty); OLDER is 1 for --rollback-older.
ruum_rollback_target_ok() {
  local target="$1" previous="$2" older="${3:-0}"
  [[ -n "$previous" && "$target" == "$previous" ]] && return 0
  [[ "$older" == 1 ]]
}

# shellcheck disable=SC2034 # used by deploy.sh and deploy-chart.sh, which source this file
RUUM_ROLLBACK_OLDER_WARNING="WARNING: --rollback-older goes back past the previous release. Design 12.7 only promises that the previous release runs on the current schema. Check every migration since the target for a contract step (a dropped or renamed column or table) that the target still uses, and have the backup ready."
