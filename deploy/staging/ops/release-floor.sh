# The oldest release that deploy.sh can deploy (review of backlog O-25, required change 1).
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
