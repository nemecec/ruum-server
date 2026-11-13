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
# Base images by digest (multi-arch indexes, 2026-11-13), not by a tag that can move.
NODE_IMAGE="${S2_NODE_IMAGE:-node:22-alpine@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a}"
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
jqf() { jq -e "$@" >/dev/null 2>&1; } # jq options... filter file
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
# The gateway's unit tests (header, cookie, CIDR and address rules) run first; a failure stops the run.
(cd "$HERE/gw" && cargo test --release --locked -q >"$WORK/gw-test.log" 2>&1) ||
  { tail -30 "$WORK/gw-test.log" >&2; echo "gateway unit tests failed" >&2; exit 2; }
note "gateway unit tests: $(grep -o '[0-9]* passed' "$WORK/gw-test.log" | head -1)"
pull "$NODE_IMAGE"
note "node image: $NODE_IMAGE"
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
  check "2.8 an app's Set-Cookie for __Host-ruum_*, ruum_*, RUUM, an empty name (=ruum_app=x) or no = (ruum_app) is dropped; its own stays" \
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
  out="$(curl -fsS "http://$ipb:8080/" -H "Ruum-User: $(token "$WORK/other.pem" "$AUD_B" '{}')")"
  check "2.11 a token signed with another key fails the SDK check (signature)" \
    jqe '.ruum_user.valid == false and .ruum_user.reason == "signature"' <<<"$out"
  # Tokens signed with the right key and one claim wrong: each fails app B's check with its reason.
  local spec claims reason
  for spec in \
    'iss|{"iss":"https://evil.example"}' 'typ|{"typ":"agent"}' 'env|{"env":"preview"}' \
    'exp|{"iat":null}' 'exp|{"iat":"0"}' 'exp|{"exp":null}' 'exp|{"exp_offset":-1}' 'exp|{"exp_offset":0}' \
    'exp|{"exp_offset":61}' 'alg|{"alg":"none"}'; do
    reason=${spec%%|*} claims=${spec#*|}
    out="$(curl -fsS "http://$ipb:8080/" -H "Ruum-User: $(token "$WORK/user-token.pem" "$AUD_B" "$claims")")"
    check "2.13 the SDK check refuses a token with $claims ($reason)" \
      jqe '.ruum_user.valid == false and .ruum_user.reason == "'"$reason"'"' <<<"$out"
  done
  out="$(curl -fsS "http://$ipb:8080/" -H "Ruum-User: $(token "$WORK/user-token.pem" "$AUD_B" '{}')")"
  check "2.14 the same helper's well-formed token passes app B's check (the negative cases above are not vacuous)" \
    jqe '.ruum_user.valid == true' <<<"$out"
}

# A Ruum-User token signed with key for audience aud; overrides is JSON: claim values (null removes
# the claim), "alg", and "exp_offset" (exp = now + offset, default 60).
token() { # key aud overrides
  python3 - "$1" "$ISS" "$2" "$3" <<'PY'
import base64, json, subprocess, sys, time
key, iss, aud, over = sys.argv[1], sys.argv[2], sys.argv[3], json.loads(sys.argv[4])
b = lambda d: base64.urlsafe_b64encode(d).rstrip(b"=").decode()
now = int(time.time())
claims = {"iss": iss, "aud": aud, "typ": "app_user", "env": "live", "sub": "pairwise:x",
          "tenant": "t", "roles": ["admin"], "node_roles": {}, "actor": "user",
          "iat": now, "exp": now + over.pop("exp_offset", 60), "jti": "x"}
alg = over.pop("alg", "EdDSA")
for k, v in over.items():
    if v is None:
        claims.pop(k, None)
    else:
        claims[k] = v
h = b(json.dumps({"alg": alg, "typ": "JWT"}).encode())
c = b(json.dumps(claims).encode())
msg = key + ".msg"
with open(msg, "wb") as f:
    f.write(f"{h}.{c}".encode())
sig = subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", key, "-rawin", "-in", msg],
                     capture_output=True, check=True).stdout
