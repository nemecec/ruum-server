# The admin tunnel of staging (decision of the person, 2026-11-11). Sourced by lib.sh on the Mac
# (macOS bash 3.2) and by steps.sh in the tools container; it only defines values and functions.
#
# The firewall opens the Kubernetes API (6443) and the Talos API (50000) of the nodes only to the
# build server's fixed public IPv4 (ruum-buildbox). The Mac reaches them through an SSH local
# forward over the build server, as the user ruumtunnel, whose key may only open these ports
# (ops/buildbox-tunnel-user.sh). TLS and mTLS go end to end: the build server relays encrypted
# bytes, and the kubeconfig and talosconfig stay on the Mac.
#
# Local ports on the Mac (127.0.0.1 only):
#   16443           -> stg-1 public IPv4:6443   (kubeconfig: https://127.0.0.1:16443, tls-server-name kubernetes)
#   15000 + N       -> stg-N public IPv4:50000  (talosconfig endpoints 127.0.0.1:1500N, nodes by private IP)
# Both API certificates of Talos v1.14 hold 127.0.0.1 (apid and kube-apiserver add it themselves),
# so no certSANs change is needed. In the tools container, tunnel-relay.sh listens on the same
# 127.0.0.1 ports and relays to the Mac (host.docker.internal), so the same files work there.

# shellcheck shell=bash
# shellcheck disable=SC2034 # used by the scripts that source this file
TUNNEL_USER=ruumtunnel
TUNNEL_KUBE_PORT=16443
TUNNEL_TALOS_BASE=15000
TUNNEL_KUBE_SERVER_NAME=kubernetes
TUNNEL_PRIVATE_RE='^10\.20\.1\.([0-9]{1,3})$'

