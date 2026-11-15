#!/usr/bin/env bash
# Acceptance test 2 of card R-2e, on the build server (Linux, Docker, the internet):
#
#   crates/ruum-egress/tests/container_check.sh
#
# Builds deploy/images/ruum-egress/Dockerfile, starts the proxy on an outside network and on an
# app network made like the Docker driver makes it (`--internal`, `inhibit_ipv4`: no host address
# on the bridge), registers the app network over the control socket, and probes from a container
# on the app network:
#   - registry.npmjs.org:443 through the proxy is reached (HTTP 200);
#   - 169.254.169.254, the host's public IPv4 address, the host's IPv6 address and another address
#     in its /64, fd00:ec2::254, and decimal and IPv4-mapped forms get the proxy's own refusal
#     (403 with X-Ruum-Egress-Deny: address); a refusal counts only with that header;
#   - with egress.allow = [registry.npmjs.org], npm is still reached and example.com gets
#     deny=allow; a direct connection without the proxy reaches nothing (positive control: the
#     same probe through the proxy reaches npm);
#   - an app network that is not registered gets deny=unknown_source;
#   - the release binary in the image has no test hook.
# Every container, network and image carries the label ruum.test=s3w3r2e and is removed at the
# end, also on failure. The probe image is pinned by digest and removed if it was not on the host.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
LABEL=ruum.test=s3w3r2e
P=r2e-check
IMAGE=ruum-egress:s3w3r2e-check
CURL=curlimages/curl:8.17.0@sha256:935d9100e9ba842cdb060de42472c7ca90cfe9a7c96e4dacb55e79e560b3ff40
APP_SUBNET=10.231.7.0/24
EGRESS_IP=10.231.7.2
OTHER_SUBNET=10.231.8.0/24
OTHER_EGRESS_IP=10.231.8.2
work="$(mktemp -d)"
pass=0 fail=0

curl_was_here=0
docker image inspect "$CURL" >/dev/null 2>&1 && curl_was_here=1