print(f"{h}.{c}.{b(sig)}")
PY
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
  # The object outside the deployment's prefix exists and holds the secret: read it back through a
  # second static gateway on the prefix "other", so the traversal checks below test a real object.
  local oport
  oport="$(free_port)"
  (set -a; . "$WORK/s3.env"; GW_LISTEN="127.0.0.1:$oport" GW_PREFIX="other" exec "$GW" static) 2>"$WORK/static-other.log" &
  PIDS+=($!)
  if wait_http "http://127.0.0.1:$oport/secret.txt"; then
    check "4.6a the outside object other/secret.txt exists in the bucket (read back on its own prefix)" \
      test "$(curl -fsS "http://127.0.0.1:$oport/secret.txt")" = "S2-SECRET-OUTSIDE-PREFIX"
  else
    fail "4.6a the outside object other/secret.txt exists in the bucket (read back on its own prefix)"
  fi
  local bad bad_ok=1 code body
  for bad in "/../other/secret.txt" "/nested/../../other/secret.txt" "/%2e%2e/other/secret.txt" \
    "/..%2fother/secret.txt" "/nested/%2e%2e/%2e%2e/other/secret.txt" "//other/secret.txt" "/.%2e/other/secret.txt"; do
    body="$(curl -s --path-as-is -w '\n%{http_code}' "$S$bad")"
    code="${body##*$'\n'}"
    body="${body%$'\n'*}"
    # Only the gateway's own refusal counts: 404 or 400. 000 (no answer), 5xx or 200 is a FAIL.
    if [[ $body == *S2-SECRET* || ! $code =~ ^(400|404)$ ]]; then bad_ok=0; note "not refused: $bad ($code)"; fi
  done
  check "4.6 every path to another prefix (.., %2e%2e, %2f, //) is refused with 404 or 400 and no secret" test $bad_ok = 1
  check "4.6b the static gateway still answers after the traversal attempts" \
    test "$(curl -s -o /dev/null -w '%{http_code}' "$S/index.html")" = 200
  check "4.7 POST is refused (405)" test "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$S/index.html")" = 405
  local hdr
  hdr="$(curl -fsS -D - -o /dev/null "$S/index.html" | tr -d '\r')"
  check "4.8 static answers carry nosniff and frame-ancestors 'none'" bash -c '
    h=$(cat); grep -qi "^x-content-type-options: nosniff" <<<"$h" &&
    grep -qi "^content-security-policy: frame-ancestors .none." <<<"$h"' <<<"$hdr"
}

