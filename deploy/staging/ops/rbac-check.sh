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
# The runtime driver (I6-1, the ServiceAccount ruum/ruum-server) makes the pods and records of
# app sandboxes in ruum-apps, reads their logs and creates TokenReviews, and nothing else. No
# verb on Secrets at all (backlog S-89: the values are sealed into the records), and a
# server-side dry run of a no-op patch of a Secret must be refused. The sandbox pods' account
# ruum-apps/ruum-sandbox has no right.
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
RUNTIME_NS=ruum
RUNTIME=ruum-server
APPS_NS=ruum-apps

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
  local ns="$1" line="$2" groups resources verbs g r v name_restriction
  if [[ ! "$line" =~ $RULE_RE ]]; then
    problem "observer Role in $ns: unreadable rule: $line"
    return
  fi
  groups="${BASH_REMATCH[1]}" resources="${BASH_REMATCH[2]}" verbs="${BASH_REMATCH[5]}"
  name_restriction="${BASH_REMATCH[3]}"
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
        # A name-restricted grant does not provide the required namespace-wide reads.
        [[ -n "$name_restriction" ]] || observer_tuples="$observer_tuples $g:$r:$v "
      done < <(items "$verbs")
    done < <(items "$resources")
  done < <(items "$groups")
}

require_observer_tuples() {
  local ns="$1" tuple verb
  for tuple in :pods :events events.k8s.io:events apps:deployments apps:daemonsets apps:replicasets batch:jobs; do
    for verb in get list watch; do
      [[ "$observer_tuples" == *" $tuple:$verb "* ]] || problem "observer Role in $ns: missing $tuple:$verb"
    done
  done
  [[ "$observer_tuples" == *" :pods/log:get "* ]] || problem "observer Role in $ns: missing :pods/log:get"
}

check_bindings() {
  local identity="$1" identity_ns="$2" checker="$3"
  local rec t doc kind ns name sk sn sns rk rn d line bound=0 seen=" " observer_tuples=""
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
    observer_tuples=""
    while IFS=$'\t' read -r t d line; do
      [[ "$t" == L && "$d" == "$rec" ]] && "$checker" "$ns" "$line"
    done <<<"$recs"
    [[ "$identity" != "$OBSERVER" ]] || require_observer_tuples "$ns"
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
  if [[ "$as" == system:serviceaccount:* ]]; then
    local account_ns="${as#system:serviceaccount:}"
    account_ns="${account_ns%%:*}"
    args+=(--as-group system:serviceaccounts --as-group "system:serviceaccounts:$account_ns" --as-group system:authenticated)
  fi
  kubectl auth can-i "$verb" "$res" ${args[@]+"${args[@]}"} --as "$as" 2>&1 | tail -n 1
}

# patch_refused AS NAMESPACE: a server-side dry run of a no-op merge patch of a Secret, as AS,
# must be refused (Forbidden). A patch answers with the changed Secret, data included, so a patch
# right is a read right (Kimi run 15, I6-1 finding 1). The Secret need not exist: authorization
# comes before the lookup (NotFound would mean the patch was allowed). -o name: never any data.
patch_refused() {
  local as="$1" ns="$2" account_ns out
  account_ns="${as#system:serviceaccount:}"
  account_ns="${account_ns%%:*}"
  out="$(kubectl -n "$ns" patch secret ruum-i-00000000000000000000000000000000 --type merge -p '{}' --dry-run=server -o name \
    --as "$as" --as-group system:serviceaccounts --as-group "system:serviceaccounts:$account_ns" --as-group system:authenticated 2>&1 | tail -n 1)"
  [[ "$out" == *"(Forbidden)"* ]] || problem "a no-op patch of a Secret in $ns as $as was not refused: ${out:-no answer} (want Forbidden)"
}

want() { # want yes|no AS NAMESPACE VERB RESOURCE [SUBRESOURCE]
  local w="$1" got
  shift
  got="$(can "$@")"
  [[ "$got" == "$w" ]] || problem "kubectl auth can-i $3 $4${5:+ --subresource $5} -n $2 --as $1: $got (want $w)"
}

