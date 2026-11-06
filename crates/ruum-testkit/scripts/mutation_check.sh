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
#
# Environment:
#   MUTATION_STRICT=1      survived and skipped mutations fail the run (use when step 1 is built)
#   MUTATION_TARGET_DIR    the cargo target dir for the copies (default: <repo>/target/mutation);
#                          the copy itself is in <MUTATION_TARGET_DIR>.work/repo
#   RUUM_REQUIRE_IMPLEMENTED is passed through to the suites.
#   DATABASE_URL           needed by the mutations whose ENV picks the Postgres backend
#                          (eval "$(scripts/dev.sh env)" on the laptop).
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

run_tests() {
  # $1: cargo test arguments, $2: extra environment (NAME=value ...). Output goes to $work/log.
  # shellcheck disable=SC2086
  (cd "$copy" && env $2 cargo test $1 >"$work/log" 2>&1)
}

if [ "$#" -gt 0 ]; then
  files=()
  for n in "$@"; do files+=("$mutations_dir/$n.sh"); done
else
  files=("$mutations_dir"/*.sh)
fi

echo "mutation check: ${#files[@]} mutations, strict=$strict, copy in $copy"
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
baseline_tests="$(for f in "${files[@]}"; do (ENV=""; source "$f"; echo "$ENV|$TESTS"); done | sort -u)"
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