# ------------------------------------------------------------------------------------------------
# The egress proxy: the spike gateway binary in `egress` mode, in a container with the driver's
# flags, on an outside network and on the app networks it serves. Point 1 and point 5 each get
# their own proxy container, so the log of one point never answers a check of the other.
HOST_IP="$(ip -4 -o addr show scope global 2>/dev/null | awk '$2 !~ /^(docker|br-|veth)/ {sub(/\/.*/, "", $4); print $4; exit}')"
# The host's global IPv6 address with its prefix (for example x:y::1/64), if it has one.
HOST_IP6_NET="$(ip -6 -o addr show scope global 2>/dev/null | awk '$2 !~ /^(docker|br-|veth)/ {print $4; exit}')"
HOST_IP6="${HOST_IP6_NET%/*}"
# Another address in the host's IPv6 prefix (the deny list takes the whole /64, not one address).
HOST_IP6_OTHER="$([[ -n $HOST_IP6_NET ]] && python3 -c 'import ipaddress, sys; n = ipaddress.ip_interface(sys.argv[1]).network; print(n.network_address + 0x5eed)' "$HOST_IP6_NET" 2>/dev/null)"
# The same glibc as the host that built the binary; by digest (ubuntu:24.04, 2026-11-13).
EGRESS_IMAGE="${S2_EGRESS_IMAGE:-ubuntu:24.04@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5}"
# Address forms that the allow-list lets through, so only the address rules (private, link-local,
# loopback, ULA, EGRESS_DENY, IPv4-mapped) can refuse them: decimal, octal and hex IPv4 (glibc's
# getaddrinfo reads them as numbers), short forms and IPv6 literals.
EGRESS_ALLOW="registry.npmjs.org,localhost,169.254.169.254.nip.io,$HOST_IP.nip.io,169.254.169.254,172.18.0.2"
EGRESS_ALLOW+=",2852039166,0251.0376.0251.0376,0xa9fea9fe,127.1,::ffff:169.254.169.254,::ffff:a9fe:a9fe,::1,fd00:ec2::254"
EGRESS_ALLOW+=",$HOST_IP${HOST_IP6:+,$HOST_IP6}${HOST_IP6_OTHER:+,$HOST_IP6_OTHER},::ffff:$HOST_IP"
egress_up() { # container app-networks...
  local c=$1 n
  shift
  pull "$EGRESS_IMAGE"
  docker network inspect "$P-outside" >/dev/null 2>&1 ||
    docker network create --label ruum.app=$P "$P-outside" >/dev/null
  if ! docker inspect "$c" >/dev/null 2>&1; then
    docker run -d --name "$c" --network "$P-outside" --read-only --cap-drop ALL \
      --security-opt no-new-privileges --user 10001:10001 --memory 128m --cpus 1 --pids-limit 64 \
      --label ruum.app=$P -v "$GW:/gw:ro" -e GW_LISTEN=0.0.0.0:3128 -e EGRESS_PORTS=443 \
      -e EGRESS_ALLOW="$EGRESS_ALLOW" \
      -e EGRESS_DENY="$HOST_IP${HOST_IP6_NET:+,$HOST_IP6_NET}" "$EGRESS_IMAGE" /gw egress >/dev/null
  fi
  for n in "$@"; do docker network connect "$P-$n" "$c" 2>/dev/null; done
}
# Every refused CONNECT with the reason the proxy must give (its X-Ruum-Egress-Deny header). A 502,
# a timeout, a TLS error after a 200 or a 403 with another reason is a FAIL.
EGRESS_REFUSED_SPEC=(
  "example.org:443|allow-list" "registry.npmjs.org:22|port" "localhost:443|address"
  "169.254.169.254.nip.io:443|address" "169.254.169.254:443|address" "$HOST_IP.nip.io:443|address"
  "$HOST_IP:443|address" "[::ffff:$HOST_IP]:443|address" "172.18.0.2:443|address"
  "2852039166:443|address" "0251.0376.0251.0376:443|address" "0xa9fea9fe:443|address" "127.1:443|address"
  "[::ffff:169.254.169.254]:443|address" "[::ffff:a9fe:a9fe]:443|address" "[::1]:443|address"
  "[fd00:ec2::254]:443|address"
)
[[ -n $HOST_IP6 ]] && EGRESS_REFUSED_SPEC+=("[$HOST_IP6]:443|address")
[[ -n $HOST_IP6_OTHER ]] && EGRESS_REFUSED_SPEC+=("[$HOST_IP6_OTHER]:443|address")
EGRESS_REASONS="$(printf '%s\n' "${EGRESS_REFUSED_SPEC[@]}" | jq -R -s -c 'split("\n") | map(select(length > 0) | capture("^(?<t>.*)\\|(?<r>[^|]*)$")) | map({key: .t, value: .r}) | from_entries')"
EGRESS_REFUSED="$(jq -c 'keys_unsorted' <<<"$EGRESS_REASONS")"

