#!/usr/bin/env bash
# Run a command for one agent on the Ruum build server (scripts/buildbox/README.md).
#
#   scripts/buildbox/run.sh <name> <worktree> -- <command...>
#   scripts/buildbox/run.sh <name> --stop
#
# The first form copies the worktree (tracked and untracked files, with uncommitted changes; not
# ignored files, target*/, node_modules/ or .git) to ~/w/<name> on the server with rsync, deletes
# files there that are gone here, and runs the command in that folder. The command gets:
#   - the settings of the dev stack (`scripts/dev.sh env` of the base copy ~/ruum-server);
#   - CARGO_TARGET_DIR=~/targets/<name> and CARGO_INCREMENTAL=0, so agents never wait for one
#     another's cargo lock;
#   - its own Postgres: the container pg-<name> (the stack's image, its own port), started or
#     reused, in DATABASE_URL and TEST_DATABASE_URL;
#   - its own SpiceDB: the container spicedb-<name> (memory datastore, its own ports), started or
#     reused, in SPICEDB_ENDPOINT, RUUM_SPICEDB_ENDPOINT and RUUM_SPICEDB_HTTP_URL;
#   - a dev root of its own, ~/w/.<name>.dev, in RUUM_DEV_ROOT (scripts/e2e_browser.sh) and its
#     scripts/dev.sh in RUUM_DEV_SH (scripts/e2e_smoke.sh): a wrapper of the base copy's dev.sh
#     whose `env` prints the agent's Postgres and SpiceDB instead of the stack's, next to a link to
#     the stack's deploy/dev/.env. So the e2e scripts use the agent's databases too;
#   - free ports for the e2e scripts: RUUM_E2E_PORT and RUUM_E2E_DAV_PORT (e2e_smoke.sh),
#     RUUM_E2E_BROWSER_PORT, RUUM_E2E_BROWSER_DAV_PORT, RUUM_E2E_WEB_PORT and RUUM_E2E_MOCK_PORT
#     (e2e_browser.sh). Node.js, npm and Playwright's Chromium come from setup.sh.
# The output streams here; the exit status is the command's. The command gets no stdin. Ctrl-C or
# SIGTERM stops the command on the server too.
#
# --stop stops a running command of <name> and deletes its Postgres and SpiceDB (with their data),
# ~/targets/<name>, ~/w/<name> and its dev root ~/w/.<name>.dev.
#
# Settings: BUILDBOX_HOST (default 2.31.194.2), BUILDBOX_USER (ruum),
# BUILDBOX_KEY (~/.config/ruum/buildbox_ed25519).
set -euo pipefail

HOST="${BUILDBOX_HOST:-2.31.194.2}"
DEST="${BUILDBOX_USER:-ruum}@$HOST"
KEY="${BUILDBOX_KEY:-$HOME/.config/ruum/buildbox_ed25519}"
SSH=(ssh -i "$KEY" -o StrictHostKeyChecking=accept-new -o BatchMode=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=10)

