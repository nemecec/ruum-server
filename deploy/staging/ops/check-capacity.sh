#!/usr/bin/env bash
# Sizing rule (review 2 U-2): the sum of the memory requests of all pods must fit on N-1 nodes,
# or a node upgrade (or a lost node) leaves pods Pending and the drain hangs. Exit 1 when it
# does not fit: add stg-4 (NODE_COUNT=4, about EUR 7.43) before the next upgrade. On the Mac:
#   deploy/staging/tool.sh bash deploy/staging/ops/check-capacity.sh
set -euo pipefail
# Decimal points, whatever the locale of the caller.
export LC_ALL=C
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
export KUBECONFIG="${KUBECONFIG:-$WORK/kubeconfig}"
to_mi() { # Kubernetes quantity -> MiB
  awk -v q="$1" 'BEGIN {
    n = q + 0; u = q; sub(/^[0-9.]+/, "", u)
    if (u == "Ki") n /= 1024; else if (u == "Mi") n = n; else if (u == "Gi") n *= 1024;
    else if (u == "k") n /= 1048.576; else if (u == "M") n /= 1.048576; else if (u == "G") n *= 953.674;
    else if (u == "") n /= 1048576;
    printf "%d", n }'
}
alloc=()
while read -r q; do [[ -n "$q" ]] && alloc+=("$(to_mi "$q")"); done < <(kubectl get nodes -o jsonpath='{range .items[*]}{.status.allocatable.memory}{"\n"}{end}')
n=${#alloc[@]}
[[ $n -ge 2 ]] || { echo "check-capacity: fewer than 2 nodes" >&2; exit 1; }
mapfile -t sorted < <(printf '%s\n' "${alloc[@]}" | sort -n)
# N-1: leave out the largest node.
fit=0
for ((i = 0; i < n - 1; i++)); do fit=$((fit + sorted[i])); done
req=0
while read -r q; do [[ -n "$q" ]] && req=$((req + $(to_mi "$q"))); done < <(
  kubectl get pods -A --field-selector=status.phase!=Succeeded,status.phase!=Failed \
    -o jsonpath='{range .items[*]}{range .spec.containers[*]}{.resources.requests.memory}{"\n"}{end}{end}')
echo "memory requests: ${req} MiB; allocatable on N-1 of $n nodes: ${fit} MiB"
if ((req > fit)); then
  echo "check-capacity: requests do not fit on N-1 nodes; add stg-4 before the next upgrade" >&2
  exit 1
fi
