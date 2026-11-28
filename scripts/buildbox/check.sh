#!/usr/bin/env bash
# The full check of one tree on the build server, for integrators and cards (backlog B-63).
#
#   scripts/buildbox/run.sh <name> <worktree> -- scripts/buildbox/check.sh
#   scripts/buildbox/run.sh <name> <worktree> -- env E2E=both scripts/buildbox/check.sh
#
# Steps: fmt, clippy, the workspace without ruum-dbengine and ruum-core, then ruum-dbengine and
# ruum-core apart with 4 test threads, the test kit on memory and on Postgres, then the e2e
# scripts. Each step prints "=== STEP <name>" and "=== RESULT <status>"; the last line is
# "=== ALL <status>" and the exit status is 0 only when every step passed. Grep the log for
# "=== RESULT [1-9]"; never pipe this script into tail or grep in a gate (backlog M-96).
#
# Why the two crates run apart: their data cluster tests open many databases at once, and at the
# default thread count (one per core, 16 on the build server) the agent's data cluster (768 MB)
# is OOM-killed: "the database system is in recovery mode", "expected to read 5 bytes"
# (backlog B-29, B-35, B-63). With 4 threads both pass.
#
# Settings:
#   E2E=browser (default), smoke, both or none: the e2e scripts to run.
#   ONLY_E2E=1: only the e2e scripts.
#   ONLY_DATA=1: only ruum-api, ruum-dbengine and ruum-core (the crates with data cluster tests).
#   DATA_TEST_THREADS (default 4): the test threads of ruum-dbengine and ruum-core.
#   CARGO_BUILD_JOBS (default 8): use 4 when other agents build too (the linker is OOM-killed,
#   backlog B-58).
set -uo pipefail
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-8} CARGO_PROFILE_DEV_DEBUG=line-tables-only RUUM_REQUIRE_IMPLEMENTED=1
threads=${DATA_TEST_THREADS:-4}
st=0
step() {
  echo "=== STEP $1"
  shift
  "$@"
  local r=$?
  echo "=== RESULT $r"
  [ $r -ne 0 ] && st=1
  return 0
}
data() {
  step dbengine cargo test -p ruum-dbengine --locked --no-fail-fast -- --test-threads="$threads"
  step core cargo test -p ruum-core --locked --no-fail-fast -- --test-threads="$threads"
}
if [ -n "${ONLY_DATA:-}" ]; then
  step api cargo test -p ruum-api --locked --no-fail-fast
  data
  echo "=== ALL $st"
  exit $st
fi
if [ "${ONLY_E2E:-0}" != 1 ]; then
  step fmt cargo fmt --all -- --check
  step clippy cargo clippy --workspace --all-targets --locked -- -D warnings
  step workspace cargo test --workspace --exclude ruum-dbengine --exclude ruum-core --locked --no-fail-fast
  data
  step testkit-memory cargo test -p ruum-testkit --locked --no-fail-fast -- --nocapture
  step testkit-postgres env RUUM_SUITE_BACKEND=postgres cargo test -p ruum-testkit --locked --no-fail-fast -- --nocapture
fi
case "${E2E:-browser}" in
  browser) step e2e-browser scripts/e2e_browser.sh ;;
  smoke) step e2e-smoke scripts/e2e_smoke.sh ;;
  both)
    step e2e-browser scripts/e2e_browser.sh
    step e2e-smoke scripts/e2e_smoke.sh
    ;;
  none) ;;
  *)
    echo "E2E must be browser, smoke, both or none" >&2
    st=1
    ;;
esac
echo "=== ALL $st"
exit $st