# ipv4_public ADDRESS: true for a public unicast IPv4 address (no private, loopback, link-local,
# shared (CGNAT), multicast, reserved or documentation range).
ipv4_public() {
  local a b c o octets
  [[ "$1" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  octets=("${BASH_REMATCH[@]:1}")
  for o in "${octets[@]}"; do
    [[ "$o" == 0 || "${o:0:1}" != 0 ]] || return 1
    ((10#$o <= 255)) || return 1
  done
  a=${octets[0]} b=${octets[1]} c=${octets[2]}
  ((a == 0 || a == 10 || a == 127 || a >= 224)) && return 1
  ((a == 100 && b >= 64 && b <= 127)) && return 1
  ((a == 169 && b == 254)) && return 1
  ((a == 172 && b >= 16 && b <= 31)) && return 1
  ((a == 192 && b == 168)) && return 1
  ((a == 192 && b == 0 && (c == 0 || c == 2))) && return 1
  ((a == 198 && (b == 18 || b == 19))) && return 1
  ((a == 198 && b == 51 && c == 100)) && return 1
  ((a == 203 && b == 0 && c == 113)) && return 1
  return 0
}

# tunnel_parse TEXT: reads the output of `steps.sh tunnel-targets`:
#   bastion <build server public IPv4>
#   node stg-N <public IPv4> <private IPv4>      (one line per node)
# Sets TUNNEL_BASTION, TUNNEL_NAMES, TUNNEL_INDEX, TUNNEL_PUBLIC, TUNNEL_PRIVATE (indexed arrays,
# in the order of the lines). Prints the reason and returns 1 on anything unexpected.
tunnel_parse() {
  local kind name pub priv extra n=0 seen=" "
  TUNNEL_BASTION="" TUNNEL_NAMES=() TUNNEL_INDEX=() TUNNEL_PUBLIC=() TUNNEL_PRIVATE=()
  while read -r kind name pub priv extra; do
    [[ -n "$kind" ]] || continue
    [[ -z "$extra" ]] || { echo "tunnel targets: extra field on line '$kind $name'"; return 1; }
    case "$kind" in
      bastion)
        [[ -z "$TUNNEL_BASTION" && -z "$pub" ]] || { echo "tunnel targets: one bastion line with one address"; return 1; }
        ipv4_public "$name" || { echo "tunnel targets: the build server address '$name' is not a public IPv4"; return 1; }
        TUNNEL_BASTION="$name"
        ;;
      node)
        [[ "$name" =~ ^stg-([1-4])$ ]] || { echo "tunnel targets: unexpected node name '$name'"; return 1; }
        [[ "$seen" != *" $name "* ]] || { echo "tunnel targets: node $name twice"; return 1; }
        ipv4_public "$pub" || { echo "tunnel targets: $name public address '$pub' is not a public IPv4"; return 1; }
        [[ "$priv" =~ $TUNNEL_PRIVATE_RE ]] && ((10#${BASH_REMATCH[1]} >= 1 && 10#${BASH_REMATCH[1]} <= 254)) ||
          { echo "tunnel targets: $name private address '$priv' is not in 10.20.1.0/24"; return 1; }
        seen="$seen$name "
        TUNNEL_NAMES[n]="$name"
        TUNNEL_INDEX[n]="${name#stg-}"
        TUNNEL_PUBLIC[n]="$pub"
        TUNNEL_PRIVATE[n]="$priv"
        n=$((n + 1))
        ;;
      *) echo "tunnel targets: unexpected line '$kind'"; return 1 ;;
    esac
  done <<<"$1"
  [[ -n "$TUNNEL_BASTION" ]] || { echo "tunnel targets: no build server address"; return 1; }
  [[ $n -ge 1 ]] || { echo "tunnel targets: no staging node"; return 1; }
  [[ "$seen" == *" stg-1 "* ]] || { echo "tunnel targets: no stg-1 (the Kubernetes API endpoint)"; return 1; }
  local i
  for ((i = 0; i < n; i++)); do
    [[ "${TUNNEL_PUBLIC[i]}" != "$TUNNEL_BASTION" ]] || { echo "tunnel targets: a node has the build server's address"; return 1; }
  done
  return 0
}

# After tunnel_parse: the -L options of ssh, one word per line.
tunnel_forward_args() {
  local i
  for ((i = 0; i < ${#TUNNEL_NAMES[@]}; i++)); do
    if [[ "${TUNNEL_NAMES[i]}" == stg-1 ]]; then
      printf '%s\n' -L "127.0.0.1:$TUNNEL_KUBE_PORT:${TUNNEL_PUBLIC[i]}:6443"
    fi
  done
  for ((i = 0; i < ${#TUNNEL_NAMES[@]}; i++)); do
    printf '%s\n' -L "127.0.0.1:$((TUNNEL_TALOS_BASE + TUNNEL_INDEX[i])):${TUNNEL_PUBLIC[i]}:50000"
  done
}

# After tunnel_parse: the local ports, space separated (kube first).
tunnel_ports() {
  local i out="$TUNNEL_KUBE_PORT"
  for ((i = 0; i < ${#TUNNEL_NAMES[@]}; i++)); do out="$out $((TUNNEL_TALOS_BASE + TUNNEL_INDEX[i]))"; done
  printf '%s' "$out"
}

# After tunnel_parse: the talosconfig endpoints (127.0.0.1:1500N) and nodes (private IPv4s).
tunnel_talos_endpoints() {
  local i out=""
  for ((i = 0; i < ${#TUNNEL_NAMES[@]}; i++)); do out="$out 127.0.0.1:$((TUNNEL_TALOS_BASE + TUNNEL_INDEX[i]))"; done
  printf '%s' "${out# }"
}
tunnel_talos_nodes() {
  local i out=""
  for ((i = 0; i < ${#TUNNEL_NAMES[@]}; i++)); do out="$out ${TUNNEL_PRIVATE[i]}"; done
  printf '%s' "${out# }"
}

# tunnel_kube_rewrite FILE: point every cluster of a kubeconfig at the tunnel (needs kubectl).
tunnel_kube_rewrite() {
  local file="$1" c
  for c in $(kubectl --kubeconfig "$file" config get-clusters | tail -n +2); do
    kubectl --kubeconfig "$file" config set-cluster "$c" \
      --server="https://127.0.0.1:$TUNNEL_KUBE_PORT" --tls-server-name="$TUNNEL_KUBE_SERVER_NAME" >/dev/null
  done
}
