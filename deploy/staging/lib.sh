# Shared functions of the staging scripts (up.sh, deploy.sh, status.sh, destroy.sh,
# secrets-push.sh, bootstrap-state.sh, tool.sh). Sourced, not run.
#
# Where things run (decision of the person, 2026-11-11): every staging step runs on the person's
# Mac, never on the build server. The build server's agent user is in the docker group, which is
# root on that host, so no user separation there can protect the staging credentials.
#
# - The scripts run in the Mac's own bash (macOS bash 3.2 is enough). They read the secrets file
#   ~/.config/ruum/secrets.env line by line, one name at a time, and never source it.
# - Each step runs steps.sh in the tools container (tools-image.sh: one local image with
#   OpenTofu, talosctl, kubectl, helm, helmfile, cosign, hcloud and hcloud-upload-image at the
#   versions and SHA-256 sums of versions.env, on a digest-pinned Alpine base). Docker Desktop
#   runs it; `docker run --rm`, so nothing stays behind but the files in the work folder.
# - Secrets go to the container on its stdin as NAME=value lines: never on a command line (not
#   in `ps`, not in `docker inspect`), never in a file, never printed.
# - The work folder ~/.config/ruum/staging (0700) holds the kubeconfig, the talosconfig and the
#   plans. It is the only folder that the container can write. The checkout is mounted read-only.
#
# Admin access (decision of the person, 2026-11-11): the staging firewall opens the Talos API
# (50000) and the Kubernetes API (6443) only to the build server's fixed public IPv4 (steps.sh
# admin_cidrs reads it from the Hetzner API). The Mac reaches them through an SSH local forward
# over the build server (stg_tunnel_up below, tunnel.sh), as the user ruumtunnel, which may only
# forward to those ports. The admin credentials stay on the Mac; the build server relays the
# encrypted TLS and mTLS bytes only.

# shellcheck shell=bash
STG_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STG_ROOT="$(cd "$STG_HERE/../.." && pwd)"
# shellcheck source=versions.env
. "$STG_HERE/versions.env"
# shellcheck source=tunnel.sh
. "$STG_HERE/tunnel.sh"

STG_SECRETS="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
STG_WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"

stg_log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
stg_warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
stg_die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# stg_on_exit COMMAND: run COMMAND when the script exits (after the ones added later).
STG_EXIT_CMDS=""
stg_on_exit() {
  STG_EXIT_CMDS="$1${STG_EXIT_CMDS:+; $STG_EXIT_CMDS}"
  # shellcheck disable=SC2064
  trap "$STG_EXIT_CMDS" EXIT
}

