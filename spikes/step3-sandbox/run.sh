#!/usr/bin/env bash
# Spike S-2 (docs/STEP3.md card S-2): the sandbox, the builder and the gateway on the build server.
# See README.md next to this file.
#
#   scripts/buildbox/run.sh <name> <worktree> -- spikes/step3-sandbox/run.sh [point...]
#   spikes/step3-sandbox/run.sh --clean
#
# Points: 2 4 1 3 5 r (r = runsc). Without arguments it runs every point in that order. It prints
# PASS, FAIL or SKIP per check and exits non-zero on any FAIL. Everything it makes on the host is
# named s3w1s-* (containers, networks, images, the Garage bucket and key); --clean removes it and it
# cleans up at the end of a run. It probes only from its own containers, only with TCP connects and
# one HTTP GET to the metadata address, and never targets staging or the SSH tunnel.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
P=s3w1s
WORK="${S2_WORK:-$HOME/$P-work}"
GARAGE_C="${S2_GARAGE_CONTAINER:-ruum-garage-1}"
TARGET="${CARGO_TARGET_DIR:-$HERE/gw/target}"
GW="$TARGET/release/s2-gw"
NODE_IMAGE="${S2_NODE_IMAGE:-node:22-alpine}"
ISS="https://ruum.localhost"
AUD_A="app:0190b6c4-a00d-7cc1-9a4b-00000000000a"
AUD_B="app:0190b6c4-a00d-7cc1-9a4b-00000000000b"

PASS=0 FAIL=0 SKIP=0
FAILED=()
pass() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); FAILED+=("$1"); printf 'FAIL  %s%s\n' "$1" "${2:+ ($2)}"; }
skip() { SKIP=$((SKIP + 1)); printf 'SKIP  %s (%s)\n' "$1" "$2"; }
check() { # name command...
  local name=$1; shift
  if "$@"; then pass "$name"; else fail "$name"; fi
}
jqe() { jq -e "$1" >/dev/null 2>&1; }
section() { printf '\n== %s\n' "$*"; }
note() { printf '      %s\n' "$*"; }

# The driver's flags (docs/STEP3.md "How an app runs" 1): no privileges, read-only root, no
# capabilities, no new privileges, a non-root user, the default seccomp profile, CPU, memory and
# process limits, one internal network per app, no Docker socket, a small noexec /tmp.
DRIVER_FLAGS=(
  --read-only --cap-drop ALL --security-opt no-new-privileges
  --user 10001:10001 --memory 256m --memory-swap 256m --cpus 1 --pids-limit 128
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m --label ruum.app=$P
)

garage() { docker exec "$GARAGE_C" /garage "$@" 2>/dev/null; }
ip_of() { # container network
  docker inspect -f "{{(index .NetworkSettings.Networks \"$2\").IPAddress}}" "$1" 2>/dev/null
}
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
wait_http() { # url [tries]
  local i
  for ((i = 0; i < ${2:-100}; i++)); do
    curl -fsS -o /dev/null --max-time 1 "$1" 2>/dev/null && return 0
    sleep 0.05
  done
  return 1
}