cleanup() {
  docker ps -aq --filter "label=$LABEL" | xargs -r docker rm -f >/dev/null 2>&1 || true
  docker network ls -q --filter "label=$LABEL" | xargs -r docker network rm >/dev/null 2>&1 || true
  docker image rm -f "$IMAGE" >/dev/null 2>&1 || true
  [[ $curl_was_here == 1 ]] || docker image rm -f "$CURL" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

ok() { pass=$((pass + 1)); echo "PASS $*"; }
bad() { fail=$((fail + 1)); echo "FAIL $*"; }

# The host's public addresses: they go into EGRESS_DENY (a name like <IP>.nip.io resolves there).
host_v4=$(ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')
host_v6=$(ip -6 addr show scope global | sed -n 's/.*inet6 \([0-9a-f:]*\)\/.*/\1/p' | head -1)
[[ -n $host_v4 ]] || { echo "no public IPv4 address of the host" >&2; exit 2; }
deny="$host_v4"
if [[ -n $host_v6 ]]; then
  prefix=$(python3 -c "import ipaddress,sys; print(ipaddress.ip_network(sys.argv[1] + '/64', strict=False))" "$host_v6")
  neighbour=$(python3 -c "import ipaddress,sys; n=ipaddress.ip_network(sys.argv[1]); print(n.network_address + 0xbeef)" "$prefix")
  deny="$deny,$prefix"
fi
echo "host: $host_v4 ${host_v6:-(no IPv6)}; EGRESS_DENY=$deny"

echo "== build the image"
docker build -q --label "$LABEL" -f "$repo/deploy/images/ruum-egress/Dockerfile" -t "$IMAGE" "$repo" >/dev/null

cid=$(docker create --label "$LABEL" "$IMAGE")
docker cp "$cid:/app/ruum-egress" "$work/bin" >/dev/null
docker rm "$cid" >/dev/null
if grep -q "the test upstream is a loopback address" "$work/bin"; then
  bad "the release binary contains the test hook"
else
  ok "the release binary has no test hook"
fi

docker pull -q "$CURL" >/dev/null

echo "== networks and the proxy"
docker network create --label "$LABEL" "$P-out" >/dev/null
docker network create --label "$LABEL" --internal -o com.docker.network.bridge.inhibit_ipv4=true \
  --subnet "$APP_SUBNET" "$P-app" >/dev/null
docker network create --label "$LABEL" --internal -o com.docker.network.bridge.inhibit_ipv4=true \
  --subnet "$OTHER_SUBNET" "$P-other" >/dev/null

mkdir -p "$work/ctl" "$work/secret"
head -c 48 /dev/urandom | base64 | tr -d '\n/+=' >"$work/secret/token"
chmod 600 "$work/secret/token"
printf 'Authorization: Bearer %s\n' "$(cat "$work/secret/token")" >"$work/secret/header"
chmod 600 "$work/secret/header"

# The proxy runs as the calling user here, so the control socket on the bind mount is usable by
# this script (connecting to a Unix socket needs write permission on it). Production gives the
# socket's volume to the server only.
docker run -d --name "$P-egress" --label "$LABEL" --network "$P-out" --read-only \
  --cap-drop ALL --security-opt no-new-privileges --user "$(id -u):$(id -g)" \
  -v "$work/ctl:/run/egress" -v "$work/secret/token:/run/egress-token/token:ro" \
  -e RUUM_EGRESS_CONTROL=unix:/run/egress/control.sock \
  -e RUUM_EGRESS_CONTROL_TOKEN_FILE=/run/egress-token/token \
  -e "EGRESS_DENY=$deny" "$IMAGE" >/dev/null
docker network connect --ip "$EGRESS_IP" "$P-app" "$P-egress"
docker network connect --ip "$OTHER_EGRESS_IP" "$P-other" "$P-egress"
for _ in $(seq 1 50); do [[ -S $work/ctl/control.sock ]] && break; sleep 0.2; done
[[ -S $work/ctl/control.sock ]] || { docker logs "$P-egress" >&2; echo "no control socket" >&2; exit 2; }

control() { # method path [body]
  curl -sS -o "$work/answer" -w '%{http_code}' --unix-socket "$work/ctl/control.sock" \
    -H @"$work/secret/header" -H 'Content-Type: application/json' -X "$1" "http://egress$2" ${3:+--data "$3"}
}
identity='{"tenant":"0190f0e2-0000-7000-8000-00000000c001","app":"0190f0e2-0000-7000-8000-00000000c002","deployment":"0190f0e2-0000-7000-8000-00000000c003","environment":"live"}'
code=$(curl -sS -o /dev/null -w '%{http_code}' --unix-socket "$work/ctl/control.sock" -H 'Authorization: Bearer wrong' http://egress/v1/usage)
[[ $code == 401 ]] && ok "control: a wrong token gets 401" || bad "control: a wrong token got $code"
code=$(control PUT /v1/networks/app "{\"subnet\":\"$APP_SUBNET\",\"identity\":$identity}")
[[ $code == 204 ]] && ok "control: the app network is registered" || { bad "control: PUT got $code: $(cat "$work/answer")"; }

probe() { # network args...
  local net=$1
  shift
  docker run --rm --label "$LABEL" --network "$net" --cap-drop ALL --read-only "$CURL" "$@" 2>&1 || true
}
reached() { # what url
  local out
  out=$(probe "$P-app" -sS -o /dev/null -w 'http=%{http_code}' --max-time 20 --proxy "http://$EGRESS_IP:3128" "$2")
  [[ $out == *http=200* ]] && ok "$1 reached ($out)" || bad "$1 not reached: $out"
}
denied() { # what reason url [network proxy]
  local out
  out=$(probe "${4:-$P-app}" -sS -v -o /dev/null --max-time 20 --proxy "http://${5:-$EGRESS_IP}:3128" "$3")
  if grep -qi "^< X-Ruum-Egress-Deny: $2" <<<"$out"; then ok "$1: deny=$2"; else bad "$1: no deny=$2: $(grep -E '^< HTTP|^< X-Ruum|curl:' <<<"$out" | tr '\n' ' ')"; fi
}

echo "== probes (no allow-list)"
reached "registry.npmjs.org:443" https://registry.npmjs.org/
denied "metadata 169.254.169.254" address https://169.254.169.254/
denied "metadata, decimal 2852039166" address https://2852039166/
denied "metadata IPv6 fd00:ec2::254" address "https://[fd00:ec2::254]/"
denied "the host's public IPv4 $host_v4" address "https://$host_v4/"
denied "the host's IPv4 as IPv4-mapped IPv6" address "https://[::ffff:$host_v4]/"
denied "the host's address through a name ($host_v4.nip.io)" address "https://$host_v4.nip.io/"
if [[ -n $host_v6 ]]; then
  denied "the host's IPv6 $host_v6" address "https://[$host_v6]/"
  denied "the host's /64 neighbour $neighbour" address "https://[$neighbour]/"
else
  bad "the host has no IPv6 address: the /64 cases did not run"
fi
denied "loopback 127.1" address https://127.1/
denied "plain http (absolute form)" method http://registry.npmjs.org/
out=$(probe "$P-app" -sS -o /dev/null -w 'http=%{http_code}' --max-time 10 https://registry.npmjs.org/)
[[ $out != *http=200* ]] && ok "no direct way out without the proxy ($(tr '\n' ' ' <<<"$out" | cut -c1-80))" || bad "a direct connection reached npm"
denied "an unregistered app network" unknown_source https://registry.npmjs.org/ "$P-other" "$OTHER_EGRESS_IP"

echo "== probes (egress.allow = registry.npmjs.org)"
code=$(control PUT /v1/networks/app "{\"subnet\":\"$APP_SUBNET\",\"identity\":$identity,\"allow\":[\"registry.npmjs.org\"]}")
[[ $code == 204 ]] && ok "control: the allow-list is set" || bad "control: PUT with allow got $code"
reached "registry.npmjs.org on the list" https://registry.npmjs.org/
denied "example.com off the list" allow https://example.com/

code=$(control GET /v1/usage)
if [[ $code == 200 ]] && python3 -c "import json,sys; r=json.load(open(sys.argv[1])); e=r['entries']; assert e and sum(x['bytes_down'] for x in e) > 0 and sum(x['denied'] for x in e) > 0" "$work/answer"; then
  ok "usage: bytes and refusals counted for the identity"
else
  bad "usage: $code $(cat "$work/answer")"
fi

logs=$(docker logs "$P-egress" 2>&1)
if grep -q "$(cat "$work/secret/token")" <<<"$logs"; then bad "the token is in the proxy's log"; else ok "the token is not in the proxy's log"; fi
if grep -qE 'GET|/-/|registry.npmjs.org/' <<<"$logs"; then bad "the log holds a path or method"; else ok "the log holds no path"; fi
grep -q "deny=address" <<<"$logs" && ok "the log has the refusals" || bad "no refusal in the log"

echo
echo "container check: $pass PASS, $fail FAIL"
[[ $fail == 0 ]]
