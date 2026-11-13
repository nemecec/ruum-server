#!/usr/bin/env bash
# The deployer's rights (backlog O-34, decision 0008-2): it may only write release requests. It
# must not create a Pod, Job or any other workload anywhere, use a pod subresource (log, exec,
# attach, port-forward, ephemeral containers), read a Secret, get a ServiceAccount token, change
# a Role or binding, or write anything but a new ConfigMap in ruum-deploy. A pod mounts any Secret
# of its namespace whatever its creator may read, so "no workload" is what keeps the Secrets out
# of its reach.
#
#   rbac-check.sh static FILE...   the rendered manifests (helm template; CI, tests/run.sh)
#   rbac-check.sh live             the live cluster, with the admin kubeconfig (k8s/apply.sh runs
#                                  it after every platform step): kubectl auth can-i --as
#
# static: every RoleBinding or ClusterRoleBinding whose subjects include the deployer (its
# ServiceAccount, its user name, or a group that holds it: system:serviceaccounts,
# system:serviceaccounts:ruum, system:authenticated) must be a RoleBinding to a Role in the same
# files, and every rule of that Role must be one line of the form
#   - { apiGroups: [...], resources: [...], [resourceNames: [...], ]verbs: [...] }
# Anything else fails: an unreadable rule, a ClusterRole, a wildcard, a Secret, a subresource, a
# write other than `create configmaps` in ruum-deploy, or a ConfigMap read without resourceNames
# outside ruum-deploy (Helm's release records in ruum hold the values of the release).
set -uo pipefail

DEPLOYER_NS=ruum
DEPLOYER=deployer
REQUESTS_NS=ruum-deploy
CONTROLLER_NS=ruum-deploy
CONTROLLER=deploy-controller

fail=0
problem() { echo "rbac-check: $*" >&2; fail=1; }

# docs FILE...: one record per line, tab-separated, for every document of the manifests:
#   D <doc> <kind> <namespace> <name>
#   S <doc> <subject kind> <subject name> <subject namespace>
#   R <doc> <roleRef kind> <roleRef name>
#   L <doc> <rule line>              (a line under rules:, comments left out)
docs() {
  awk '
    function subj() { if (sk != "") printf "S\t%d\t%s\t%s\t%s\n", doc, sk, sn, sns; sk = ""; sn = ""; sns = "" }
    function flush() { subj(); if (kind != "") printf "D\t%d\t%s\t%s\t%s\n", doc, kind, ns, name; kind = ""; ns = ""; name = ""; sect = "" }
    FNR == 1 { flush(); doc++ }
    /^---/ { flush(); doc++; next }
    /^[[:space:]]*#/ { next }
    /^kind: / { subj(); kind = $2; sect = ""; next }
    /^metadata:/ { subj(); sect = "metadata"; next }
    /^rules:/ { subj(); sect = "rules"; next }
    /^subjects:/ { subj(); sect = "subjects"; next }
    /^roleRef:/ { subj(); sect = "roleRef"; next }
    /^[^[:space:]]/ { subj(); sect = "other"; next }
    sect == "metadata" && /^  name: / { if (name == "") name = $2; next }
    sect == "metadata" && /^  namespace: / { ns = $2; next }
    sect == "rules" && NF > 0 { printf "L\t%d\t%s\n", doc, $0; next }
    sect == "subjects" && /^  - kind: / { subj(); sk = $3; next }
    sect == "subjects" && /^    name: / { sn = $2; next }
    sect == "subjects" && /^    namespace: / { sns = $2; next }
    sect == "subjects" && /^    apiGroup: / { next }
    sect == "subjects" && NF > 0 { subj(); printf "S\t%d\tUNREADABLE\t%s\t-\n", doc, $0; next }
    sect == "roleRef" && /^  kind: / { rk = $2; next }
    sect == "roleRef" && /^  name: / { printf "R\t%d\t%s\t%s\n", doc, rk, $2; next }
    END { flush() }
  ' "$@"
}