# The targets an app or a build must never reach, as JSON for the probe, in TARGETS:
# [{name, host, port, control}]. Only dev stack containers, the host's own addresses and the
# spike's own containers; never staging or the SSH tunnel. Every target has a positive control
# that must reach it (run_controls): "host" (a connect from the host to the same address),
# "host:<address>" (from the host to that address, for a link-local address with another scope),
# "net:<network>" (from a container on that network) or "plain:<name>" (the result <name> of the
# probe on a plain internal network, see plain_control). An expected target without an address is
# a FAIL, never a SKIP. Host ports that listen on 127.0.0.1 only (the dev stack's published ports,
# such as 5434) are not targets: no control can reach them on another address, so their probe
# would pass whatever the network does.
probe_targets() { # check prefix, app network, other app container, other app network
  local pre=$1 net=$2 lines=() gw gwname subnet holder bridge ll
  shift
  add() { # name address port control
    if [[ $2 =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ || $2 =~ ^[0-9a-fA-F]*:[0-9a-fA-F:.]+(%[A-Za-z0-9_.-]+)?$ ]]; then
      lines+=("$1|$2|$3|${4:-host}")
    else
      fail "$pre.target $1 has an address" "none found"
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
  # host would hold the subnet's first host address, so that address is the target. Its control is
  # the plain internal network, where the host does hold the gateway address and sshd answers.
  gw="$(docker network inspect "$P-$net" -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}')"
  gwname="gateway"
  if [[ ! $gw =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then # Docker prints "invalid IP" for none
    subnet="$(docker network inspect "$P-$net" -f '{{range .IPAM.Config}}{{.Subnet}}{{end}}')"
    gw="$(python3 -c 'import ipaddress, sys; print(next(ipaddress.ip_network(sys.argv[1], strict=False).hosts()))' "$subnet" 2>/dev/null)"
    gwname="would-be gateway $gw (no gateway with inhibit_ipv4; first host of $subnet)"
    holder="$(docker network inspect "$P-$net" -f '{{range .Containers}}{{.Name}}={{.IPv4Address}} {{end}}' | tr ' ' '\n' | grep "=$gw/" | cut -d= -f1)"
    [[ -z $holder ]] || note "INFO the would-be gateway address $gw is held by the container $holder"
  fi
  add "host through the app network's $gwname, sshd" "$gw" 22 plain:sshd
  # The finding itself, on the host: the bridge has no IPv4 address, so no host service listens on
  # it. (With inhibit_ipv4 Docker gives the subnet's first address to a container, so a probe of
  # that address alone would not show it.) Every step must work first: the network exists, its
  # bridge device exists, and the address list and the IPv6 setting can be read. An empty answer
  # from a missing device is a FAIL, not "no address".
  bridge=""
  if docker network inspect "$P-$net" >/dev/null 2>&1; then
    bridge="$(docker network inspect "$P-$net" -f '{{index .Options "com.docker.network.bridge.name"}}' 2>/dev/null)"
    [[ -n $bridge && $bridge != "<no value>" ]] || bridge="br-$(docker network inspect "$P-$net" -f '{{.Id}}' | cut -c1-12)"
  fi
  if [[ -n $bridge ]] && ip link show dev "$bridge" >/dev/null 2>&1; then
    pass "$pre.br the app network's bridge device $bridge exists on the host"
    local v4 v4rc dis6
    v4="$(ip -4 -o addr show dev "$bridge" 2>/dev/null)"
    v4rc=$?
    v4="$(awk '{print $4}' <<<"$v4" | tr '\n' ' ' | sed 's/ *$//')"
    check "$pre.v4 the host has no IPv4 address on the app bridge $bridge${v4:+ (has: $v4)}" test "$v4rc" = 0 -a -z "$v4"
    dis6="$(cat "/proc/sys/net/ipv6/conf/$bridge/disable_ipv6" 2>/dev/null)"
    ll="$(ip -6 -o addr show dev "$bridge" scope link 2>/dev/null | awk '{sub(/\/.*/, "", $4); print $4; exit}')"
    note "IPv6 of the app bridge $bridge: disable_ipv6=${dis6:-unreadable}, link-local ${ll:-none}"
    if [[ ! $dis6 =~ ^[01]$ ]]; then
      fail "$pre.v6 the IPv6 setting of the app bridge $bridge is readable"
    elif [[ -n $ll ]]; then
      add "host through the app bridge's IPv6 link-local address, sshd" "$ll%eth0" 22 "host:$ll%$bridge"
    else
      pass "$pre.v6 the app bridge $bridge has no IPv6 link-local address (no IPv6 path to the host through it)"
    fi
  else
    fail "$pre.br the app network's bridge device exists on the host" "network $P-$net, bridge '${bridge:-none}'"
  fi
  add "host public IPv6 address, sshd" "$HOST_IP6" 22
  add "host through ruum_default's gateway, sshd" "$(docker network inspect ruum_default -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}' 2>/dev/null)" 22
  add "host through docker0, sshd" 172.17.0.1 22
  add "host public address, sshd" "$HOST_IP" 22
  add "metadata address TCP 80" 169.254.169.254 80
  add "internet direct 1.1.1.1:443" 1.1.1.1 443
  add "internet direct 8.8.8.8:53" 8.8.8.8 53
  [[ -n ${2:-} ]] && add "another app ($2)" "$(ip_of "$P-$2" "$P-$3")" 8080 "net:$P-$3"
  TARGETS="$(printf '%s\n' "${lines[@]}" | jq -R -s -c 'split("\n") | map(select(length > 0) | split("|") | {name: .[0], host: .[1], port: (.[2] | tonumber), control: .[3]})')"
}

# The positive control of the host path: the same probe on a plain internal network (no
# inhibit_ipv4), where the host holds the gateway address. sshd must answer there, or the
# "would-be gateway" target of the app network proves nothing. Run once per run.
plain_control() {
  [[ -s $WORK/plain/result.json ]] && return 0
  net plain
  local gw
  gw="$(docker network inspect "$P-plain" -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}')"
  mkdir -p "$WORK/plain" && chmod 777 "$WORK/plain"
  docker run --rm --network "$P-plain" "${DRIVER_FLAGS[@]}" -v "$HERE/probe:/probe:ro" \
    -v "$WORK/plain:/out" -e PROBE_OUT=/out/result.json -e PROBE_ONLY_TARGETS=1 \
    -e PROBE_TARGETS="[{\"name\":\"sshd\",\"host\":\"$gw\",\"port\":22}]" \
    "$NODE_IMAGE" node /probe/probe.js
  check "plain.1 control: on a plain internal network (no inhibit_ipv4) the host's sshd IS reached through the gateway $gw ($(jq -r .sshd.detail "$WORK/plain/result.json" 2>/dev/null))" \
    jqe '.sshd.reached == true' <"$WORK/plain/result.json"
}

host_tcp() { # address port: prints the outcome, "connected" when the host reaches it
  python3 - "$1" "$2" <<'PY'
import socket, sys
host, port = sys.argv[1], int(sys.argv[2])
try:
    fam, typ, proto, _, addr = socket.getaddrinfo(host, port, 0, socket.SOCK_STREAM)[0]
    s = socket.socket(fam, typ, proto)
    s.settimeout(3)
    s.connect(addr)
    s.close()
    print("connected")
except Exception as e:
    print(type(e).__name__)
PY
}

# The positive controls of one probe run, in a JSON file {key: {reached, detail}}: one per TCP
# target (its control), the metadata GET (from the host) and the DNS names (resolved on the host).
run_controls() { # targets-json out-file
  local name host port ctl res c='{}'
  while IFS=$'\t' read -r name host port ctl; do
    case $ctl in
      host) res="$(host_tcp "$host" "$port")" ;;
      host:*) res="$(host_tcp "${ctl#host:}" "$port")" ;;
      plain:*) res="$(jq -r --arg k "${ctl#plain:}" '.[$k].detail // "no plain result"' "$WORK/plain/result.json" 2>/dev/null)" ;;
      net:*)
        res="$(docker run --rm --network "${ctl#net:}" "${DRIVER_FLAGS[@]}" -v "$HERE/probe:/probe:ro" \
          -e PROBE_ONLY_TARGETS=1 -e PROBE_TARGETS="[{\"name\":\"c\",\"host\":\"$host\",\"port\":$port}]" \
          "$NODE_IMAGE" node /probe/probe.js 2>/dev/null | jq -r '.c.detail')" ;;
      *) res="unknown control $ctl" ;;
    esac
    c="$(jq -c --arg k "$name" --arg d "${res:-no answer}" '.[$k] = {reached: ($d == "connected"), detail: ("control " + $d)}' <<<"$c")"
  done < <(jq -r '.[] | [.name, .host, (.port | tostring), .control] | @tsv' <<<"$1")
  res="$(curl -s -o /dev/null -m 3 -w '%{http_code}' http://169.254.169.254/ 2>/dev/null)"
  c="$(jq -c --arg d "$res" '.["metadata HTTP GET 169.254.169.254"] = {reached: ($d != "000" and $d != ""), detail: ("control host HTTP " + $d)}' <<<"$c")"
  for name in registry.npmjs.org example.com; do
    res="$(getent ahosts "$name" | awk 'NR==1 {print $1}')"
    c="$(jq -c --arg k "DNS $name" --arg d "$res" '.[$k] = {reached: ($d != ""), detail: ("control host resolves " + (if $d == "" then "nothing" else $d end))}' <<<"$c")"
  done
  echo "$c" >"$2"
}

