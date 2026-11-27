#!/usr/bin/env bash
# Mutation check of the security suites (docs/DESIGN.md, section 18.2).
#
# For each file in crates/ruum-testkit/mutations/, the script breaks one security rule on purpose
# in a temporary copy of the repository and runs the suites named by the mutation. The suites
# must fail ("killed"). A mutation that the suites do not notice ("survived") is a hole in the
# suites.
#
# Usage:
#   crates/ruum-testkit/scripts/mutation_check.sh            # all mutations
#   crates/ruum-testkit/scripts/mutation_check.sh NAME...    # some, by file name without .sh
#   crates/ruum-testkit/scripts/mutation_check.sh --shards   # print the shard names as JSON
#   crates/ruum-testkit/scripts/mutation_check.sh --list     # print the names of MUTATION_SHARD
#
# Environment:
#   MUTATION_STRICT=1      survived and skipped mutations fail the run (use when step 1 is built)
#   MUTATION_SHARD         run only the mutations of this shard (see `shards` below; backlog T-82:
#                          the whole check takes about 10 hours, more than one GitHub job may
#                          run). Ignored when names are given.
#   MUTATION_TARGET_DIR    the cargo target dir for the copies (default: <repo>/target/mutation);
#                          the copy itself is in <MUTATION_TARGET_DIR>.work/repo
#   RUUM_REQUIRE_IMPLEMENTED is passed through to the suites.
#   DATABASE_URL           needed by the mutations whose ENV picks the Postgres backend
#                          (eval "$(scripts/dev.sh env)" on the laptop).
#   CI                     when set (also when empty), the case filters that the tests refuse under CI (T-26:
#                          RUUM_BUILD_CASES, RUUM_SANDBOX_CASES) are dropped from ENV, so the
#                          baseline and the mutation run the whole test (backlog T-79).
#
# A mutation file sets DESCRIPTION, FILE, PERL, TESTS, optionally PLACEHOLDER=1 and ENV: extra
# environment variables (NAME=value, separated by spaces) for its cargo test runs, for example
# ENV="RUUM_SUITE_BACKEND=postgres RUUM_SUITE_RLS=off".
#
# Results: KILLED (good), SURVIVED, SKIP (placeholder: the file or pattern does not exist yet),
# BROKEN (the mutated code does not compile: fix the mutation). Exit status 0 when no mutation
# fails under the current mode.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
mutations_dir="$repo/crates/ruum-testkit/mutations"
strict="${MUTATION_STRICT:-0}"
export CARGO_TARGET_DIR="${MUTATION_TARGET_DIR:-$repo/target/mutation}"

# The copy has a fixed path next to its target dir, so cargo can reuse earlier builds of it.
work="$CARGO_TARGET_DIR.work"
copy="$work/repo"
mkdir -p "$work"

# Without -t and with --checksum, rsync rewrites only files whose content differs, and they get
# a new mtime. So cargo rebuilds exactly what a mutation (or a change in the repo) touched.
sync_copy() {
  rsync -rlp --checksum --delete --exclude /target --exclude node_modules --exclude /.git \
    --exclude /web/dist "$repo/" "$copy/"
}

# The case filters that a test refuses when CI is set (backlog T-26): a filter skips cases, so CI
# must run them all. Under CI the script drops them from a mutation's ENV and runs the whole test,
# which holds every case of the filtered run (backlog T-79). Without CI they stay (the build
# server's strict check, the laptop).
ci_refused_filters="RUUM_BUILD_CASES RUUM_SANDBOX_CASES"

# The shards of the nightly check (backlog T-82). A mutation belongs to the first shard whose list
# holds the part of its name before the first "-"; the shard "rest" takes every other mutation, so
# a mutation with a new prefix still runs. Each shard runs its own baselines, so a prefix whose
# mutations share test runs stays in one shard. Times of one GitHub runner (run 45703034717):
# databases about 2 h, apps 1.5 h, dbengine 1 h; each shard should stay under 3 hours.
# "runtime" holds the Docker driver, build and sandbox mutations, alone in their shard.
shards="
databases=databases
apps=apps
dbengine=dbengine gateway egress
runtime=runtime
telemetry=telemetry
core=privacy feed authz agents email csrf
rest=
"

shard_of() {
  # $1: a mutation name. Prints its shard.
  local prefix="${1%%-*}" line name list
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    name="${line%%=*}"
    list="${line#*=}"
    case " $list " in
      *" $prefix "*) printf '%s' "$name"; return ;;
    esac
  done <<<"$shards"
  printf 'rest'
}

shard_names() {
  local line
  while IFS= read -r line; do
    [ -n "$line" ] && printf '%s\n' "${line%%=*}"
  done <<<"$shards"
}

effective_env() {
  # $1: a mutation's ENV. Prints the ENV the cargo test runs get.
  # ${CI+x}: CI set but empty counts as set, as the tests read it (var_os; backlog T-83).
  if [ -z "${CI+x}" ]; then
    printf '%s' "$1"
    return
  fi
  local out="" word name
  for word in $1; do
    name="${word%%=*}"
    case " $ci_refused_filters " in
      *" $name "*) ;;
      *) out="${out:+$out }$word" ;;
    esac
  done
  printf '%s' "$out"
}

