#!/usr/bin/env bash
# The guard against drift between ruum-server and its staging deploy (tests/run.sh):
#
#   tests/server-env.sh <rendered charts/ruum> [<apply.sh>] [<crates dir>]
#
# 1. crates/ruum-server/env-names.txt lists exactly the names the server code reads: string
#    literals "RUUM_*", "DATABASE_URL", "SMTP_URL" and "S3_*" (as RUUM_S3_*) in crates/*/src,
#    without ruum-cli, ruum-testkit and ruum-gateway (its own binary with its own settings).
# 2. Every RUUM_* name that the ruum-server Deployment sets (env) or gets from the Secret
#    ruum-server-env (the keys that apply.sh writes) is in the list and not a test name.
# 3. Every name of class `required` is set by one of the two.
# Exit 0 when all hold, 1 otherwise (each problem on its own line).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG="$(cd "$HERE/.." && pwd)"
ROOT="$(cd "$STG/../.." && pwd)"
rendered="${1:?rendered charts/ruum (helm template output)}"
apply="${2:-$STG/k8s/apply.sh}"
crates="${3:-$ROOT/crates}"
list="$ROOT/crates/ruum-server/env-names.txt"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fail=0
problem() { printf '%s\n' "$*"; fail=1; }

grep -vE '^[[:space:]]*(#|$)' "$list" >"$TMP/list"
if awk 'NF != 2 || $2 !~ /^(required|optional|test)$/ { bad = 1 } END { exit !bad }' "$TMP/list"; then
  problem "env-names.txt: every line must be '<NAME> required|optional|test'"
fi
cut -d' ' -f1 "$TMP/list" | sort >"$TMP/listed"
dups="$(uniq -d "$TMP/listed")"
[[ -z "$dups" ]] || problem "env-names.txt lists twice: $dups"
awk '$2 != "test" { print $1 }' "$TMP/list" | sort -u >"$TMP/deployable"
awk '$2 == "required" { print $1 }' "$TMP/list" | sort -u >"$TMP/required"

# 1. The list against the code.
find "$crates" -path '*/src/*' -name '*.rs' \
  -not -path "$crates/ruum-cli/*" -not -path "$crates/ruum-testkit/*" \
  -not -path "$crates/ruum-gateway/*" -print0 |
  xargs -0 grep -hoE '"(RUUM_[A-Z0-9_]+|DATABASE_URL|SMTP_URL|S3_[A-Z0-9_]+)"' |
  tr -d '"' | sed 's/^S3_/RUUM_S3_/' | sort -u >"$TMP/code"
while read -r n; do problem "the server reads $n, but env-names.txt does not list it"; done \
  < <(comm -23 "$TMP/code" "$TMP/listed")
while read -r n; do problem "env-names.txt lists $n, but no server code reads it"; done \
  < <(comm -13 "$TMP/code" "$TMP/listed")

# 2. and 3. The deploy: the env of the ruum-server Deployment and the keys of ruum-server-env.
awk 'function out() { if (doc ~ /kind: Deployment/ && doc ~ /\n  name: ruum-server\n/) printf "%s", doc; doc = "" }
  /^---/ { out(); next } { doc = doc $0 "\n" } END { out() }' "$rendered" |
  grep -oE 'name: [A-Z][A-Z0-9_]*' | cut -d' ' -f2 | sort -u >"$TMP/env"
[[ -s "$TMP/env" ]] || problem "no env of the ruum-server Deployment in $rendered"
grep -q 'secretRef: { name: ruum-server-env }' "$rendered" ||
  problem "the ruum-server Deployment does not read the Secret ruum-server-env"
awk '/^\{$/ { block = "" } { block = block $0 "\n" } /\} \| secret ruum ruum-server-env/ { printf "%s", block }' "$apply" |
  grep -oE "(RUUM_[A-Z0-9_]+|DATABASE_URL|SMTP_URL)=" | tr -d '=' | sort -u >"$TMP/secret"
[[ -s "$TMP/secret" ]] || problem "no keys of ruum-server-env in $apply"
sort -u "$TMP/env" "$TMP/secret" >"$TMP/deployed"
while read -r n; do
  case "$n" in
    RUUM_* | DATABASE_URL | SMTP_URL) grep -qx "$n" "$TMP/deployable" ||
      problem "the deploy sets $n, but the server does not read it (or only tests do)" ;;
  esac
done <"$TMP/deployed"
while read -r n; do problem "the server needs $n in production, but the deploy does not set it"; done \
  < <(comm -23 "$TMP/required" "$TMP/deployed")

if [[ $fail == 0 ]]; then
  printf 'server env: %s names read, %s set by the deploy, all %s required ones set\n' \
    "$(wc -l <"$TMP/code" | tr -d ' ')" "$(wc -l <"$TMP/deployed" | tr -d ' ')" "$(wc -l <"$TMP/required" | tr -d ' ')"
fi
exit "$fail"