# Checks of a probe result. The probe must answer for exactly the expected keys. A TCP target, the
# metadata GET and the DNS names pass only when the sandbox did not reach them AND their positive
# control did: a timeout or an error alone is no proof (a dead target looks the same). A refused
# egress request passes only on the proxy's own deny answer with the expected reason; the allowed
# one only with an HTTPS answer through the tunnel.
probe_checks() { # prefix result.json targets-json controls.json egress(0|1) allowed-json
  local pre=$1 f=$2 tj=$3 cf=$4 eg=$5 allowed=${6:-'[]'} k i=0 want got
  [[ -s $f ]] || { fail "$pre the probe ran"; return; }
  want="$(jq -c --argjson eg "$eg" --argjson allowed "$allowed" --argjson refused "$EGRESS_REFUSED" \
    'map(.name) + ["metadata HTTP GET 169.254.169.254", "DNS registry.npmjs.org", "DNS example.com"]
     + (if $eg == 1 then ($allowed | map("egress allowed " + .)) + ($refused | map("egress refused " + .)) + ["egress plain HTTP GET"] else [] end)
     | sort' <<<"$tj")"
  got="$(jq -c 'del(.self) | keys | sort' "$f")"
  check "$pre.set the probe answered for exactly the $(jq length <<<"$want") expected targets" test "$got" = "$want"
  [[ $got == "$want" ]] || note "missing: $(jq -cn --argjson w "$want" --argjson g "$got" '$w - $g'); unexpected: $(jq -cn --argjson w "$want" --argjson g "$got" '$g - $w')"
  while IFS= read -r k; do
    i=$((i + 1))
    local d
    d="$(jq -r --arg k "$k" '.[$k].detail // "no result"' "$f")"
    case $k in
      "egress allowed "*)
        check "$pre.$i $k is reached through the egress proxy ($d)" \
          jqf --arg k "$k" '.[$k].outcome == "tunnel" and (.[$k].https | type) == "number"' "$f" ;;
      "egress refused "*)
        local r
        r="$(jq -r --arg t "${k#egress refused }" '.[$t]' <<<"$EGRESS_REASONS")"
        check "$pre.$i $k: the proxy refuses it (403, deny=$r) ($d)" \
          jqf --arg k "$k" --arg r "$r" '.[$k].outcome == "denied" and .[$k].status == 403 and .[$k].deny == $r' "$f" ;;
      "egress plain HTTP GET")
        check "$pre.$i $k: the proxy refuses it (405, deny=method) ($d)" \
          jqf --arg k "$k" '.[$k].outcome == "denied" and .[$k].status == 405 and .[$k].deny == "method"' "$f" ;;
      *)
        local cd
        cd="$(jq -r --arg k "$k" '.[$k].detail // "control missing"' "$cf")"
        if jq -e --arg k "$k" '.[$k].reached == true' "$cf" >/dev/null 2>&1; then
          check "$pre.$i $k: not reached ($d; $cd)" jqf --arg k "$k" '.[$k].reached == false' "$f"
        else
          fail "$pre.$i $k: the positive control does not reach it, so the probe proves nothing" "$d; $cd"
        fi ;;
    esac
  done < <(jq -r '.[]' <<<"$want")
  check "$pre.self runs as a non-root user with no capabilities" jqe '.self.uid != 0 and .self.cap_eff == "0000000000000000" and .self.cap_bnd == "0000000000000000"' <"$f"
  check "$pre.self has no_new_privs and a seccomp filter (mode 2)" jqe '.self.no_new_privs == "1" and .self.seccomp == "2"' <"$f"
  check "$pre.self has a read-only root and no Docker socket" jqe '.self.root_writable == false and .self.docker_socket == false' <"$f"
  check "$pre.self has no secret-like environment variables" jqe '.self.env_keys == []' <"$f"
}

