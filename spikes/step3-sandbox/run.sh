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
clean_images() {
  docker images -q --filter "reference=$P-*" | sort -u | xargs -r docker rmi -f >/dev/null 2>&1
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

trap 'cleanup' EXIT
cleanup
mkdir -p "$WORK"
chmod 700 "$WORK"

section "set-up"
df -h / | awk 'NR==2 {print "      disk free: " $4}'
(cd "$HERE/gw" && cargo build --release --locked -q) || { echo "gateway build failed" >&2; exit 2; }
docker image inspect "$NODE_IMAGE" >/dev/null 2>&1 || docker pull -q "$NODE_IMAGE" >/dev/null
note "node image: $(docker image inspect "$NODE_IMAGE" -f '{{index .RepoDigests 0}}')"
docker build -q -t "$P-app:1" --label ruum.app=$P "$HERE/app" >/dev/null || { echo "app image failed" >&2; exit 2; }

# The user-token key (Ed25519, made for this run, never printed, removed at the end).
openssl genpkey -algorithm ed25519 -out "$WORK/user-token.pem" 2>/dev/null
openssl pkey -in "$WORK/user-token.pem" -pubout -out "$WORK/user-token.pub" 2>/dev/null
chmod 600 "$WORK/user-token.pem"
PUBKEY="$(cat "$WORK/user-token.pub")"

# One internal network per app (internal: true: no route out, no port publishing).
net() { docker network create --internal --label ruum.app=$P "$P-$1" >/dev/null; }

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
  check "2.5 an incoming traceparent and X-Forwarded-For are replaced" \
    jqe '([.raw_headers[] | select(.[1] | test("0af7651916cd43dd|10\\.9\\.9\\.9"))] | length == 0)
      and ([.raw_headers[] | select(.[0] | ascii_downcase == "traceparent")] | length == 1)' <<<"$out"

  out="$(curl -fsS "$G/" -H 'Cookie: __Host-ruum_app=SESSION; a=1; __Host-ruum_visit=VISIT; b=2; ruum_x=3')"
  check "2.6 the gateway cookies leave Cookie; the app's own cookies stay" \
    jqe '[.raw_headers[] | select(.[0] | ascii_downcase == "cookie") | .[1]] == ["a=1; b=2"]' <<<"$out"
  out="$(curl -fsS "$G/" -H 'Authorization: Bearer ruum_appkey_0123456789')"
  check "2.7 a partner key in Authorization never reaches the app" \
    jqe '[.raw_headers[] | select(.[0] | ascii_downcase == "authorization")] | length == 0' <<<"$out"

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
  umask 077
  printf 'S3_ENDPOINT=http://127.0.0.1:3900\nS3_BUCKET=%s\nS3_ACCESS_KEY_ID=%s\nS3_SECRET_ACCESS_KEY=%s\n' \
    "$P-static" "$id" "$secret" >"$WORK/s3.env"
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

want 2 && point2
want 4 && point4

section "result"
printf 'PASS %d  FAIL %d  SKIP %d\n' "$PASS" "$FAIL" "$SKIP"
for f in "${FAILED[@]}"; do printf '  failed: %s\n' "$f"; done
[[ $FAIL -eq 0 ]]
