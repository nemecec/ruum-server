# Standard authenticated-user and ServiceAccount rules, shared by both identities and every scope.
# Discovery wildcards below approve only URLs under their specific prefixes.
def standard:
  [{apiGroups:["authorization.k8s.io"],resources:["selfsubjectaccessreviews","selfsubjectrulesreviews"],verbs:["create"]},
   {apiGroups:["authentication.k8s.io"],resources:["selfsubjectreviews"],verbs:["create"]},
   {apiGroups:["certificates.k8s.io"],resources:["clustertrustbundles"],verbs:["get","list","watch"]},
   {nonResourceURLs:["/.well-known/openid-configuration","/.well-known/openid-configuration/","/openid/v1/jwks","/openid/v1/jwks/"],verbs:["get"]},
   {nonResourceURLs:["/api","/api/*","/apis","/apis/*","/healthz","/livez","/readyz","/version","/version/","/openapi","/openapi/*"],verbs:["get"]}];
def covers_url($url):
  . == $url or (endswith("*") and (.[:-1] as $prefix | $url | startswith($prefix)));
def observer:
  [{apiGroups:[""],resources:["pods","events"],verbs:["get","list","watch"]},
   {apiGroups:["events.k8s.io"],resources:["events"],verbs:["get","list","watch"]},
   {apiGroups:["apps"],resources:["deployments","daemonsets","replicasets"],verbs:["get","list","watch"]},
   {apiGroups:["batch"],resources:["jobs"],verbs:["get","list","watch"]},
   {apiGroups:[""],resources:["pods/log"],verbs:["get"]}];
# Without -n kubectl uses the kubeconfig's namespace, not a cluster-only review.
# Accept the union there; the audit in EVERY namespace separately detects cluster
# grants, since those also appear in namespaces outside the allowed three.
(standard + (if $identity == "observer" then
  if (["ruum","edge","ruum-deploy","-"]|index($ns)) != null then observer else [] end
else [$deployer[] | select($ns == "-" or .namespace == $ns)] end)) as $allowed |
.[] | . as $rule |
select((
  all(.nonResourceURLs[]; . as $url |
    all($rule.verbs[]; . as $verb | any($allowed[];
      any((.nonResourceURLs // [])[]; covers_url($url)) and (.verbs|index($verb)) != null))) and
  all(.apiGroups[]; . as $group |
    all($rule.resources[]; . as $resource |
      all($rule.verbs[]; . as $verb |
        # A restricted rule must be covered name by name; unrestricted grants
        # require an unrestricted allow-list entry.
        all((if ($rule.resourceNames|length)==0 then [null] else $rule.resourceNames end)[];
          . as $name | any($allowed[];
            ((.apiGroups // [])|index($group)) != null and
            ((.resources // [])|index($resource)) != null and (.verbs|index($verb)) != null and
            (((.resourceNames // [])|length)==0 or ($name != null and (.resourceNames|index($name)) != null)))))))
) | not)