point1() { # [tag: r for runsc]
  local t=${1:-}
  section "point 1${t:+ ($t)}: a Node app in plain Docker with the driver's flags reaches nothing but the egress proxy"
  plain_control
  net p1$t-a "${APP_NET_OPTS[@]}"
  net p1$t-b "${APP_NET_OPTS[@]}"
  app p1$t-other p1$t-b "$AUD_B"
  egress_up "$P-egress$t" p1$t-a p1$t-b
  local eip targets
  eip="$(ip_of "$P-egress$t" "$P-p1$t-a")"
  [[ -n $eip ]] || { fail "1$t egress proxy on the app network"; return; }
  probe_targets "1$t" p1$t-a p1$t-other p1$t-b
  targets=$TARGETS
  note "targets: $(jq -r 'map("\(.host):\(.port)") | join(" ")' <<<"$targets")"
  mkdir -p "$WORK/p1$t" && chmod 777 "$WORK/p1$t"
  docker run --rm --name "$P-probe-p1$t" --network "$P-p1$t-a" "${DRIVER_FLAGS[@]}" \
    -v "$HERE/probe:/probe:ro" -v "$WORK/p1$t:/out" -e PROBE_OUT=/out/result.json \
    -e PROBE_TARGETS="$targets" -e EGRESS="$eip:3128" -e EGRESS_ALLOWED='["registry.npmjs.org:443"]' \
    -e EGRESS_REFUSED="$EGRESS_REFUSED" "$NODE_IMAGE" node /probe/probe.js
  run_controls "$targets" "$WORK/p1$t/controls.json"
  probe_checks "1$t.p" "$WORK/p1$t/result.json" "$targets" "$WORK/p1$t/controls.json" 1 '["registry.npmjs.org:443"]'
  note "IPv6 inside the app container: $(jq -r '.self.ipv6 | to_entries | map("\(.key)=\(.value)") | join(" ")' "$WORK/p1$t/result.json" 2>/dev/null)"
  local cfg
  cfg="$(docker inspect "$P-p1$t-other" -f '{{.HostConfig.ReadonlyRootfs}} {{.HostConfig.CapDrop}} {{.HostConfig.SecurityOpt}} {{.Config.User}} {{.HostConfig.Memory}} {{.HostConfig.NanoCpus}} {{.HostConfig.PidsLimit}} {{.HostConfig.Privileged}} {{len .Mounts}}')"
  note "app container: $cfg"
  check "1$t.cfg the app container has the driver's flags (read-only, cap-drop ALL, no-new-privileges, user, limits, no mounts)" \
    test "$cfg" = "true [ALL] [no-new-privileges] 10001:10001 268435456 1000000000 128 false 0"
  local log
  log="$(docker logs "$P-egress$t" 2>&1)"
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
  plain_control
  net build "${APP_NET_OPTS[@]}"
  net p5-other "${APP_NET_OPTS[@]}"
  app p5-other p5-other "$AUD_B"
  # Its own egress proxy: check 5.2 reads only the build's traffic, never point 1's.
  egress_up "$P-egress-build" build
  local eip targets rc
  eip="$(ip_of "$P-egress-build" "$P-build")"
  [[ -n $eip ]] || { fail "5 egress proxy on the build network"; return; }
  probe_targets 5 build p5-other p5-other
  targets=$TARGETS
  mkdir -p "$WORK/p5"
  # The job's output folder is a Docker volume on tmpfs with a size limit (64 MiB), not a
  # writable host folder: a postinstall cannot fill the host's disk. The production recipe must
  # not give a build a writable host bind mount either (README.md, point 5).
  docker volume create --label ruum.app=$P --driver local --opt type=tmpfs --opt device=tmpfs \
    --opt o=size=64m,uid=10001,gid=10001,mode=0700 "$P-p5-out" >/dev/null
  # Docker mounts a tmpfs volume while a container uses it and drops its content after the last one
  # stops, so a holder container keeps it mounted until the output is copied out.
  docker run -d --name "$P-p5-hold" --network none "${DRIVER_FLAGS[@]}" -v "$P-p5-out:/out:ro" \
    "$NODE_IMAGE" sleep 900 >/dev/null
  # The fixed node22 recipe, step 1: npm ci as a sandbox job (the driver's flags, a tmpfs work
  # folder, a size-limited output volume, a time limit, no secrets, egress only through the proxy).
  timeout 300 docker run --rm --name "$P-build-job" --network "$P-build" \
    --read-only --cap-drop ALL --security-opt no-new-privileges --user 10001:10001 \
    --memory 512m --memory-swap 512m --cpus 2 --pids-limit 256 --label ruum.app=$P \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=128m \
    --tmpfs /work:rw,noexec,nosuid,nodev,size=256m,uid=10001,gid=10001 \
    -v "$HERE/hostile:/src:ro" -v "$HERE/probe:/probe:ro" -v "$P-p5-out:/out" \
    -e HOME=/tmp -e npm_config_cache=/tmp/npm-cache -e npm_config_update_notifier=false \
    -e HTTPS_PROXY="http://$eip:3128" -e https_proxy="http://$eip:3128" \
    -e PROBE_TARGETS="$targets" -e EGRESS="$eip:3128" -e EGRESS_ALLOWED='[]' \
    -e EGRESS_REFUSED="$EGRESS_REFUSED" \
    "$NODE_IMAGE" sh -c 'cp -r /src/. /work/ && cd /work && npm ci --no-audit --no-fund --foreground-scripts \
      && tar -C /work -cf /out/layer.tar package.json node_modules' >"$WORK/p5/npm.log" 2>&1
  rc=$?
  run_controls "$targets" "$WORK/p5/controls.json"
  check "5.1 npm ci of the hostile package succeeds in the sandbox job (exit $rc)" test $rc = 0
  [[ $rc == 0 ]] || sed 's/^/      npm: /' "$WORK/p5/npm.log" | tail -20
  check "5.2 the dependency came through the build's own egress proxy" bash -c "docker logs '$P-egress-build' 2>&1 | grep -q 'host=registry.npmjs.org port=443 status=200'"
  # Copy the output out of the volume, capped in bytes, and keep only the expected regular files.
  local cap=$((80 * 1024 * 1024)) size
  docker exec "$P-p5-hold" sh -c 'cd /out && tar -cf - $(ls -A)' 2>/dev/null | head -c "$cap" >"$WORK/p5/out.tar"
  docker rm -f "$P-p5-hold" >/dev/null 2>&1
  size="$(stat -c %s "$WORK/p5/out.tar")"
  check "5.5a the job's output is copied out under the byte cap ($size of $cap bytes)" test "$size" -gt 0 -a "$size" -lt "$cap"
  tar -C "$WORK/p5" --no-same-owner --no-same-permissions -xf "$WORK/p5/out.tar" layer.tar postinstall.json fill.json 2>/dev/null
  local f
  for f in layer.tar postinstall.json fill.json; do
    [[ -L $WORK/p5/$f ]] && { rm -f "$WORK/p5/$f"; note "dropped a link: $f"; }
  done
  check "5.5 the output folder has a size limit: the postinstall's 96 MiB write stops with ENOSPC at 64 MiB or less ($(cat "$WORK/p5/fill.json" 2>/dev/null))" \
    jqf '.error == "ENOSPC" and .written <= 67108864' "$WORK/p5/fill.json"
  check "5.3 the postinstall ran and wrote its probe result" test -s "$WORK/p5/postinstall.json"
  probe_checks 5.p "$WORK/p5/postinstall.json" "$targets" "$WORK/p5/controls.json" 1 '[]'
  # Step 2 of the recipe: the layer is a plain tar; the app runs from it in a sandbox. (Read-only
  # bind of the unpacked layer: the spike does not assemble an OCI image; R-2b does.)
  if [[ -s $WORK/p5/layer.tar && ! -L $WORK/p5/layer.tar ]]; then
    note "layer: $(sha256sum "$WORK/p5/layer.tar" | cut -c1-71) ($(stat -c %s "$WORK/p5/layer.tar") bytes)"
    mkdir -p "$WORK/p5/app" && tar -C "$WORK/p5/app" --no-same-owner -xf "$WORK/p5/layer.tar"
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
