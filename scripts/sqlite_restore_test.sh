#!/usr/bin/env bash
# The SQLite replication and restore test (docs/STEP2.md "Infrastructure work" 4, card I-3,
# docs/DEVSTACK.md "SQLite storage").
#
#   scripts/sqlite_restore_test.sh
#
# It proves, with the Litestream image and config of the stack's `litestream` service:
#   1. a tenant directory and a database file (random names, WAL mode) that appear after
#      Litestream started are replicated to the Garage bucket within 10 s;
#   2. a restore to a noted time, into a new file, finds the rows that were deleted later;
#   3. after the file is deleted locally, a restore of the latest state brings the file and
#      its rows back, Litestream keeps running, and it replicates a write to the restored file.
#
# It never changes the stack's services. It starts its own Litestream container
# litestream-<key> (key: BUILDBOX_NAME on the build server, else SQLITE_TEST_KEY, else
# "local") on the network of the stack's Garage, with its own temporary directory and its own
# prefix ruum-sqlite/<key>-<random>/ in the bucket. It creates the bucket only when it is
# missing, and allows the stack's key on it (both additive). At exit, also on failure, it
# removes the container, the temporary directory and its prefix in the bucket.
#
# Settings: the stack's S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY and S3_ENDPOINT (from
# `scripts/dev.sh env`, which this script reads when they are not set; scripts/buildbox/run.sh
# sets them); RUUM_SQLITE_BUCKET (default ruum-sqlite). Needs docker, curl (with --aws-sigv4),
# python3 and openssl on the host. SQLite itself runs in the Litestream image.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$ROOT/deploy/dev/docker-compose.yml"
CONFIG="$ROOT/deploy/dev/litestream/litestream.yml"
BOUND=10 # seconds: the replication bound of the card