say() { printf '\033[1;34mbuildbox:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mbuildbox error:\033[0m %s\n' "$*" >&2; exit 2; }
usage() { sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

# The part that runs on the server: bash -c "$REMOTE" buildbox <action> <name> [args...]
read -r -d '' REMOTE <<'EOF' || true
set -euo pipefail
action=$1 name=$2
shift 2
W=$HOME/w/$name T=$HOME/targets/$name C=spicedb-$name BASE=$HOME/ruum-server
P=pg-$name D=$HOME/w/.$name.dev
PIDFILE=$HOME/w/.$name.pid
# shellcheck disable=SC1091
source "$HOME/.cargo/env"

kill_running() {
  if [[ -s $PIDFILE ]]; then
    kill -TERM -- "-$(cat "$PIDFILE")" 2>/dev/null || true
    rm -f "$PIDFILE"
  fi
}

case $action in
  prune) # stdin: the NUL-separated list of files that the worktree has
    mkdir -p "$W"
    cd "$W"
    declare -A keep=()
    while IFS= read -r -d '' f; do keep["./$f"]=1; done
    while IFS= read -r -d '' f; do
      [[ -n ${keep[$f]:-} ]] || rm -f -- "$f"
    done < <(find . \( -name node_modules -o -name target \) -prune -o \( -type f -o -type l \) -print0)
    find . -mindepth 1 \( -name node_modules -o -name target \) -prune -o -type d -empty -print0 | xargs -0r rmdir -p --ignore-fail-on-non-empty 2>/dev/null || true
    ;;
  run)
    [[ -d $W ]] || { echo "no $W" >&2; exit 2; }
    eval "$("$BASE/scripts/dev.sh" env)"
    image=$(awk '/image: authzed\/spicedb:/ {print $2; exit}' "$W/deploy/dev/docker-compose.yml" 2>/dev/null || true)
    [[ -n $image ]] || image=$(awk '/image: authzed\/spicedb:/ {print $2; exit}' "$BASE/deploy/dev/docker-compose.yml")
    state=$(docker inspect -f '{{.State.Running}} {{.Config.Image}}' "$C" 2>/dev/null || true)
    if [[ $state == "false $image" ]]; then
      docker start "$C" >/dev/null
    elif [[ $state != "true $image" ]]; then
      [[ -z $state ]] || docker rm -f "$C" >/dev/null
      docker run -d --name "$C" --label ruum.buildbox=agent --label "ruum.buildbox.name=$name" \
        --restart unless-stopped --memory 512m -p 127.0.0.1::50051 -p 127.0.0.1::8443 \
        -e "SPICEDB_GRPC_PRESHARED_KEY=$SPICEDB_PRESHARED_KEY" -e SPICEDB_DATASTORE_ENGINE=memory \
        "$image" serve --http-enabled >/dev/null
    fi
    for _ in $(seq 1 120); do
      docker exec "$C" grpc_health_probe -addr=localhost:50051 >/dev/null 2>&1 && break
      sleep 0.5
    done
    docker exec "$C" grpc_health_probe -addr=localhost:50051 >/dev/null 2>&1 || { echo "$C is not healthy" >&2; docker logs --tail 20 "$C" >&2; exit 2; }
    grpc=$(docker port "$C" 50051/tcp | head -1 | awk -F: '{print $NF}')
    http=$(docker port "$C" 8443/tcp | head -1 | awk -F: '{print $NF}')
    export SPICEDB_ENDPOINT=http://127.0.0.1:$grpc RUUM_SPICEDB_ENDPOINT=http://127.0.0.1:$grpc
    export RUUM_SPICEDB_HTTP_URL=http://127.0.0.1:$http RUUM_SPICEDB_PRESHARED_KEY=$SPICEDB_PRESHARED_KEY
    # A Postgres of its own (the stack's image, data on a volume, no fsync): #[sqlx::test] names a
    # test database after the test, so two agents that run the same test in one Postgres clash.
    pgimage=ruum-dev/postgres:17-postgis-pgvector
    secret() { sed -n "s/^$1=//p" "$BASE/deploy/dev/.env" | tail -1; }
    ruum_pw=$(secret RUUM_DB_PASSWORD)
    state=$(docker inspect -f '{{.State.Running}}' "$P" 2>/dev/null || true)
    if [[ $state == false ]]; then
      docker start "$P" >/dev/null
    elif [[ -z $state ]]; then
      docker run -d --name "$P" --label ruum.buildbox=agent --label "ruum.buildbox.name=$name" \
        --restart unless-stopped --memory 2g --shm-size 256m -p 127.0.0.1::5432 \
        -v /var/lib/postgresql/data \
        -e POSTGRES_USER=postgres -e "POSTGRES_PASSWORD=$(secret POSTGRES_PASSWORD)" -e "RUUM_DB_PASSWORD=$ruum_pw" \
        -e "ZITADEL_DB_PASSWORD=$(secret ZITADEL_DB_PASSWORD)" -e "SPICEDB_DB_PASSWORD=$(secret SPICEDB_DB_PASSWORD)" \
        "$pgimage" postgres -c max_connections=300 -c shared_buffers=256MB -c work_mem=8MB \
        -c fsync=off -c synchronous_commit=off -c full_page_writes=off >/dev/null
    fi
    for _ in $(seq 1 240); do
      docker exec "$P" pg_isready -q -h 127.0.0.1 -U postgres -d ruum 2>/dev/null && break
      sleep 0.5
    done
    docker exec "$P" pg_isready -q -h 127.0.0.1 -U postgres -d ruum || { echo "$P is not ready" >&2; docker logs --tail 20 "$P" >&2; exit 2; }
    pg=$(docker port "$P" 5432/tcp | head -1 | awk -F: '{print $NF}')
    export DATABASE_URL=postgres://ruum:$ruum_pw@127.0.0.1:$pg/ruum
    export TEST_DATABASE_URL=postgres://ruum:$ruum_pw@127.0.0.1:$pg/ruum_test
    export CARGO_TARGET_DIR=$T CARGO_INCREMENTAL=0 BUILDBOX_NAME=$name
    # The agent's dev root: the e2e scripts read `dev.sh env` from it, so they get the agent's
    # Postgres and SpiceDB (the BUILDBOX_* settings), and the rest of the shared stack.
    export BUILDBOX_DATABASE_URL=$DATABASE_URL BUILDBOX_TEST_DATABASE_URL=$TEST_DATABASE_URL
    export BUILDBOX_SPICEDB_ENDPOINT=$SPICEDB_ENDPOINT BUILDBOX_SPICEDB_HTTP_URL=$RUUM_SPICEDB_HTTP_URL
    mkdir -p "$D/scripts" "$D/deploy/dev"
    ln -sfn "$BASE/deploy/dev/.env" "$D/deploy/dev/.env"
    cat >"$D/scripts/dev.sh" <<'WRAPPER'
