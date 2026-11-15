#!/usr/bin/env bash
# The deployer's rights (backlog O-34, decision 0008-2): it may only write release requests. It
# must not create a Pod, Job or any other workload anywhere, use a pod subresource (log, exec,
# attach, port-forward, ephemeral containers), read a Secret, get a ServiceAccount token, change
# a Role or binding, or write anything but a new ConfigMap in ruum-deploy. A pod mounts any Secret
# of its namespace whatever its creator may read, so "no workload" is what keeps the Secrets out
# of its reach.
#
# The observer (O-51) reads only troubleshooting resources and pod logs in ruum, edge and
# ruum-deploy. Its direct and group bindings pass the same checks, with a strict allow-list.
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
OBSERVER_NS=ruum
OBSERVER=observer

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

# is_subject KIND NAME NAMESPACE: the subject includes the identity being checked.
is_subject() {
  case "$1" in
    ServiceAccount) [[ "$2" == "$identity" && "$3" == "$identity_ns" ]] ;;
    User) [[ "$2" == "system:serviceaccount:$identity_ns:$identity" ]] ;;
    Group) [[ "$2" == system:serviceaccounts || "$2" == "system:serviceaccounts:$identity_ns" || "$2" == system:authenticated ]] ;;
    *) return 0 ;; # unreadable: count it as this identity (fail closed)
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

# Only these API group/resource/verb combinations belong to the observer.
check_observer_rule() {
  local ns="$1" line="$2" groups resources verbs g r v
  if [[ ! "$line" =~ $RULE_RE ]]; then
    problem "observer Role in $ns: unreadable rule: $line"
    return
  fi
  groups="${BASH_REMATCH[1]}" resources="${BASH_REMATCH[2]}" verbs="${BASH_REMATCH[5]}"
  [[ "$line" != *'*'* ]] || { problem "observer Role in $ns: wildcard: $line"; return; }
  while IFS= read -r g; do
    while IFS= read -r r; do
      case "$g:$r" in
        :pods | :events | :pods/log | events.k8s.io:events | apps:deployments | apps:daemonsets | apps:replicasets | batch:jobs) ;;
        *) problem "observer Role in $ns: forbidden resource $g:$r" ;;
      esac
      while IFS= read -r v; do
        case "$r:$v" in
          pods/log:get) ;;
          pods/log:*) problem "observer Role in $ns: only get is allowed on pods/log" ;;
          *:get | *:list | *:watch) ;;
          *) problem "observer Role in $ns: forbidden verb $v" ;;
        esac
      done < <(items "$verbs")
    done < <(items "$resources")
  done < <(items "$groups")
}

check_bindings() {
  local identity="$1" identity_ns="$2" checker="$3"
  local rec t doc kind ns name sk sn sns rk rn d line bound=0 seen=" "
  # Check direct, user and group bindings for this identity.
  while IFS=$'\t' read -r t doc sk sn sns; do
    [[ "$t" == S ]] || continue
    is_subject "$sk" "$sn" "$sns" || continue
    rec="$(awk -F'\t' -v d="$doc" '$1 == "D" && $2 == d' <<<"$recs")"
    IFS=$'\t' read -r _ _ kind ns name <<<"$rec"
    IFS=$'\t' read -r _ _ rk rn <<<"$(awk -F'\t' -v d="$doc" '$1 == "R" && $2 == d' <<<"$recs")"
    if [[ "$sk" == UNREADABLE ]]; then
      problem "$kind $ns/$name: a subject that this check cannot read: $sn"
      continue
    fi
    if [[ "$kind" != RoleBinding || "$rk" != Role ]]; then
      problem "$kind ${ns:+$ns/}$name binds $sk $sn to $rk $rn: the $identity gets only namespaced Roles"
      continue
    fi
    rec="$(awk -F'\t' -v ns="$ns" -v n="$rn" '$1 == "D" && $3 == "Role" && $4 == ns && $5 == n { print $2 }' <<<"$recs")"
    if [[ -z "$rec" || "$rec" == *$'\n'* ]]; then
      problem "RoleBinding $ns/$name binds the $identity to Role $ns/$rn, which is missing or duplicated in the manifests"
      continue
    fi
    if [[ "$identity" == "$OBSERVER" ]]; then
      case "$ns" in
        ruum | edge | ruum-deploy) seen=" $ns$seen" ;;
        *) problem "observer binding outside ruum, edge and ruum-deploy: $ns/$name" ;;
      esac
    fi
    bound=$((bound + 1))
    while IFS=$'\t' read -r t d line; do
      [[ "$t" == L && "$d" == "$rec" ]] && "$checker" "$ns" "$line"
    done <<<"$recs"
  done <<<"$recs"
  [[ $bound -gt 0 ]] || problem "no RoleBinding of the $identity found (wrong files?)"
  if [[ "$identity" == "$OBSERVER" ]]; then
    for ns in ruum edge ruum-deploy; do
      [[ "$seen" == *" $ns "* ]] || problem "no observer RoleBinding in $ns"
    done
  fi
  [[ $fail == 0 ]] && echo "rbac-check: $identity rights checked ($bound bindings)"
}

static() {
  local recs
  [[ $# -gt 0 ]] || { echo "usage: rbac-check.sh static FILE..." >&2; exit 2; }
  recs="$(docs "$@")"
  check_bindings "$DEPLOYER" "$DEPLOYER_NS" check_rule
  check_bindings "$OBSERVER" "$OBSERVER_NS" check_observer_rule
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
    # Pod logs through the subresource (backlog O-50): `kubectl auth can-i get pods/log` reads
    # `log` as the name of a pod, so it answers yes for anyone who may get pods. Only
    # `--subresource log` asks for the logs.
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
  local o="system:serviceaccount:$OBSERVER_NS:$OBSERVER" resource verb
  for ns in ruum edge ruum-deploy; do
    for resource in pods events events.events.k8s.io deployments.apps daemonsets.apps replicasets.apps jobs.batch; do
      for verb in get list watch; do
        want yes "$o" "$ns" "$verb" "$resource"
      done
    done
    want yes "$o" "$ns" get pods log
  done
  for ns in ruum edge ruum-deploy data auth authz nats ops default kube-system; do
    want no "$o" "$ns" get secrets
    want no "$o" "$ns" get configmaps
    want no "$o" "$ns" create pods
    want no "$o" "$ns" create pods exec
    want no "$o" "$ns" create pods attach
    want no "$o" "$ns" create pods portforward
    want no "$o" "$ns" patch pods ephemeralcontainers
    want no "$o" "$ns" create serviceaccounts token
    want no "$o" "$ns" patch deployments.apps
    case "$ns" in
      ruum | edge | ruum-deploy) ;;
      *) want no "$o" "$ns" get pods; want no "$o" "$ns" get pods log ;;
    esac
  done
  [[ $fail == 0 ]] && echo "rbac-check: the live deployer may only write release requests; the controller has the deploy rights and no Secret; the observer is read-only"
  return "$fail"
}

case "${1:-}" in
  static) shift; static "$@" ;;
  live) live ;;
  *) echo "usage: rbac-check.sh static FILE... | live" >&2; exit 2 ;;
esac