log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mFAIL:\033[0m %s\n' "$*" >&2; exit 1; }

for tool in docker curl python3 openssl; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done
if [[ -z ${S3_ACCESS_KEY_ID:-} || -z ${S3_SECRET_ACCESS_KEY:-} ]]; then
  eval "$("${RUUM_DEV_SH:-$ROOT/scripts/dev.sh}" env)"
fi
[[ -n ${S3_ACCESS_KEY_ID:-} && -n ${S3_SECRET_ACCESS_KEY:-} ]] || die "S3_ACCESS_KEY_ID and S3_SECRET_ACCESS_KEY are not set"
S3_ENDPOINT=${S3_ENDPOINT:-http://127.0.0.1:3900}
BUCKET=${RUUM_SQLITE_BUCKET:-ruum-sqlite}
# The containers read the keys from the environment by name; values never go on a command line.
export AWS_ACCESS_KEY_ID=$S3_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY=$S3_SECRET_ACCESS_KEY

# The pinned image of the compose service, so the test checks what the stack runs.
IMAGE=$(awk '/image: litestream\/litestream:/ {print $2; exit}' "$COMPOSE_FILE")
[[ $IMAGE == litestream/litestream:*@sha256:* ]] || die "no litestream image pinned by version and digest in $COMPOSE_FILE"

KEY=${BUILDBOX_NAME:-${SQLITE_TEST_KEY:-local}}
[[ $KEY =~ ^[a-z0-9][a-z0-9_-]{0,47}$ ]] || die "the key must match [a-z0-9][a-z0-9_-]*"
NAME=litestream-$KEY
PREFIX=$KEY-$(openssl rand -hex 4)

# The stack's Garage (compose project "ruum") and its network.
GARAGE=$(docker ps -q --filter label=com.docker.compose.project=ruum --filter label=com.docker.compose.service=garage | head -1)
[[ -n $GARAGE ]] || die "the stack's garage is not running (scripts/dev.sh up)"
GARAGE_NAME=$(docker inspect -f '{{.Name}}' "$GARAGE")
GARAGE_NAME=${GARAGE_NAME#/}
NETWORK=$(docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}}{{"\n"}}{{end}}' "$GARAGE" | grep -m1 .) ||
  die "garage has no network"
INNER_ENDPOINT=http://$GARAGE_NAME:3900

WORK=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-restore-test.XXXXXX")
mkdir -p "$WORK/data" "$WORK/out"

# --- S3 helpers (curl signs with SigV4; the key goes to curl on stdin, not argv) ---------------
s3() { # s3 <method> <path-and-query> [curl args...]
  local method=$1 path=$2
  shift 2
  printf 'user = "%s:%s"\n' "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" |
    curl -fsS -K - --aws-sigv4 "aws:amz:garage:s3" -X "$method" "$S3_ENDPOINT/$path" "$@"
}
# Prints the keys under a prefix, one per line (at most 1000 per call).
s3_keys() {
  s3 GET "$BUCKET?list-type=2&prefix=$1" | python3 -I -c '
import sys, xml.etree.ElementTree as ET
ns = {"s3": "http://s3.amazonaws.com/doc/2006-03-01/"}
for k in ET.parse(sys.stdin).getroot().findall("s3:Contents/s3:Key", ns):
    print(k.text)'
}
s3_delete_prefix() {
  local keys key n=0
  while :; do
    keys=$(s3_keys "$1/")
    [[ -n $keys ]] || break
    while IFS= read -r key; do
      s3 DELETE "$BUCKET/$key" -o /dev/null
      n=$((n + 1))
    done <<<"$keys"
  done
  echo "$n"
}

cleanup() {
  local status=$?
  set +e
  if ((status != 0)) && docker inspect "$NAME" >/dev/null 2>&1; then
    log "litestream log (last 30 lines):"
    docker logs --tail 30 "$NAME" 2>&1 | sed 's/^/    /' >&2
  fi
  docker rm -f "$NAME" >/dev/null 2>&1
  # The containers write as root; remove the files with the same image.
  docker run --rm -v "$WORK:/w" --entrypoint rm "$IMAGE" -rf /w/data /w/out >/dev/null 2>&1
  rm -rf "$WORK"
  local n
  if n=$(s3_delete_prefix "$PREFIX"); then
    log "removed $NAME, $WORK and $n objects under s3://$BUCKET/$PREFIX/"
  else
    log "warning: could not delete s3://$BUCKET/$PREFIX/"
  fi
  if ((status == 0)); then printf '\033[1;32mPASS\033[0m sqlite_restore_test\n' >&2; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# --- SQLite and Litestream in the pinned image -------------------------------------------------
sql() { # sql <path under data/> <statements>
  docker run --rm -v "$WORK/data:/data" --entrypoint sqlite3 "$IMAGE" -bail "/data/$1" "$2"
}
# restore [-timestamp T] <tenant/db> <output under out/ or data/>
restore() {
  local args=()
  if [[ $1 == -timestamp ]]; then args=(-timestamp "$2"); shift 2; fi
  docker run --rm --network "$NETWORK" -v "$WORK:/w" -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
    "$IMAGE" restore ${args[@]+"${args[@]}"} -o "/w/$2" \
    "s3://$BUCKET/$PREFIX/$1?endpoint=$INNER_ENDPOINT&region=garage&forcePathStyle=true" >/dev/null 2>&1
}
# count <file under the work dir>: the rows of table plants, or "none"
count() {
  docker run --rm -v "$WORK:/w" --entrypoint sqlite3 "$IMAGE" -bail "/w/$1" 'select count(*) from plants' 2>/dev/null || echo none
}
# replica_count <tenant/db>: restores the latest state into a scratch file and counts the rows.
# (It runs in a subshell, so the scratch name is random, not a counter.)
replica_count() {
  local probe
  probe=out/probe-$(openssl rand -hex 6).db
  restore "$1" "$probe" && count "$probe" || echo none
}
# wait_replica <tenant/db> <rows> <what>: waits until the replica has <rows> rows, at most BOUND s.
wait_replica() {
  local start=$SECONDS got
  while :; do
    got=$(replica_count "$1")
    if [[ $got == "$2" ]]; then
      log "$3: the replica has $2 rows after $((SECONDS - start)) s (bound $BOUND s)"
      return
    fi
    ((SECONDS - start < BOUND)) || die "$3: the replica has '$got' rows after $BOUND s, want $2"
    sleep 0.5
  done
}
running() { [[ $(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null) == true ]]; }
managed() { docker exec "$NAME" litestream list -json -timeout 5; }

# --- 0. Bucket and Litestream ------------------------------------------------------------------
log "litestream $IMAGE as $NAME; garage $GARAGE_NAME on $NETWORK; prefix s3://$BUCKET/$PREFIX/"
garage() { docker exec -e RUST_LOG=warn "$GARAGE" /garage "$@"; }
if ! garage bucket info "$BUCKET" >/dev/null 2>&1; then
  log "creating the bucket $BUCKET (it was missing)"
  garage bucket create "$BUCKET" >/dev/null
fi
garage bucket allow --read --write "$BUCKET" --key "$AWS_ACCESS_KEY_ID" >/dev/null

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" --label ruum.sqlite-restore-test="$KEY" --memory 256m \
  --network "$NETWORK" -v "$WORK/data:/data" -v "$CONFIG:/etc/litestream.yml:ro" \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e "LITESTREAM_S3_ENDPOINT=$INNER_ENDPOINT" \
  -e "LITESTREAM_BUCKET=$BUCKET" -e "LITESTREAM_PATH=$PREFIX" "$IMAGE" replicate >/dev/null
for _ in $(seq 1 40); do
  managed >/dev/null 2>&1 && break
  sleep 0.25
done
managed >/dev/null 2>&1 || die "$NAME does not answer on its control socket"

# --- 1. A new tenant directory and a new database file are replicated ------------------------
tenant=$(openssl rand -hex 8)
db=$(openssl rand -hex 8).db
rel=$tenant/$db
mkdir "$WORK/data/$tenant"
mode=$(sql "$rel" "pragma journal_mode=wal;
  create table plants (id integer primary key, name text not null);
  insert into plants (name) values ('fern'), ('moss'), ('ivy'), ('oak'), ('reed');")
[[ $mode == wal ]] || die "journal_mode is '$mode', want wal"
wait_replica "$rel" 5 "new file $rel"
managed | grep -q "\"/data/$rel\"" || die "litestream does not list /data/$rel"

# --- 2. Restore to a time ----------------------------------------------------------------------
# floor(now + 1 s) is later than every write above; the delete below is later than at.
sleep 1
at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 1.5
sql "$rel" "delete from plants where id <= 3;" >/dev/null
wait_replica "$rel" 2 "delete of 3 rows"
restore -timestamp "$at" "$rel" out/at-time.db || die "restore to $at failed"
got=$(count out/at-time.db)
[[ $got == 5 ]] || die "restore to $at: $got rows, want 5"
log "restore to $at into a new file: 5 rows (the latest state has 2)"

# --- 3. Delete the file locally, restore the latest state ------------------------------------
# The order matters (see docs/DEVSTACK.md, notes for E-3): delete the .db file first, wait until
# Litestream has dropped it, then delete -wal and -shm. Litestream reads the WAL when it closes
# the file; without it the close fails and the path is never replicated again until a restart.
docker run --rm -v "$WORK/data:/data" --entrypoint rm "$IMAGE" -f "/data/$rel"
for _ in $(seq 1 40); do
  managed | grep -q "\"/data/$rel\"" || break
  sleep 0.25
done
! managed | grep -q "\"/data/$rel\"" || die "litestream still lists /data/$rel 10 s after the delete"
docker run --rm -v "$WORK/data:/data" --entrypoint rm "$IMAGE" -f "/data/$rel-wal" "/data/$rel-shm"
[[ ! -e $WORK/data/$rel && ! -e $WORK/data/$rel-wal ]] || die "$rel is still there after the delete"
running || die "litestream stopped after the file was deleted"
log "deleted $rel locally; litestream keeps running"
restore "$rel" "data/$rel" || die "restore of the latest state failed"
got=$(count "data/$rel")
[[ $got == 2 ]] || die "restored $rel: $got rows, want 2"
check=$(sql "$rel" 'pragma integrity_check;')
[[ $check == ok ]] || die "restored $rel: integrity_check says $check"
log "restored the latest state to $rel: 2 rows, integrity ok"
# Litestream picks the restored file up again, and a new write reaches the replica.
sql "$rel" "insert into plants (name) values ('sedge');" >/dev/null
wait_replica "$rel" 3 "write after the restore"
running || die "litestream stopped after the restore"