# is_deployer KIND NAME NAMESPACE: the subject is the deployer, or a group that holds it.
is_deployer() {
  case "$1" in
    ServiceAccount) [[ "$2" == "$DEPLOYER" && "$3" == "$DEPLOYER_NS" ]] ;;
    User) [[ "$2" == "system:serviceaccount:$DEPLOYER_NS:$DEPLOYER" ]] ;;
    Group) [[ "$2" == system:serviceaccounts || "$2" == "system:serviceaccounts:$DEPLOYER_NS" || "$2" == system:authenticated ]] ;;
    *) return 0 ;; # unreadable: count it as the deployer (fail closed)
  esac
}

# split "[a, b, ""]" contents: one item per line, quotes removed.
items() { tr ',' '\n' <<<"$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'\$/\1/"; }

RULE_RE='^  - \{ apiGroups: \[([^]]*)\], resources: \[([^]]*)\], (resourceNames: \[([^]]*)\], )?verbs: \[([^]]*)\] \}$'

# check_rule NAMESPACE LINE: the rule allows nothing that the deployer must not do.
check_rule() {
  local ns="$1" line="$2" groups resources names verbs r v
  if [[ ! "$line" =~ $RULE_RE ]]; then
    problem "deployer Role in $ns: a rule that this check cannot read (one line per rule): $line"
    return
  fi
  groups="${BASH_REMATCH[1]}" resources="${BASH_REMATCH[2]}" names="${BASH_REMATCH[4]}" verbs="${BASH_REMATCH[5]}"
  [[ "$groups$resources$names$verbs" != *'*'* ]] || { problem "deployer Role in $ns: a wildcard: $line"; return; }
  while IFS= read -r r; do
    [[ -n "$r" ]] || continue
    [[ "$r" != */* ]] || { problem "deployer Role in $ns: the subresource $r: $line"; continue; }
    [[ "$r" != secrets ]] || { problem "deployer Role in $ns: Secrets: $line"; continue; }
    while IFS= read -r v; do
      case "$v" in
        get | list | watch)
          if [[ "$r" == configmaps && "$ns" != "$REQUESTS_NS" && ( -z "$names" || "$v" != get ) ]]; then
            problem "deployer Role in $ns: ConfigMaps read without resourceNames (Helm's release records): $line"
          fi ;;
        create)
          [[ "$r" == configmaps && "$ns" == "$REQUESTS_NS" && "$(items "$groups")" == "" ]] ||
            problem "deployer Role in $ns: $v $r (the deployer creates only release requests in $REQUESTS_NS): $line" ;;
        *) problem "deployer Role in $ns: $v $r (the deployer changes nothing): $line" ;;
      esac
    done < <(items "$verbs")
  done < <(items "$resources")
}

static() {
  local recs rec t doc kind ns name sk sn sns rk rn bound=0
  [[ $# -gt 0 ]] || { echo "usage: rbac-check.sh static FILE..." >&2; exit 2; }
  recs="$(docs "$@")"
  # The bindings of the deployer.
  while IFS=$'\t' read -r t doc sk sn sns; do
    [[ "$t" == S ]] || continue
    is_deployer "$sk" "$sn" "$sns" || continue
    rec="$(awk -F'\t' -v d="$doc" '$1 == "D" && $2 == d' <<<"$recs")"
    IFS=$'\t' read -r _ _ kind ns name <<<"$rec"
    IFS=$'\t' read -r _ _ rk rn <<<"$(awk -F'\t' -v d="$doc" '$1 == "R" && $2 == d' <<<"$recs")"
    if [[ "$sk" == UNREADABLE ]]; then
      problem "$kind $ns/$name: a subject that this check cannot read: $sn"
      continue
    fi
    if [[ "$kind" != RoleBinding || "$rk" != Role ]]; then
      problem "$kind ${ns:+$ns/}$name binds $sk $sn to $rk $rn: the deployer gets only namespaced Roles"
      continue
    fi
    rec="$(awk -F'\t' -v ns="$ns" -v n="$rn" '$1 == "D" && $3 == "Role" && $4 == ns && $5 == n { print $2 }' <<<"$recs")"
    if [[ -z "$rec" ]]; then
      problem "RoleBinding $ns/$name binds the deployer to Role $ns/$rn, which is not in the manifests"
      continue
    fi
    bound=$((bound + 1))
    while IFS=$'\t' read -r t d line; do
      [[ "$t" == L && "$d" == "$rec" ]] && check_rule "$ns" "$line"
    done <<<"$recs"
  done <<<"$recs"
  [[ $bound -gt 0 ]] || problem "no RoleBinding of the deployer found (wrong files?)"
  [[ $fail == 0 ]] && echo "rbac-check: the deployer may only write release requests ($bound bindings checked)"
  return "$fail"
}

# can AS NAMESPACE VERB RESOURCE [SUBRESOURCE]: prints yes or no (or the error).
can() {
  local as="$1" ns="$2" verb="$3" res="$4" sub="${5:-}" args=()
  [[ -n "$sub" ]] && args+=(--subresource "$sub")
  [[ "$ns" == - ]] && args+=(--all-namespaces) || args+=(-n "$ns")
  kubectl auth can-i "$verb" "$res" ${args[@]+"${args[@]}"} --as "$as" 2>&1 | tail -n 1
}

want() { # want yes|no AS NAMESPACE VERB RESOURCE [SUBRESOURCE]
  local w="$1" got
  shift
  got="$(can "$@")"
  [[ "$got" == "$w" ]] || problem "kubectl auth can-i $3 $4${5:+ --subresource $5} -n $2 --as $1: $got (want $w)"
}

live() {
  local d="system:serviceaccount:$DEPLOYER_NS:$DEPLOYER" c="system:serviceaccount:$CONTROLLER_NS:$CONTROLLER" ns
  for ns in ruum edge ruum-deploy data auth authz nats ops default kube-system; do
    for x in "create pods" "create jobs.batch" "create cronjobs.batch" "create deployments.apps" "patch deployments.apps" \
      "update deployments.apps" "create daemonsets.apps" "patch daemonsets.apps" "create statefulsets.apps" \
      "create replicasets.apps" "create replicationcontrollers" "create podtemplates" "get secrets" "list secrets" "watch secrets" \
      "create serviceaccounts" "update serviceaccounts" "impersonate serviceaccounts" "create roles.rbac.authorization.k8s.io" \
      "create rolebindings.rbac.authorization.k8s.io" "update leases.coordination.k8s.io" "create backups.postgresql.cnpg.io" \
      "delete configmaps" "update configmaps" "patch configmaps"; do
      # shellcheck disable=SC2086 # verb and resource
      want no "$d" "$ns" $x
    done
    want no "$d" "$ns" get pods log
    want no "$d" "$ns" create pods exec
    want no "$d" "$ns" create pods attach
    want no "$d" "$ns" create pods portforward
    want no "$d" "$ns" patch pods ephemeralcontainers
    want no "$d" "$ns" create serviceaccounts token
    [[ "$ns" == "$REQUESTS_NS" ]] || want no "$d" "$ns" create configmaps
  done
  want no "$d" - create clusterrolebindings.rbac.authorization.k8s.io
  want no "$d" - impersonate users
  want no "$d" - get nodes proxy
  want no "$d" ruum list configmaps
  want yes "$d" "$REQUESTS_NS" create configmaps
  want yes "$d" ruum get configmaps/ruum-release
  # The controller has what the deploy needs, and no Secret either.
  want yes "$c" ruum create jobs.batch
  want yes "$c" ruum update leases.coordination.k8s.io/ruum-deploy
  want yes "$c" ruum patch deployments.apps
  want yes "$c" edge patch daemonsets.apps
  want yes "$c" data create backups.postgresql.cnpg.io
  want yes "$c" "$REQUESTS_NS" update configmaps
  for ns in ruum edge ruum-deploy data auth; do
    want no "$c" "$ns" get secrets
    want no "$c" "$ns" list secrets
  done
  want no "$c" auth create pods
  [[ $fail == 0 ]] && echo "rbac-check: the live deployer may only write release requests; the controller has the deploy rights and no Secret"
  return "$fail"
}

case "${1:-}" in
  static) shift; static "$@" ;;
  live) live ;;
  *) echo "usage: rbac-check.sh static FILE... | live" >&2; exit 2 ;;
esac