#!/usr/bin/env bash
# scripts/dev.sh of the build server's stack, but `env` prints this agent's Postgres and SpiceDB
# (made by scripts/buildbox/run.sh, which sets the BUILDBOX_* settings).
set -euo pipefail
base=$HOME/ruum-server/scripts/dev.sh
[[ ${1:-} == env ]] || exec "$base" "$@"
"$base" env
for v in DATABASE_URL TEST_DATABASE_URL; do
  b=BUILDBOX_$v
  printf 'export %s=%q\n' "$v" "${!b:?run this through scripts/buildbox/run.sh}"
done
printf 'export %s=%q\n' SPICEDB_ENDPOINT "$BUILDBOX_SPICEDB_ENDPOINT" \
  RUUM_SPICEDB_ENDPOINT "$BUILDBOX_SPICEDB_ENDPOINT" RUUM_SPICEDB_HTTP_URL "$BUILDBOX_SPICEDB_HTTP_URL"
WRAPPER
    chmod +x "$D/scripts/dev.sh"
    export RUUM_DEV_ROOT=$D RUUM_DEV_SH=$D/scripts/dev.sh
    # Free ports for the e2e scripts; a setting given with `env` wins.
    read -r p1 p2 p3 p4 p5 p6 < <(python3 -c '
import socket
s = [socket.socket() for _ in range(6)]
for x in s: x.bind(("127.0.0.1", 0))
print(*[x.getsockname()[1] for x in s])')
    export RUUM_E2E_PORT=${RUUM_E2E_PORT:-$p1} RUUM_E2E_DAV_PORT=${RUUM_E2E_DAV_PORT:-$p2}
    export RUUM_E2E_BROWSER_PORT=${RUUM_E2E_BROWSER_PORT:-$p3} RUUM_E2E_BROWSER_DAV_PORT=${RUUM_E2E_BROWSER_DAV_PORT:-$p4}
    export RUUM_E2E_WEB_PORT=${RUUM_E2E_WEB_PORT:-$p5} RUUM_E2E_MOCK_PORT=${RUUM_E2E_MOCK_PORT:-$p6}
    mkdir -p "$T"
    cd "$W"
    echo "postgres $P on :$pg; spicedb $C on grpc :$grpc http :$http; target $T; e2e ports: smoke $RUUM_E2E_PORT $RUUM_E2E_DAV_PORT, browser $RUUM_E2E_BROWSER_PORT $RUUM_E2E_BROWSER_DAV_PORT web $RUUM_E2E_WEB_PORT mock $RUUM_E2E_MOCK_PORT" >&2
    # A session of its own, so an interrupt can stop the whole process group.
    status=0
    setsid -w bash -c 'echo $$ >"$0"; exec "$@"' "$PIDFILE" "$@" || status=$?
    rm -f "$PIDFILE"
    exit "$status"
    ;;
  kill) kill_running ;;
  stop)
    kill_running
    docker rm -f -v "$C" "$P" >/dev/null 2>&1 || true
    rm -rf "$T" "$W" "$D"
    echo "removed $C, $P, $T, $W and $D"
    ;;
  *) echo "unknown action $action" >&2; exit 2 ;;