# The value of one name in the secrets file. The file is read line by line for that name and
# never sourced, so no other secret gets into the shell.
stg_secret() {
  local name="$1" line value
  [[ -r "$STG_SECRETS" ]] || return 0
  line="$(grep -E "^(export +)?$name=" "$STG_SECRETS" | tail -n 1 || true)"
  [[ -n "$line" ]] || return 0
  value="${line#*=}"
  value="${value%$'\r'}"
  if [[ "$value" =~ ^\"(.*)\"$ || "$value" =~ ^\'(.*)\'$ ]]; then value="${BASH_REMATCH[1]}"; fi
  printf '%s' "$value"
}

stg_has_secret() { [[ -n "$(stg_secret "$1")" ]]; }

# The names that are missing from the secrets file (prints them, one per line).
stg_missing() {
  local n
  for n in "$@"; do stg_has_secret "$n" || echo "$n"; done
}

# Prints NAME=value for each name (to be piped into stg_run). Never call it on a terminal.
stg_secret_stream() {
  local n v
  for n in "$@"; do
    v="$(stg_secret "$n")"
    [[ -n "$v" ]] && printf '%s=%s\n' "$n" "$v"
  done
  printf '__END__\n'
}

# The names each step needs. Nothing else is ever sent.
# shellcheck disable=SC2034 # used by the scripts that source this file
STG_NAMES_TOFU=(HCLOUD_TOKEN HETZNER_S3_ACCESS_KEY HETZNER_S3_SECRET_KEY SCW_ACCESS_KEY SCW_SECRET_KEY
  SCW_DEFAULT_PROJECT_ID SCW_DEFAULT_ORGANIZATION_ID TOFU_STATE_PASSPHRASE)

# The work folder on the Mac: 0700, owned by the person.
stg_work() {
  mkdir -p "$STG_WORK"
  chmod 700 "$STG_WORK"
}

# The name of the tools image for this checkout (tools-image.sh builds it when it is missing).
stg_image() {
  STG_IMAGE="$(bash "$STG_HERE/tools-image.sh" ensure)" || stg_die "the tools image is not available (deploy/staging/tools-image.sh build)"
  [[ "$STG_IMAGE" =~ ^ruum-staging-tools:[0-9a-f]{16}$ ]] || stg_die "unexpected tools image name '$STG_IMAGE'"
}

# stg_docker [docker run options...] -- COMMAND...: run COMMAND in the tools container.
# The checkout is mounted read-only and the work folder read-write, both at their Mac paths, so
# every path in a message is the same on both sides. No capabilities, no new privileges, the
# person's user and group IDs. stdin is passed on (-i), so the secrets reach the command.
stg_docker() {
  local opts=()
  while [[ $# -gt 0 && "$1" != -- ]]; do opts+=("$1"); shift; done
  [[ "${1:-}" == -- ]] && shift
  command -v docker >/dev/null || stg_die "docker is missing (Docker Desktop)"
  stg_work
  stg_image
  # While the admin tunnel is up, the command runs behind tunnel-relay.sh, which listens on the
  # tunnel ports on 127.0.0.1 of the container and relays them to the Mac.
  if [[ -n "$STG_TUNNEL_PORTS" ]]; then
    opts+=(-e "STG_TUNNEL_PORTS=$STG_TUNNEL_PORTS")
    set -- bash "$STG_HERE/tunnel-relay.sh" "$@"
  fi
  docker run --rm -i --init \
    --user "$(id -u):$(id -g)" \
    --cap-drop ALL --security-opt no-new-privileges \
    --tmpfs /tmp:rw,exec,size=2g \
    -e HOME=/tmp \
    -e RUUM_STAGING_WORK="$STG_WORK" \
    -e KUBECONFIG="$STG_WORK/kubeconfig" \
    -e TALOSCONFIG="$STG_WORK/talosconfig" \
    -v "$STG_ROOT:$STG_ROOT:ro" \
    -v "$STG_WORK:$STG_WORK" \
    -w "$STG_ROOT" \
    ${opts[@]+"${opts[@]}"} \
    "$STG_IMAGE" "$@"
}

# stg_run MUTATES STEP ARGS...: run steps.sh STEP in the tools container; stdin is passed on.
# MUTATES is "ro" or "rw". Only an "rw" step gets RUUM_STAGING_APPLY (when it is "yes" here), and
# the steps that create or change cloud resources refuse to run without it (steps.sh).
stg_run() {
  local mutates="$1" env=()
  shift
  case "$mutates" in
    ro) ;;
    rw) [[ "${RUUM_STAGING_APPLY:-}" != yes ]] || env+=(-e RUUM_STAGING_APPLY=yes) ;;
    *) stg_die "stg_run: ro or rw, not '$mutates'" ;;
  esac
  stg_docker ${env[@]+"${env[@]}"} -- bash "$STG_HERE/steps.sh" "$@"
}

# One audit line per operation (review 2 O-2): who, what, when, result. Local file; the digest
# reads it. Never holds a secret.
stg_audit() {
  local dir="${RUUM_STAGING_AUDIT_DIR:-$HOME/.local/state/ruum}"
  mkdir -p "$dir"
  printf '%s\t%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${USER:-agent}" "$1" "$2" >>"$dir/staging-audit.tsv"
}

# --- The admin tunnel over the build server (tunnel.sh, ops/buildbox-tunnel-user.sh) ---
# The key and the host key of the build server live only on the Mac. ops/tunnel-setup.sh makes
# the key when it is missing, installs it on the build server (ops/buildbox-tunnel-user.sh) and
# records the host key.
STG_TUNNEL_KEY="$HOME/.config/ruum/staging_tunnel_ed25519"
STG_TUNNEL_KNOWN_HOSTS="$HOME/.config/ruum/staging_tunnel_known_hosts"
# The control socket of the open tunnel. stg_tunnel_up names it after a fingerprint of the
# connection (tunnel-<fingerprint>.sock), so a tunnel is joined only when it was opened to the same
# build server, with the same host key, key and forwards (Kimi review F4).
STG_TUNNEL_SOCK=""
STG_TUNNEL_PORTS=""
STG_TUNNEL_BASTION=""
# STG_TUNNEL_FRESH=1 (ops/tunnel-setup.sh): close every open tunnel and open a new one, so the test
# uses the key, host key and forwards that were just installed.
STG_TUNNEL_FRESH=0

# The build server and the staging nodes, from the Hetzner API (in the tools container).
stg_tunnel_targets() {
  stg_secret_stream HCLOUD_TOKEN | stg_run ro tunnel-targets
}

# The SHA-256 of stdin, in hex (shasum on the Mac, sha256sum where there is no shasum).
stg_sha256() {
  if command -v shasum >/dev/null; then shasum -a 256; else sha256sum; fi | cut -d' ' -f1
}

# After tunnel_parse: the fingerprint of the tunnel connection: the user and the build server, the
# forwards, the recorded host key and the public half of the key. Any change gives another
# control socket.
stg_tunnel_fingerprint() {
  local pub
  pub="$(ssh-keygen -y -f "$STG_TUNNEL_KEY" 2>/dev/null)" || return 1
  { printf '%s\n' "$TUNNEL_USER@$TUNNEL_BASTION" "$pub"; tunnel_forward_args; cat "$STG_TUNNEL_KNOWN_HOSTS"; } |
    stg_sha256 | cut -c1-16
}

# The ssh options of every tunnel connection: only this key and this host key, no user ssh
# config, no agent, no X11, no tty, no command; a forward that cannot listen ends ssh.
stg_tunnel_ssh_opts() {
  printf '%s\n' -F /dev/null \
    -o BatchMode=yes -o IdentitiesOnly=yes -o "IdentityFile=$STG_TUNNEL_KEY" -o IdentityAgent=none \
    -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$STG_TUNNEL_KNOWN_HOSTS" -o GlobalKnownHostsFile=/dev/null \
    -o HostKeyAlgorithms=ssh-ed25519 -o UpdateHostKeys=no -o CheckHostIP=no \
    -o ExitOnForwardFailure=yes -o ForwardAgent=no -o ForwardX11=no -o RequestTTY=no -o PermitLocalCommand=no \
    -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
    -o ControlPath="$STG_TUNNEL_SOCK"
}

# stg_tunnel_close_others [all]: close the tunnels of other control sockets in the work folder
# (they hold the local ports), or every tunnel with "all".
stg_tunnel_close_others() {
  local s
  for s in "$STG_WORK"/tunnel*.sock; do
    [[ -S "$s" ]] || continue
    [[ "${1:-}" == all || "$s" != "$STG_TUNNEL_SOCK" ]] || continue
    stg_log "admin tunnel: closing the tunnel of $(basename "$s") (another build server, host key, key or forwards)"
    ssh -F /dev/null -o ControlPath="$s" -O exit "$TUNNEL_USER@$TUNNEL_BASTION" 2>/dev/null || true
    rm -f "$s"
  done
}

# stg_tunnel_probe TARGET: open one channel to TARGET (address:port) over the tunnel
# connection (ssh -W, no data sent) and close it. Fails when the build server refuses or cannot
# connect, or after 20 seconds.
stg_tunnel_probe() {
  local target="$1" pid watchdog rc
  ssh "${STG_TUNNEL_OPTS[@]}" -o ControlMaster=no -W "$target" "$TUNNEL_USER@$TUNNEL_BASTION" </dev/null >/dev/null 2>&1 &
  pid=$!
  (sleep 20 && kill "$pid") >/dev/null 2>&1 &
  watchdog=$!
  wait "$pid"
  rc=$?
  kill "$watchdog" 2>/dev/null || true
  wait "$watchdog" 2>/dev/null || true
  return "$rc"
}

# After tunnel_parse: check every forward of the tunnel (Kimi review F4). The local port must
# listen on 127.0.0.1, and the build server must open a connection to its node port.
stg_tunnel_verify() {
  local fwd=() line i spec port target
  while IFS= read -r line; do fwd+=("$line"); done < <(tunnel_forward_args)
  for ((i = 1; i < ${#fwd[@]}; i += 2)); do
    spec="${fwd[i]#127.0.0.1:}"
    port="${spec%%:*}"
    target="${spec#*:}"
    (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null ||
      { stg_warn "admin tunnel: nothing listens on 127.0.0.1:$port"; return 1; }
    stg_tunnel_probe "$target" ||
      { stg_warn "admin tunnel: the build server cannot open $target (the forward of 127.0.0.1:$port)"; return 1; }
  done
}

# Opens the tunnel (or joins one that is up already with the same fingerprint) and sets
# STG_TUNNEL_PORTS, so the next stg_docker and stg_run calls reach the APIs. A tunnel that this
# script opened is closed on exit. Every forward is checked before the tunnel is used.
STG_TUNNEL_OPTS=()
stg_tunnel_up() {
  local targets fwd=() line
  [[ -z "$STG_TUNNEL_PORTS" ]] || return 0
  command -v ssh >/dev/null || stg_die "ssh is missing"
  [[ -r "$STG_TUNNEL_KEY" ]] || stg_die "the tunnel key $STG_TUNNEL_KEY is missing: run deploy/staging/ops/tunnel-setup.sh (README 'Admin access')"
  [[ -s "$STG_TUNNEL_KNOWN_HOSTS" ]] || stg_die "the build server's host key is not recorded in $STG_TUNNEL_KNOWN_HOSTS: run deploy/staging/ops/tunnel-setup.sh"
  stg_work
  targets="$(stg_tunnel_targets)" || stg_die "cannot read the tunnel targets from the Hetzner API"
  tunnel_parse "$targets" >&2 || stg_die "unexpected tunnel targets"
  line="$(stg_tunnel_fingerprint)" || stg_die "cannot read the public key of $STG_TUNNEL_KEY"
  STG_TUNNEL_SOCK="$STG_WORK/tunnel-$line.sock"
  STG_TUNNEL_OPTS=()
  while IFS= read -r line; do STG_TUNNEL_OPTS+=("$line"); done < <(stg_tunnel_ssh_opts)
  while IFS= read -r line; do fwd+=("$line"); done < <(tunnel_forward_args)
  STG_TUNNEL_BASTION="$TUNNEL_BASTION"
  if [[ "$STG_TUNNEL_FRESH" == 1 ]]; then
    stg_tunnel_close_others all
  else
    stg_tunnel_close_others
  fi
  if [[ "$STG_TUNNEL_FRESH" != 1 ]] && ssh "${STG_TUNNEL_OPTS[@]}" -O check "$TUNNEL_USER@$TUNNEL_BASTION" 2>/dev/null; then
    stg_log "admin tunnel: joining the open tunnel over $TUNNEL_BASTION"
  else
    rm -f "$STG_TUNNEL_SOCK"
    stg_log "admin tunnel: $TUNNEL_USER@$TUNNEL_BASTION, local ports $(tunnel_ports)"
    ssh "${STG_TUNNEL_OPTS[@]}" -o ControlMaster=yes -f -N "${fwd[@]}" "$TUNNEL_USER@$TUNNEL_BASTION" ||
      stg_die "the admin tunnel over $TUNNEL_BASTION did not open (key installed? ops/tunnel-setup.sh)"
    stg_on_exit stg_tunnel_down
  fi
  stg_tunnel_verify || stg_die "the admin tunnel over $TUNNEL_BASTION does not reach every node API (run deploy/staging/ops/tunnel-setup.sh after a node or key change)"
  STG_TUNNEL_PORTS="$(tunnel_ports)"
}

# Closes the tunnel that stg_tunnel_up opened.
stg_tunnel_down() {
  [[ -n "$STG_TUNNEL_BASTION" && -n "$STG_TUNNEL_SOCK" && -S "$STG_TUNNEL_SOCK" ]] || return 0
  ssh "${STG_TUNNEL_OPTS[@]}" -O exit "$TUNNEL_USER@$STG_TUNNEL_BASTION" 2>/dev/null || true
  STG_TUNNEL_PORTS=""
}