run_tests() {
  # $1: cargo test arguments, $2: extra environment (NAME=value ...). Output goes to $work/log.
  # shellcheck disable=SC2086
  (cd "$copy" && env $2 cargo test $1 >"$work/log" 2>&1)
}

shard="${MUTATION_SHARD:-}"
if [ "${1:-}" = "--shards" ]; then
  # One JSON array, for a job matrix.
  shard_names | awk 'BEGIN { printf "[" } { printf "%s\"%s\"", (NR > 1 ? "," : ""), $0 } END { print "]" }'
  exit 0
fi
# No grep -q here: it ends early, and under pipefail the closed pipe would fail the test.
if [ -n "$shard" ] && ! printf '%s\n' "$(shard_names)" | grep -x -- "$shard" >/dev/null; then
  echo "MUTATION_SHARD=$shard is not a shard; the shards are: $(shard_names | tr '\n' ' ')" >&2
  exit 2
fi

if [ "${1:-}" != "--list" ] && [ "$#" -gt 0 ]; then
  files=()
  for n in "$@"; do files+=("$mutations_dir/$n.sh"); done
else
  files=()
  for f in "$mutations_dir"/*.sh; do
    if [ -z "$shard" ] || [ "$(shard_of "$(basename "$f" .sh)")" = "$shard" ]; then
      files+=("$f")
    fi
  done
fi
if [ "${1:-}" = "--list" ]; then
  for f in "${files[@]}"; do basename "$f" .sh; done
  exit 0
fi
if [ "${#files[@]}" -eq 0 ]; then
  echo "FAIL: no mutation to run${shard:+ in shard $shard}" >&2
  exit 2
fi

echo "mutation check: ${#files[@]} mutations${shard:+ of shard $shard}, strict=$strict, copy in $copy"
# A fresh copy: every file gets a new mtime, so no build of an earlier run is taken as fresh.
rm -rf "$copy"
sync_copy
# The copy and the repository may share one target dir (MUTATION_TARGET_DIR=<repo>/target), and
# their crates then share build units. At the end, put the clean files back into the copy: they
# get a new mtime, so cargo rebuilds a unit that was built from a mutated file and no later
# build in the repository reuses it.
trap 'sync_copy || true' EXIT

# The suites must pass on the clean copy, or a failure proves nothing.
# Each line is "ENV|TESTS".
# Under CI the refused case filters are dropped first (effective_env), so the filtered runs of
# one test become one baseline run of the whole test.
if [ -n "${CI+x}" ]; then
  echo "CI is set: the case filters $ci_refused_filters are dropped from ENV; those mutations run the whole test (T-26, T-79)"
fi
# shellcheck disable=SC1090
baseline_tests="$(for f in "${files[@]}"; do (ENV=""; source "$f"; echo "$(effective_env "$ENV")|$TESTS"); done | sort -u)"
while IFS= read -r line; do
  [ -z "$line" ] && continue
  e="${line%%|*}"
  t="${line#*|}"
  echo "baseline: ${e:+$e }cargo test $t"
  if ! run_tests "$t" "$e"; then
    tail -50 "$work/log"
    echo "FAIL: the suites fail without a mutation; fix them first" >&2
    exit 2
  fi
done <<<"$baseline_tests"

failed=0
summary=()
for f in "${files[@]}"; do
  name="$(basename "$f" .sh)"
  DESCRIPTION="" FILE="" PERL="" TESTS="" PLACEHOLDER=0 ENV=""
  # shellcheck disable=SC1090
  source "$f"
  ENV="$(effective_env "$ENV")"
  sync_copy
  target="$copy/$FILE"
  result=""
  if [ ! -f "$target" ]; then
    result="SKIP"
    why="$FILE does not exist yet"
  else
    cp "$target" "$work/before"
    perl -0pi -e "$PERL" "$target"
    if cmp -s "$target" "$work/before"; then
      result="SKIP"
      why="the pattern does not match $FILE"
    elif ! (cd "$copy" && cargo test --no-run $TESTS >"$work/log" 2>&1); then
      result="BROKEN"
      why="the mutated code does not compile"
      tail -20 "$work/log"
    elif run_tests "$TESTS" "$ENV"; then
      result="SURVIVED"
      why="${ENV:+$ENV }cargo test $TESTS passed with the mutation"
    else
      result="KILLED"
      why="${ENV:+$ENV }cargo test $TESTS failed, as it must"
    fi
  fi
  case "$result" in
    KILLED) ;;
    BROKEN) failed=1 ;;
    SKIP)
      # A placeholder may wait for its code; a real mutation whose target is gone is stale.
      if [ "$strict" = "1" ] || [ "$PLACEHOLDER" != "1" ]; then failed=1; fi ;;
    SURVIVED) if [ "$strict" = "1" ]; then failed=1; fi ;;
  esac
  line="$(printf '%-9s %-32s %s (%s)' "$result" "$name" "$DESCRIPTION" "$why")"
  echo "$line"
  summary+=("$line")
done

echo
echo "=== mutation check summary (strict=$strict) ==="
printf '%s\n' "${summary[@]}"
if [ "$strict" != "1" ]; then
  echo "Not strict: SURVIVED and placeholder SKIP results are warnings while handlers answer 501."
fi
exit "$failed"