# Render the fixed namespace loop without Helm. Refuse new template syntax rather than
# silently ignoring it. The deployer allow-list comes from these rendered Role rules.
effective_allowlist() {
  local template rendered records t doc line ns groups resources names verbs
  template="$(dirname "${BASH_SOURCE[0]}")/../charts/platform/templates/deployer-rbac.yaml"
  rendered="$(awk '
    $0 == "{{- range list \"ruum-deploy\" \"ruum\" \"edge\" }}" { loop=1; next }
    $0 == "{{- end }}" {
      split("ruum-deploy ruum edge", ns, " ")
      for (i=1;i<=3;i++) { out=body; gsub(/\{\{ \. \}\}/, ns[i], out); printf "%s", out }
      body=""; loop=0; next
    }
    loop { body=body $0 "\n"; next }
    { print }
    END { if (loop) exit 1 }
  ' "$template")" || return 1
  [[ "$rendered" != *'{{'* ]] || return 1
  records="$(docs /dev/stdin <<<"$rendered")" || return 1
  while IFS=$'\t' read -r t doc line; do
    [[ "$t" == L ]] || continue
    ns="$(awk -F'\t' -v d="$doc" '$1=="D" && $2==d && $3=="Role" && $5=="deployer" {print $4}' <<<"$records")"
    [[ -n "$ns" && "$line" =~ $RULE_RE ]] || return 1
    groups="${BASH_REMATCH[1]}" resources="${BASH_REMATCH[2]}" names="${BASH_REMATCH[4]}" verbs="${BASH_REMATCH[5]}"
    jq -cn --arg ns "$ns" --arg g "$groups" --arg r "$resources" --arg n "$names" --arg v "$verbs" '
      def parts: split(",") | map(gsub("^ +| +$"; "") | gsub("^[\"\u0027]|[\"\u0027]$"; ""));
      {namespace:$ns, apiGroups:($g|parts),resources:($r|parts),
       resourceNames:(if $n=="" then [] else $n|parts end),verbs:($v|parts)}' || return 1
  done <<<"$records"
}

audit_rights() {
  local identity="$1" ns="$2" allowed="$3" output rules rejected account_ns args=()
  account_ns="${identity#system:serviceaccount:}"
  account_ns="${account_ns%%:*}"
  [[ "$ns" == - ]] || args+=(-n "$ns")
  args+=(--as "$identity" --as-group system:serviceaccounts --as-group "system:serviceaccounts:$account_ns" --as-group system:authenticated)
  # The supported can-i --list interface is a table (including kubectl v1.37.2).
  # The normalizer also accepts a complete JSON review if a future client supplies it.
  if ! output="$(kubectl auth can-i --list "${args[@]}" 2>&1)"; then
    problem "cannot list rights for $identity in $ns"
    return
  fi
  if ! rules="$(printf '%s\n' "$output" | jq -Rse -f "$(dirname "${BASH_SOURCE[0]}")/rbac-rules.jq")"; then
    problem "unreadable or incomplete rights for $identity in $ns"
    return
  fi
  # An empty review (a header-only table, an empty JSON list) would make the audit below pass
  # without checking anything: every authenticated identity has at least the self-review and
  # discovery rules, so no rules means the listing did not work (Kimi, third O-51 review).
  if ! jq -e 'length > 0' >/dev/null 2>&1 <<<"$rules"; then
    problem "empty rights listing for $identity in $ns (expected at least the standard rules)"
    return
  fi
  if ! rejected="$(jq -c --arg identity "${identity##*:}" --arg ns "$ns" \
    --argjson deployer "$(printf '%s\n' "$allowed" | jq -s .)" \
    -f "$(dirname "${BASH_SOURCE[0]}")/rbac-audit.jq" <<<"$rules")"; then
    problem "cannot audit rights for $identity in $ns"
    return
  fi
  [[ -z "$rejected" ]] || problem "forbidden effective rules for $identity in $ns: $rejected"
}