PIDS=()
cleanup() {
  local p
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  PIDS=()
  docker ps -aq --filter "name=^$P-" | xargs -r docker rm -f >/dev/null 2>&1
  docker network ls -q --filter "name=^$P-" | xargs -r docker network rm >/dev/null 2>&1
  docker volume ls -q --filter "name=^$P-" | xargs -r docker volume rm >/dev/null 2>&1
  if [[ -f $WORK/s3.env ]]; then
    (set -a; . "$WORK/s3.env"; "$GW" clean >/dev/null 2>&1)
  fi
  garage bucket delete --yes "$P-static" >/dev/null
  garage key delete --yes "$P-key" >/dev/null
  rm -rf "$WORK"
}
# Images this run pulled (they were not on the host before), removed at the end with its own.
PULLED=()
pull() { # image
  docker image inspect "$1" >/dev/null 2>&1 && return 0
  docker pull -q "$1" >/dev/null 2>&1 && PULLED+=("$1")
}
clean_images() {
  docker images -q --filter "reference=$P-*" | sort -u | xargs -r docker rmi -f >/dev/null 2>&1
  [[ ${#PULLED[@]} -eq 0 ]] || docker rmi "${PULLED[@]}" >/dev/null 2>&1
  PULLED=()
  docker builder prune -f --filter "label=ruum.app=$P" >/dev/null 2>&1
}

if [[ ${1:-} == --clean ]]; then
  cleanup
  clean_images
  echo "removed every $P-* container, network, volume, image, the Garage bucket and key"
  exit 0
fi

POINTS=("$@")
[[ ${#POINTS[@]} -gt 0 ]] || POINTS=(2 4 1 3 5 r)
want() { local p; for p in "${POINTS[@]}"; do [[ $p == "$1" ]] && return 0; done; return 1; }

trap 'cleanup; clean_images' EXIT
cleanup
mkdir -p "$WORK"
chmod 700 "$WORK"

section "set-up"
df -h / | awk 'NR==2 {print "      disk free: " $4}'
(cd "$HERE/gw" && cargo build --release --locked -q) || { echo "gateway build failed" >&2; exit 2; }
pull "$NODE_IMAGE"
note "node image: $(docker image inspect "$NODE_IMAGE" -f '{{index .RepoDigests 0}}')"
docker build -q -t "$P-app:1" --label ruum.app=$P "$HERE/app" >/dev/null || { echo "app image failed" >&2; exit 2; }

# The user-token key (Ed25519, made for this run, never printed, removed at the end).
openssl genpkey -algorithm ed25519 -out "$WORK/user-token.pem" 2>/dev/null
openssl pkey -in "$WORK/user-token.pem" -pubout -out "$WORK/user-token.pub" 2>/dev/null
chmod 600 "$WORK/user-token.pem"
PUBKEY="$(cat "$WORK/user-token.pub")"
RUNSC_RUNTIME=""
docker info --format '{{json .Runtimes}}' | grep -q '"runsc"' && RUNSC_RUNTIME=runsc

# One internal network per app (internal: true: no route out, no port publishing). APP_NET_OPTS is
# the finding of point 1: without inhibit_ipv4 the host has an address on every bridge, and an app
# reaches every host service that listens on all addresses (sshd) through its gateway.
APP_NET_OPTS=(-o com.docker.network.bridge.inhibit_ipv4=true)
net() { # name [docker network create options...]
  local name=$1
  shift
  docker network create --internal --label ruum.app=$P "$@" "$P-$name" >/dev/null
}

app() { # name network audience [extra docker args...]
  local name=$1 network=$2 aud=$3
  shift 3
  docker run -d --name "$P-$name" --network "$P-$network" "${DRIVER_FLAGS[@]}" \
    -e RUUM_USER_PUBKEY="$PUBKEY" -e RUUM_ISSUER="$ISS" -e RUUM_AUDIENCE="$aud" -e RUUM_ENV=live \
    "$@" "$P-app:1" >/dev/null
}

# ------------------------------------------------------------------------------------------------
point2() {
  section "point 2: the gateway cleans the request, adds a signed Ruum-User and cleans the response"
  net net-a
  net net-b
  app app-a net-a "$AUD_A"
  app app-b net-b "$AUD_B"
  local ipa ipb port
  ipa="$(ip_of "$P-app-a" "$P-net-a")"
  ipb="$(ip_of "$P-app-b" "$P-net-b")"
  wait_http "http://$ipa:8080/healthz" || { fail "2 app A starts on its internal network"; return; }
  wait_http "http://$ipb:8080/healthz" || { fail "2 app B starts on its internal network"; return; }
  port="$(free_port)"
  GW_LISTEN="127.0.0.1:$port" GW_UPSTREAM="http://$ipa:8080" GW_KEY_FILE="$WORK/user-token.pem" \
    GW_ISS="$ISS" GW_AUD="$AUD_A" GW_ENV=live "$GW" proxy 2>"$WORK/proxy.log" &
  PIDS+=($!)
  wait_http "http://127.0.0.1:$port/healthz" || { fail "2 gateway starts"; return; }
  local G="http://127.0.0.1:$port"

  local out
  out="$(curl -fsS "$G/")"
  check "2.1 a plain request reaches the app with one Ruum-User" \
    jqe '.ruum_user_count == 1' <<<"$out"
  check "2.2 the app's SDK check accepts it (EdDSA signature, iss, aud, typ, env, exp <= 60 s)" \
    jqe '.ruum_user.valid == true and .ruum_user.claims.aud == "'"$AUD_A"'"' <<<"$out"

  out="$(curl -fsS "$G/" -H 'Ruum-User: forged-1' -H 'RUUM-USER: forged-2' -H 'Ruum_User: forged-3' \
    -H 'ruum-request-id: forged-4' -H 'Ruum-Tenant: forged-5' -H 'Ruum: forged-6' \
    -H 'traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01' \
    -H 'X-Forwarded-For: 10.9.9.9' -H 'Connection: keep-alive, Ruum-User')"
  check "2.3 forged Ruum-User headers (any case, Ruum_User, Ruum-*) never reach the app" \
    jqe '[.raw_headers[] | select(.[1] | test("forged"))] | length == 0' <<<"$out"
  check "2.4 the app sees exactly one Ruum-User, the gateway's, and no ruum_* header" \
    jqe '.ruum_user_count == 1 and .ruum_user.valid == true and
      ([.raw_headers[] | select(.[0] | ascii_downcase | test("^ruum_"))] | length == 0)' <<<"$out"
  check "2.5 an incoming traceparent is replaced; no X-Forwarded-For reaches the app (the visitor's IP is not passed)" \
    jqe '([.raw_headers[] | select(.[1] | test("0af7651916cd43dd|10\\.9\\.9\\.9"))] | length == 0)
      and ([.raw_headers[] | select(.[0] | ascii_downcase == "traceparent")] | length == 1)
      and ([.raw_headers[] | select(.[0] | ascii_downcase == "x-forwarded-for")] | length == 0)' <<<"$out"

  out="$(curl -fsS "$G/" -H 'Cookie: __Host-ruum_app=SESSION; a=1; __Host-ruum_visit=VISIT; b=2; ruum_x=3')"
  check "2.6 the gateway cookies leave Cookie; the app's own cookies stay" \
    jqe '[.raw_headers[] | select(.[0] | ascii_downcase == "cookie") | .[1]] == ["a=1; b=2"]' <<<"$out"
  out="$(curl -fsS "$G/" -H 'Authorization: Bearer ruum_appkey_0123456789')"
  check "2.7 a partner key in Authorization never reaches the app" \
    jqe '[.raw_headers[] | select(.[0] | ascii_downcase == "authorization")] | length == 0' <<<"$out"

  out="$(curl -fsS "$G/" -H 'X-Forwarded-Host: forged.example' -H 'X-Forwarded-Port: 8443' \
    -H 'X-Forwarded-Prefix: /forged' -H 'X-Forwarded-Proto: http' -H 'X-Forwarded-Server: forged' \
    -H 'Forwarded: for=10.9.9.9;host=forged.example' -H 'X-Real-IP: 10.9.9.9' \
    -H 'tracestate: forged=1' -H 'baggage: forged=1')"
  check "2.12 X-Forwarded-* (any suffix), Forwarded, X-Real-IP, tracestate and baggage never reach the app" \
    jqe '[.raw_headers[] | select((.[0] | ascii_downcase | test("^(x-forwarded-.*|forwarded|x-real-ip|tracestate|baggage)$"))
      or (.[1] | test("forged|10\\.9\\.9\\.9")))] | length == 0' <<<"$out"

  local hdr
  hdr="$(curl -fsS -D - -o /dev/null "$G/cookies" | tr -d '\r')"
  check "2.8 an app's Set-Cookie for __Host-ruum_*, ruum_* and RUUM is dropped; its own stays" \
    test "$(grep -i '^set-cookie:' <<<"$hdr" | sed 's/^[^:]*: //')" = "app_session=kept; Path=/; HttpOnly"
  check "2.9 the response has HSTS, nosniff, Referrer-Policy and frame-ancestors 'none'" bash -c '
    h=$(cat); grep -qi "^strict-transport-security: max-age" <<<"$h" &&
    grep -qi "^x-content-type-options: nosniff" <<<"$h" &&
    grep -qi "^referrer-policy: strict-origin-when-cross-origin" <<<"$h" &&
    grep -qi "^content-security-policy: frame-ancestors .none." <<<"$h"' <<<"$hdr"

  # A token made for app A does not pass app B's check (sent straight to B on its network).
  local tok
  tok="$(curl -fsS "$G/" | jq -r '.raw_headers[] | select(.[0] | ascii_downcase == "ruum-user") | .[1]')"
  out="$(curl -fsS "http://$ipb:8080/" -H "Ruum-User: $tok")"
  check "2.10 app B's SDK check refuses app A's token (aud)" \
    jqe '.ruum_user.valid == false and .ruum_user.reason == "aud"' <<<"$out"
  # A token signed with another key fails the signature check.
  openssl genpkey -algorithm ed25519 -out "$WORK/other.pem" 2>/dev/null
  local forged
  forged="$(python3 - "$WORK/other.pem" "$ISS" "$AUD_B" <<'PY'
import base64, json, subprocess, sys, time
key, iss, aud = sys.argv[1:4]
b = lambda d: base64.urlsafe_b64encode(d).rstrip(b"=").decode()
now = int(time.time())
h = b(json.dumps({"alg": "EdDSA", "typ": "JWT"}).encode())
c = b(json.dumps({"iss": iss, "aud": aud, "typ": "app_user", "env": "live", "sub": "pairwise:x",
                  "tenant": "t", "roles": ["admin"], "node_roles": {}, "actor": "user",
                  "iat": now, "exp": now + 60, "jti": "x"}).encode())
msg = key + ".msg"
with open(msg, "wb") as f:
    f.write(f"{h}.{c}".encode())
sig = subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", key, "-rawin", "-in", msg],
                     capture_output=True, check=True).stdout
print(f"{h}.{c}.{b(sig)}")
PY
)"
  out="$(curl -fsS "http://$ipb:8080/" -H "Ruum-User: $forged")"
  check "2.11 a token signed with another key fails the SDK check (signature)" \
    jqe '.ruum_user.valid == false and .ruum_user.reason == "signature"' <<<"$out"
}

# ------------------------------------------------------------------------------------------------
setup_garage() {
  garage bucket create "$P-static" >/dev/null
  garage key create "$P-key" >/dev/null
  garage bucket allow --read --write "$P-static" --key "$P-key" >/dev/null
  local info id secret
  info="$(garage key info --show-secret "$P-key")"
  id="$(awk -F': *' '/^Key ID:/ {print $2}' <<<"$info")"
  secret="$(awk -F': *' '/^Secret key:/ {print $2}' <<<"$info")"
  [[ -n $id && -n $secret ]] || return 1
  (umask 077 && printf 'S3_ENDPOINT=http://127.0.0.1:3900\nS3_BUCKET=%s\nS3_ACCESS_KEY_ID=%s\nS3_SECRET_ACCESS_KEY=%s\n' \
    "$P-static" "$id" "$secret" >"$WORK/s3.env")
}

point4() {
  section "point 4: a static archive served from Garage through the gateway"
  setup_garage || { fail "4 Garage bucket and key"; return; }
  local dep="dep-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
  tar -C "$HERE/site" -czf "$WORK/site.tar.gz" .
  mkdir -p "$WORK/unpacked" "$WORK/outside"
  tar -C "$WORK/unpacked" -xzf "$WORK/site.tar.gz"
  echo "S2-SECRET-OUTSIDE-PREFIX" >"$WORK/outside/secret.txt"
  if (set -a; . "$WORK/s3.env"; GW_PREFIX="$dep" "$GW" put "$WORK/unpacked" >/dev/null &&
      GW_PREFIX="other" "$GW" put "$WORK/outside" >/dev/null); then
    pass "4.1 the archive is unpacked and stored in Garage under the deployment's prefix"
  else
    fail "4.1 the archive is unpacked and stored in Garage under the deployment's prefix"
    return
  fi
  local port
  port="$(free_port)"
  (set -a; . "$WORK/s3.env"; GW_LISTEN="127.0.0.1:$port" GW_PREFIX="$dep" exec "$GW" static) 2>"$WORK/static.log" &
  PIDS+=($!)
  wait_http "http://127.0.0.1:$port/" || { fail "4 static gateway starts"; return; }
  local S="http://127.0.0.1:$port"

  local f ok=1 want got
  while IFS= read -r f; do
    want="$(sha256sum <"$WORK/unpacked/$f" | cut -d' ' -f1)"
    got="$(curl -fsS "$S/$f" | sha256sum | cut -d' ' -f1)"
    [[ $want == "$got" ]] || { ok=0; note "differs: $f"; }
  done < <(cd "$WORK/unpacked" && find . -type f | sed 's|^\./||')
  check "4.2 every file comes back byte for byte" test $ok = 1
  check "4.3 / serves index.html" bash -c "[[ \"\$(curl -fsS '$S/' | sha256sum)\" == \"\$(sha256sum <'$WORK/unpacked/index.html')\" ]]"
  local ct
  ct="$(curl -fsS -o /dev/null -w '%{content_type}' "$S/index.html") $(curl -fsS -o /dev/null -w '%{content_type}' "$S/app.js") $(curl -fsS -o /dev/null -w '%{content_type}' "$S/style.css")"
  check "4.4 content types: html, javascript, css" \
    test "$ct" = "text/html; charset=utf-8 text/javascript; charset=utf-8 text/css; charset=utf-8"
  check "4.5 a missing file is 404" test "$(curl -s -o /dev/null -w '%{http_code}' "$S/nope.html")" = 404
  local bad leaked=0 code body
  for bad in "/../other/secret.txt" "/nested/../../other/secret.txt" "/%2e%2e/other/secret.txt" \
    "/..%2fother/secret.txt" "/nested/%2e%2e/%2e%2e/other/secret.txt" "//other/secret.txt" "/.%2e/other/secret.txt"; do
    body="$(curl -s --path-as-is "$S$bad")"
    code="$(curl -s --path-as-is -o /dev/null -w '%{http_code}' "$S$bad")"
    if [[ $body == *S2-SECRET* || $code == 200 ]]; then leaked=1; note "leaked: $bad ($code)"; fi
  done
  check "4.6 no path reaches another prefix of the bucket (.., %2e%2e, %2f, //)" test $leaked = 0
  check "4.7 POST is refused (405)" test "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$S/index.html")" = 405
  local hdr
  hdr="$(curl -fsS -D - -o /dev/null "$S/index.html" | tr -d '\r')"
  check "4.8 static answers carry nosniff and frame-ancestors 'none'" bash -c '
    h=$(cat); grep -qi "^x-content-type-options: nosniff" <<<"$h" &&
    grep -qi "^content-security-policy: frame-ancestors .none." <<<"$h"' <<<"$hdr"
}

# ------------------------------------------------------------------------------------------------
# The egress proxy: the spike gateway binary in `egress` mode, in a container with the driver's
# flags, on an outside network and on the app networks it serves.
HOST_IP="$(ip -4 -o addr show scope global 2>/dev/null | awk '$2 !~ /^(docker|br-|veth)/ {sub(/\/.*/, "", $4); print $4; exit}')"
# The host's global IPv6 address with its prefix (for example x:y::1/64), if it has one.
HOST_IP6_NET="$(ip -6 -o addr show scope global 2>/dev/null | awk '$2 !~ /^(docker|br-|veth)/ {print $4; exit}')"
HOST_IP6="${HOST_IP6_NET%/*}"
EGRESS_IMAGE="${S2_EGRESS_IMAGE:-ubuntu:24.04}" # the same glibc as the host that built the binary
egress_up() { # app networks...
  pull "$EGRESS_IMAGE"
  docker network inspect "$P-outside" >/dev/null 2>&1 ||
    docker network create --label ruum.app=$P "$P-outside" >/dev/null
  if ! docker inspect "$P-egress" >/dev/null 2>&1; then
    docker run -d --name "$P-egress" --network "$P-outside" --read-only --cap-drop ALL \
      --security-opt no-new-privileges --user 10001:10001 --memory 128m --cpus 1 --pids-limit 64 \
      --label ruum.app=$P -v "$GW:/gw:ro" -e GW_LISTEN=0.0.0.0:3128 -e EGRESS_PORTS=443 \
      -e EGRESS_ALLOW="registry.npmjs.org,localhost,169.254.169.254.nip.io,$HOST_IP.nip.io" \
      -e EGRESS_DENY="$HOST_IP${HOST_IP6_NET:+,$HOST_IP6_NET}" "$EGRESS_IMAGE" /gw egress >/dev/null
  fi
  local n
  for n in "$@"; do docker network connect "$P-$n" "$P-egress" 2>/dev/null; done
}
EGRESS_REFUSED='["example.org:443","registry.npmjs.org:22","localhost:443","169.254.169.254.nip.io:443","169.254.169.254:443","'"$HOST_IP"'.nip.io:443","172.18.0.2:5432"]'

# The targets an app or a build must never reach, as JSON for the probe, in TARGETS. Only dev stack
# containers, the host's own addresses and the spike's own containers; never staging or the SSH
# tunnel. An expected target without an address (a container that is down, a network without a
# gateway) is a counted SKIP line, never dropped silently.
probe_targets() { # check prefix, app network, other app container, other app network
  local pre=$1 net=$2 lines=() gw gwname subnet holder bridge ll
  shift
  add() { # name address port
    if [[ $2 =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ || $2 =~ ^[0-9a-fA-F]*:[0-9a-fA-F:.]+(%[A-Za-z0-9_.-]+)?$ ]]; then
      lines+=("$1|$2|$3")
    else
      skip "$pre.target $1" "no address"
    fi
  }
  add "platform Postgres (ruum-postgres-1)" "$(ip_of ruum-postgres-1 ruum_default)" 5432
  add "data cluster Postgres (ruum-postgres-data-1)" "$(ip_of ruum-postgres-data-1 ruum_data)" 5432
  add "SpiceDB gRPC (ruum-spicedb-1)" "$(ip_of ruum-spicedb-1 ruum_default)" 50051
  add "SpiceDB HTTP (ruum-spicedb-1)" "$(ip_of ruum-spicedb-1 ruum_default)" 8443
  add "OpenBao (ruum-openbao-1)" "$(ip_of ruum-openbao-1 ruum_default)" 8200
  add "NATS (ruum-nats-1)" "$(ip_of ruum-nats-1 ruum_default)" 4222
  add "Garage S3 (ruum-garage-1)" "$(ip_of ruum-garage-1 ruum_default)" 3900
  add "Zitadel (ruum-zitadel-1)" "$(ip_of ruum-zitadel-1 ruum_default)" 8080
  add "Caddy (ruum-caddy-1)" "$(ip_of ruum-caddy-1 ruum_default)" 443
  if [[ -n ${BUILDBOX_NAME:-} ]]; then
    add "agent Postgres on the default bridge (pg-$BUILDBOX_NAME)" "$(ip_of "pg-$BUILDBOX_NAME" bridge)" 5432
  fi
  # The host through the app network's bridge. With inhibit_ipv4 Docker reports no gateway; the
  # host would hold the subnet's first host address, so that address is the target.
  gw="$(docker network inspect "$P-$net" -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}')"
  gwname="gateway"
  if [[ ! $gw =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then # Docker prints "invalid IP" for none
    subnet="$(docker network inspect "$P-$net" -f '{{range .IPAM.Config}}{{.Subnet}}{{end}}')"
    gw="$(python3 -c 'import ipaddress, sys; print(next(ipaddress.ip_network(sys.argv[1], strict=False).hosts()))' "$subnet" 2>/dev/null)"
    gwname="would-be gateway $gw (no gateway with inhibit_ipv4; first host of $subnet)"
    holder="$(docker network inspect "$P-$net" -f '{{range .Containers}}{{.Name}}={{.IPv4Address}} {{end}}' | tr ' ' '\n' | grep "=$gw/" | cut -d= -f1)"
    [[ -z $holder ]] || note "INFO the would-be gateway address $gw is held by the container $holder"
  fi
  add "host through the app network's $gwname, sshd" "$gw" 22
  add "host through the app network's $gwname, platform Postgres port" "$gw" 5434
  # IPv6: a bridge gets a link-local address even without an IPv4 address.
  bridge="$(docker network inspect "$P-$net" -f '{{index .Options "com.docker.network.bridge.name"}}' 2>/dev/null)"
  [[ -n $bridge && $bridge != "<no value>" ]] || bridge="br-$(docker network inspect "$P-$net" -f '{{.Id}}' | cut -c1-12)"
  # The finding itself, on the host: the bridge has no IPv4 address, so no host service listens on
  # it. (With inhibit_ipv4 Docker gives the subnet's first address to a container, so a probe of
  # that address alone would not show it.)
  local v4
  v4="$(ip -4 -o addr show dev "$bridge" 2>/dev/null | awk '{print $4}' | tr '\n' ' ')"
  check "$pre.v4 the host has no IPv4 address on the app bridge $bridge${v4:+ (has: $v4)}" test -z "$v4"
  ll="$(ip -6 -o addr show dev "$bridge" scope link 2>/dev/null | awk '{sub(/\/.*/, "", $4); print $4; exit}')"
  note "IPv6 of the app bridge $bridge: disable_ipv6=$(cat "/proc/sys/net/ipv6/conf/$bridge/disable_ipv6" 2>/dev/null || echo '?'), link-local ${ll:-none}"
  if [[ -n $ll ]]; then
    add "host through the app bridge's IPv6 link-local address, sshd" "$ll%eth0" 22
  else
    pass "$pre.v6 the app bridge $bridge has no IPv6 link-local address (no IPv6 path to the host through it)"
  fi
  add "host public IPv6 address, sshd" "$HOST_IP6" 22
  add "host through ruum_default's gateway, sshd" "$(docker network inspect ruum_default -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}' 2>/dev/null)" 22
  add "host through docker0, sshd" 172.17.0.1 22
  add "host through docker0, platform Postgres port" 172.17.0.1 5434
  add "host public address, sshd" "$HOST_IP" 22
  add "metadata address TCP 80" 169.254.169.254 80
  add "internet direct 1.1.1.1:443" 1.1.1.1 443
  add "internet direct 8.8.8.8:53" 8.8.8.8 53
  [[ -n ${2:-} ]] && add "another app ($2)" "$(ip_of "$P-$2" "$P-$3")" 8080
  TARGETS="$(printf '%s\n' "${lines[@]}" | jq -R -s -c 'split("\n") | map(select(length > 0) | split("|") | {name: .[0], host: .[1], port: (.[2] | tonumber)})')"
}

# Checks of a probe result: nothing reached except the allowed egress target.
probe_checks() { # prefix result.json
  local pre=$1 f=$2 k d i=0
  [[ -s $f ]] || { fail "$pre the probe ran"; return; }
  while IFS=$'\t' read -r k d; do
    i=$((i + 1))
    case $k in
      "egress allowed "*) check "$pre.$i $k is reached through the egress proxy ($d)" jqe ".\"$k\".reached == true" <"$f" ;;
      *) check "$pre.$i $k: not reached ($d)" jqe ".\"$k\".reached == false" <"$f" ;;
    esac
  done < <(jq -r 'to_entries[] | select(.key != "self") | [.key, .value.detail] | @tsv' "$f")
  check "$pre.self runs as a non-root user with no capabilities" jqe '.self.uid != 0 and .self.cap_eff == "0000000000000000" and .self.cap_bnd == "0000000000000000"' <"$f"
  check "$pre.self has no_new_privs and a seccomp filter (mode 2)" jqe '.self.no_new_privs == "1" and .self.seccomp == "2"' <"$f"
  check "$pre.self has a read-only root and no Docker socket" jqe '.self.root_writable == false and .self.docker_socket == false' <"$f"
  check "$pre.self has no secret-like environment variables" jqe '.self.env_keys == []' <"$f"
}

point1() { # [tag: r for runsc]
  local t=${1:-}
  section "point 1${t:+ ($t)}: a Node app in plain Docker with the driver's flags reaches nothing but the egress proxy"
  net p1$t-a "${APP_NET_OPTS[@]}"
  net p1$t-b "${APP_NET_OPTS[@]}"
  app p1$t-other p1$t-b "$AUD_B"
  egress_up p1$t-a p1$t-b
  local eip targets
  eip="$(ip_of "$P-egress" "$P-p1$t-a")"
  [[ -n $eip ]] || { fail "1$t egress proxy on the app network"; return; }
  probe_targets "1$t" p1$t-a p1$t-other p1$t-b
  targets=$TARGETS
  note "targets: $(jq -r 'map("\(.host):\(.port)") | join(" ")' <<<"$targets")"
  mkdir -p "$WORK/p1$t" && chmod 777 "$WORK/p1$t"
  docker run --rm --name "$P-probe-p1$t" --network "$P-p1$t-a" "${DRIVER_FLAGS[@]}" \
    -v "$HERE/probe:/probe:ro" -v "$WORK/p1$t:/out" -e PROBE_OUT=/out/result.json \
    -e PROBE_TARGETS="$targets" -e EGRESS="$eip:3128" -e EGRESS_ALLOWED='["registry.npmjs.org:443"]' \
    -e EGRESS_REFUSED="$EGRESS_REFUSED" "$NODE_IMAGE" node /probe/probe.js
  probe_checks "1$t.p" "$WORK/p1$t/result.json"
  note "IPv6 inside the app container: $(jq -r '.self.ipv6 | to_entries | map("\(.key)=\(.value)") | join(" ")' "$WORK/p1$t/result.json" 2>/dev/null)"
  local cfg
  cfg="$(docker inspect "$P-p1$t-other" -f '{{.HostConfig.ReadonlyRootfs}} {{.HostConfig.CapDrop}} {{.HostConfig.SecurityOpt}} {{.Config.User}} {{.HostConfig.Memory}} {{.HostConfig.NanoCpus}} {{.HostConfig.PidsLimit}} {{.HostConfig.Privileged}} {{len .Mounts}}')"
  note "app container: $cfg"
  check "1$t.cfg the app container has the driver's flags (read-only, cap-drop ALL, no-new-privileges, user, limits, no mounts)" \
    test "$cfg" = "true [ALL] [no-new-privileges] 10001:10001 268435456 1000000000 128 false 0"
  # For the record: the same probe on a plain internal network (no inhibit_ipv4).
  net p1$t-plain
  local gw
  gw="$(docker network inspect "$P-p1$t-plain" -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}')"
  mkdir -p "$WORK/p1$t-plain" && chmod 777 "$WORK/p1$t-plain"
  docker run --rm --network "$P-p1$t-plain" "${DRIVER_FLAGS[@]}" -v "$HERE/probe:/probe:ro" \
    -v "$WORK/p1$t-plain:/out" -e PROBE_OUT=/out/result.json \
    -e PROBE_TARGETS="[{\"name\":\"sshd\",\"host\":\"$gw\",\"port\":22},{\"name\":\"pg\",\"host\":\"$gw\",\"port\":5434}]" \
    "$NODE_IMAGE" node /probe/probe.js
  note "INFO plain internal network (no inhibit_ipv4), host through its gateway $gw: sshd $(jq -r .sshd.detail "$WORK/p1$t-plain/result.json"), port 5434 $(jq -r .pg.detail "$WORK/p1$t-plain/result.json")"
  local log
  log="$(docker logs "$P-egress" 2>&1)"
  check "1$t.log the egress proxy logs host, port, status and bytes, and no path" bash -c '
    l=$(cat); grep -q "host=registry.npmjs.org port=443 status=200 bytes_up=[0-9]* bytes_down=[0-9]*" <<<"$l" &&
    grep -q "host=example.org port=443 status=403" <<<"$l" && ! grep -q "GET\|HEAD /" <<<"$l"' <<<"$log"
}

# ------------------------------------------------------------------------------------------------
point3() {
  section "point 3: cold start of a small Node app from a stopped container (decision 25: 3 s)"
  net p3
  local runtime results
  for runtime in runc ${RUNSC_RUNTIME:-}; do
    docker rm -f "$P-cold" >/dev/null 2>&1
    docker create --name "$P-cold" --network "$P-p3" --runtime "$runtime" "${DRIVER_FLAGS[@]}" \
      -e RUUM_ISSUER="$ISS" -e RUUM_AUDIENCE="$AUD_A" -e RUUM_ENV=live "$P-app:1" >/dev/null
    results="$(python3 - "$P-cold" "${S2_COLD_RUNS:-20}" <<'PY'
import json, socket, statistics, subprocess, sys, time
name, runs = sys.argv[1], int(sys.argv[2])
def ip():
    return subprocess.check_output(["docker", "inspect", "-f",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", name]).decode().strip()
def ready(addr, deadline):
    while time.monotonic() < deadline:
        try:
            s = socket.create_connection((addr, 8080), timeout=0.2)
            s.sendall(b"GET /healthz HTTP/1.0\r\n\r\n")
            d = s.recv(64)
            s.close()
            if d.startswith(b"HTTP/1.1 200"):
                return True
        except OSError:
            pass
        time.sleep(0.005)
    return False
times = []
for _ in range(runs):
    subprocess.run(["docker", "stop", "-t", "0", name], capture_output=True)
    t0 = time.monotonic()
    subprocess.run(["docker", "start", name], check=True, capture_output=True)
    ok = ready(ip(), t0 + 10)
    times.append(time.monotonic() - t0 if ok else 99.0)
times.sort()
p95 = times[min(len(times) - 1, int(round(0.95 * len(times))) - 1)]
print(json.dumps({"runs": runs, "median": round(statistics.median(times), 3), "p95": round(p95, 3),
                  "min": round(times[0], 3), "max": round(times[-1], 3)}))
PY
)"
    note "$runtime start from stopped: $results"
    check "3.$runtime p95 of $(jq -r .runs <<<"$results") starts from a stopped container is under 3 s" \
      jqe '.p95 < 3' <<<"$results"
    echo "$runtime $results" >>"$WORK/cold.txt"
  done
  docker rm -f "$P-cold" >/dev/null 2>&1
  # For comparison: a new container each time (create, start, first answer, remove).
  results="$(python3 - "$P" "$P-p3" "$P-app:1" "${S2_COLD_RUNS:-20}" "${DRIVER_FLAGS[@]}" <<'PY'
import json, socket, statistics, subprocess, sys, time
p, network, image, runs, flags = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5:]
times = []
for i in range(runs):
    name = f"{p}-coldrun-{i}"
    t0 = time.monotonic()
    subprocess.run(["docker", "run", "-d", "--name", name, "--network", network, *flags, image],
                   check=True, capture_output=True)
    addr = subprocess.check_output(["docker", "inspect", "-f",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", name]).decode().strip()
    ok = False
    while time.monotonic() < t0 + 10 and not ok:
        try:
            s = socket.create_connection((addr, 8080), timeout=0.2)
            s.sendall(b"GET /healthz HTTP/1.0\r\n\r\n")
            ok = s.recv(64).startswith(b"HTTP/1.1 200")
            s.close()
        except OSError:
            time.sleep(0.005)
    times.append(time.monotonic() - t0 if ok else 99.0)
    subprocess.run(["docker", "rm", "-f", name], capture_output=True)
times.sort()
p95 = times[min(len(times) - 1, int(round(0.95 * len(times))) - 1)]
print(json.dumps({"runs": runs, "median": round(statistics.median(times), 3), "p95": round(p95, 3),
                  "min": round(times[0], 3), "max": round(times[-1], 3)}))
PY
)"
  note "runc new container (docker run): $results"
  echo "runc-new $results" >>"$WORK/cold.txt"
}

# ------------------------------------------------------------------------------------------------
point5() {
  section "point 5: a hostile package.json builds in a sandbox job and reaches nothing"
  net build "${APP_NET_OPTS[@]}"
  net p5-other "${APP_NET_OPTS[@]}"
  app p5-other p5-other "$AUD_B"
  egress_up build
  local eip targets rc
  eip="$(ip_of "$P-egress" "$P-build")"
  probe_targets 5 build p5-other p5-other
  targets=$TARGETS
  mkdir -p "$WORK/p5" && chmod 777 "$WORK/p5"
  # The fixed node22 recipe, step 1: npm ci as a sandbox job (the driver's flags, a tmpfs work
  # folder, a time limit, no secrets, egress only through the proxy).
  timeout 300 docker run --rm --name "$P-build-job" --network "$P-build" \
    --read-only --cap-drop ALL --security-opt no-new-privileges --user 10001:10001 \
    --memory 512m --memory-swap 512m --cpus 2 --pids-limit 256 --label ruum.app=$P \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=128m \
    --tmpfs /work:rw,noexec,nosuid,nodev,size=256m,uid=10001,gid=10001 \
    -v "$HERE/hostile:/src:ro" -v "$HERE/probe:/probe:ro" -v "$WORK/p5:/out" \
    -e HOME=/tmp -e npm_config_cache=/tmp/npm-cache -e npm_config_update_notifier=false \
    -e HTTPS_PROXY="http://$eip:3128" -e https_proxy="http://$eip:3128" \
    -e PROBE_TARGETS="$targets" -e EGRESS="$eip:3128" -e EGRESS_ALLOWED='[]' \
    -e EGRESS_REFUSED="$EGRESS_REFUSED" \
    "$NODE_IMAGE" sh -c 'cp -r /src/. /work/ && cd /work && npm ci --no-audit --no-fund --foreground-scripts \
      && tar -C /work -cf /out/layer.tar package.json node_modules' >"$WORK/p5/npm.log" 2>&1
  rc=$?
  check "5.1 npm ci of the hostile package succeeds in the sandbox job (exit $rc)" test $rc = 0
  [[ $rc == 0 ]] || sed 's/^/      npm: /' "$WORK/p5/npm.log" | tail -20
  check "5.2 the dependency came through the egress proxy" bash -c "docker logs '$P-egress' 2>&1 | grep -q 'host=registry.npmjs.org port=443 status=200'"
  check "5.3 the postinstall ran and wrote its probe result" test -s "$WORK/p5/postinstall.json"
  probe_checks 5.p "$WORK/p5/postinstall.json"
  # Step 2 of the recipe: the layer is a plain tar; the app runs from it in a sandbox.
  if [[ -s $WORK/p5/layer.tar ]]; then
    note "layer: $(sha256sum "$WORK/p5/layer.tar" | cut -c1-71) ($(stat -c %s "$WORK/p5/layer.tar") bytes)"
    mkdir -p "$WORK/p5/app" && tar -C "$WORK/p5/app" -xf "$WORK/p5/layer.tar"
    check "5.4 the built layer runs in a sandbox (require('ms'))" bash -c "
      [[ \$(docker run --rm --network none ${DRIVER_FLAGS[*]} -v '$WORK/p5/app:/app:ro' -w /app '$NODE_IMAGE' \
        node -e \"process.stdout.write(String(require('ms')('2h')))\") == 7200000 ]]"
    [[ ${#FAILED[@]} -gt 0 && ${FAILED[${#FAILED[@]}-1]} == 5.4* ]] && { tar -tvf "$WORK/p5/layer.tar" | head -8; ls -la "$WORK/p5/app" "$WORK/p5/app/node_modules"; } | sed 's/^/      /'
  else
    fail "5.4 the built layer runs in a sandbox" "no layer"
  fi
  # Rootless BuildKit under the same flags (for the record; the recipe above does not need it).
  local bk="${S2_BUILDKIT_IMAGE:-moby/buildkit:v0.23.2-rootless}" out
  pull "$bk"
  mkdir -p "$WORK/p5/bkctx" && printf 'FROM scratch\nCOPY a /a\n' >"$WORK/p5/bkctx/Dockerfile" && echo a >"$WORK/p5/bkctx/a"
  out="$(timeout 120 docker run --rm --name "$P-buildkit" --network "$P-build" \
    --read-only --cap-drop ALL --security-opt no-new-privileges --user 1000:1000 \
    --memory 512m --cpus 2 --pids-limit 256 --label ruum.app=$P \
    --tmpfs /tmp:rw,size=64m --tmpfs /home/user/.local:rw,size=256m,uid=1000,gid=1000 \
    --tmpfs /run/user/1000:rw,size=16m,uid=1000,gid=1000 \
    -e BUILDKITD_FLAGS=--oci-worker-no-process-sandbox -v "$WORK/p5/bkctx:/ctx:ro" \
    --entrypoint sh "$bk" -c 'mkdir -p ~/.local/tmp ~/.local/share/buildkit && exec buildctl-daemonless.sh \
      build --frontend dockerfile.v0 --local context=/ctx --local dockerfile=/ctx \
      --output type=tar,dest=/tmp/out.tar' 2>&1)"
  BUILDKIT_RC=$?
  BUILDKIT_OUT="$(tail -3 <<<"$out" | tr '\n' ' ' | cut -c1-300)"
  note "INFO rootless BuildKit with the driver's flags: exit $BUILDKIT_RC: $BUILDKIT_OUT"
}

# ------------------------------------------------------------------------------------------------
pointr() {
  section "runsc (gVisor)"
  if [[ -n ${RUNSC_RUNTIME:-} ]]; then
    note "Docker knows the runsc runtime; point 1 again under runsc (point 3 measured it too)"
    local saved=("${DRIVER_FLAGS[@]}")
    DRIVER_FLAGS+=(--runtime runsc)
    point1 r
    DRIVER_FLAGS=("${saved[@]}")
  elif [[ -x $HOME/bin/runsc ]]; then
    local out
    if out="$("$HOME/bin/runsc" --rootless --network=none do /bin/true 2>&1)"; then
      pass "r.1 runsc --rootless do runs a process"
    else
      skip "r.1 runsc --rootless" "fails: $(tail -1 <<<"$out" | cut -c1-200)"
    fi
  else
    skip "r runsc checks" "runsc is not installed and needs root; see README.md, 'runsc'"
  fi
}

want 2 && point2
want 4 && point4
want 1 && point1
want 3 && point3
want 5 && point5
want r && pointr

section "result"
printf 'PASS %d  FAIL %d  SKIP %d\n' "$PASS" "$FAIL" "$SKIP"
for f in "${FAILED[@]}"; do printf '  failed: %s\n' "$f"; done
[[ $FAIL -eq 0 ]]