esac
EOF

# remote <action> <name> [args...]: run the server part over ssh; extra ssh options come first in SSH_EXTRA.
remote() {
  local cmd arg
  cmd="bash -c $(printf %q "$REMOTE") buildbox"
  for arg in "$@"; do cmd+=" $(printf %q "$arg")"; done
  "${SSH[@]}" ${SSH_EXTRA[@]+"${SSH_EXTRA[@]}"} "$DEST" "$cmd"
}

[[ $# -ge 2 ]] || usage
NAME="$1"
shift
[[ "$NAME" =~ ^[a-z0-9][a-z0-9_-]{0,47}$ ]] || die "the name must match [a-z0-9][a-z0-9_-]* (it names a container and folders)"

if [[ "$1" == "--stop" ]]; then
  remote stop "$NAME"
  exit 0
fi

SRC="$1"
shift
[[ "${1:-}" == "--" ]] || usage
shift
[[ $# -ge 1 ]] || usage
SRC="$(git -C "$SRC" rev-parse --show-toplevel 2>/dev/null)" || die "$SRC is not a git worktree"

start=$SECONDS
LIST="$(mktemp "${TMPDIR:-/tmp}/buildbox.XXXXXX")"
trap 'rm -f "$LIST"' EXIT
git -C "$SRC" ls-files -z -co --exclude-standard | while IFS= read -r -d '' f; do
  case "$f" in
    target | target/* | target[.-]* | node_modules/* | */node_modules/*) continue ;;
  esac
  [[ -e "$SRC/$f" || -L "$SRC/$f" ]] && printf '%s\0' "$f"
done >"$LIST"

remote prune "$NAME" <"$LIST"
rsync -a -0 --files-from="$LIST" -e "${SSH[*]}" "$SRC/" "$DEST:w/$NAME/" ||
  die "rsync failed"
say "$NAME: synced $(tr -cd '\0' <"$LIST" | wc -c | tr -d ' ') files from $SRC in $((SECONDS - start))s"

# Run in the background and wait, so that SIGTERM reaches the trap at once.
SSH_EXTRA=(-n)
remote run "$NAME" "$@" &
pid=$!
on_signal() {
  say "$NAME: interrupted; stopping the command on the server"
  SSH_EXTRA=(-n)
  remote kill "$NAME" || true
  kill "$pid" 2>/dev/null || true
  exit 130
}
trap on_signal INT TERM
status=0
wait "$pid" || status=$?
say "$NAME: exit $status after $((SECONDS - start))s"
exit "$status"