live() {
  local d="system:serviceaccount:$DEPLOYER_NS:$DEPLOYER" c="system:serviceaccount:$CONTROLLER_NS:$CONTROLLER" ns
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
  # The runtime driver (backlog I6-1 and S-89, charts/platform runtime-rbac.yaml): the pods and
  # records of sandboxes in ruum-apps and TokenReviews, nothing else. No verb on Secrets (every
  # verb but create can read one, Kimi run 15 I6-1 finding 1, and none is needed since S-89),
  # no pod subresource but the log, no token, no RBAC change.
  local r="system:serviceaccount:$RUNTIME_NS:$RUNTIME" verb sub
  for verb in create get list delete; do want yes "$r" "$APPS_NS" "$verb" pods; done
  want yes "$r" "$APPS_NS" get pods log
  for verb in create get list update delete; do want yes "$r" "$APPS_NS" "$verb" configmaps; done
  for verb in create get list watch patch update delete deletecollection; do want no "$r" "$APPS_NS" "$verb" secrets; done
  patch_refused "$r" "$APPS_NS"
  want yes "$r" - create tokenreviews.authentication.k8s.io
  want no "$r" - create subjectaccessreviews.authorization.k8s.io
  # The sandbox pods' account (S-89): no right of its own.
  local sb="system:serviceaccount:$APPS_NS:ruum-sandbox"
  for verb in get list create; do
    want no "$sb" "$APPS_NS" "$verb" pods
    want no "$sb" "$APPS_NS" "$verb" configmaps
    want no "$sb" "$APPS_NS" "$verb" secrets
  done
  want no "$sb" - create tokenreviews.authentication.k8s.io
  for sub in exec attach portforward; do want no "$r" "$APPS_NS" create pods "$sub"; done
  want no "$r" "$APPS_NS" patch pods ephemeralcontainers
  want no "$r" "$APPS_NS" update pods
  want no "$r" "$APPS_NS" patch pods
  want no "$r" "$APPS_NS" create serviceaccounts token
  want no "$r" "$APPS_NS" create rolebindings.rbac.authorization.k8s.io
  want no "$r" "$APPS_NS" create roles.rbac.authorization.k8s.io
  for ns in ruum data auth authz edge ruum-deploy ops kube-system; do
    want no "$r" "$ns" create pods
    want no "$r" "$ns" get secrets
    want no "$r" "$ns" list secrets
    want no "$r" "$ns" create configmaps
  done
  want no "$r" - create pods
  want no "$r" - list secrets
  local o="system:serviceaccount:$OBSERVER_NS:$OBSERVER" resource
  for ns in ruum edge ruum-deploy; do
    for resource in pods events events.events.k8s.io deployments.apps daemonsets.apps replicasets.apps jobs.batch; do
      for verb in get list watch; do
        want yes "$o" "$ns" "$verb" "$resource"
      done
    done
    want yes "$o" "$ns" get pods log
  done
  # Query the inventory as the admin; an unreadable inventory must never pass.
  local namespaces bindings forbidden_bindings
  if ! namespaces="$(kubectl get ns -o json | jq -er '.items | map(.metadata.name) | .[]')"; then
    problem "cannot list live namespaces"
    return 1
  fi
  if ! bindings="$(kubectl get clusterrolebindings -o json)"; then
    problem "cannot list live ClusterRoleBindings"
    return 1
  fi
  # Only the five standard group bindings can grant cluster rights to the observer.
  # Their names and complete roleRefs must match. The rule audit also checks their rights.
  if ! forbidden_bindings="$(jq -er --arg ns "$OBSERVER_NS" --arg name "$OBSERVER" --arg user "$o" '
    [.items[] | select(any(.subjects[]?;
      (.kind == "ServiceAccount" and .name == $name and .namespace == $ns) or
      (.kind == "User" and .name == $user) or
      (.kind == "Group" and (.name == "system:serviceaccounts" or .name == ("system:serviceaccounts:" + $ns) or .name == "system:authenticated"))
    )) | select((
      (.metadata.name as $binding | ["system:basic-user", "system:discovery",
        "system:public-info-viewer", "system:cluster-trust-bundle-discovery",
        "system:service-account-issuer-discovery"] | index($binding)) != null and
      .roleRef == {apiGroup:"rbac.authorization.k8s.io", kind:"ClusterRole", name:.metadata.name} and
      (.metadata.name as $binding | all(.subjects[]; .kind == "Group" and
        (if $binding == "system:cluster-trust-bundle-discovery" or $binding == "system:service-account-issuer-discovery" then
          .name == "system:serviceaccounts"
        elif $binding == "system:public-info-viewer" then
          .name == "system:authenticated" or .name == "system:unauthenticated"
        else .name == "system:authenticated" end)))
    ) | not) | .metadata.name] | join(", ")' <<<"$bindings")"; then
    problem "cannot parse live ClusterRoleBindings"
    return 1
  fi
  [[ -z "$forbidden_bindings" ]] || problem "observer has forbidden ClusterRoleBindings: $forbidden_bindings"
  # Include the required namespaces even if the inventory unexpectedly omits one.
  namespaces="$(printf '%s\n' ruum edge ruum-deploy "$namespaces" | sort -u)"
  local allowed identity
  if ! allowed="$(effective_allowlist)"; then
    problem "cannot derive effective-rights allow-list"
    return 1
  fi
  for identity in "$o" "$d"; do
    for ns in $namespaces; do
      audit_rights "$identity" "$ns" "$allowed"
    done
    audit_rights "$identity" - "$allowed"
  done
  [[ $fail == 0 ]] && echo "rbac-check: the live deployer may only write release requests; the controller has the deploy rights and no Secret; the observer is read-only; the runtime driver has its sandbox rights in ruum-apps and TokenReviews only and no verb on Secrets (a no-op patch is refused); the sandbox account has none"
  return "$fail"
}

case "${1:-}" in
  static) shift; static "$@" ;;
  live) live ;;
  *) echo "usage: rbac-check.sh static FILE... | live" >&2; exit 2 ;;
esac
