#!/usr/bin/env bash
# Writes the kubeconfig of the ServiceAccount ruum/deployer (Helm in `ruum` and `edge` only,
# charts/platform/templates/deployer-rbac.yaml) to ~/.ruum-staging/deployer.kubeconfig (0600).
# The release agent and the deploy timer use it; the admin kubeconfig stays for up.sh,
# upgrades and destroy (review 1 S-2, review 2 O-1).
set -euo pipefail
WORK="${RUUM_STAGING_WORK:-$HOME/.ruum-staging}"
out="$WORK/deployer.kubeconfig"
umask 077
server="$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
ca="$(kubectl config view --raw --minify -o jsonpath='{.clusters[0].cluster.certificate-authority-data}')"
for _ in $(seq 1 30); do
  token="$(kubectl -n ruum get secret deployer-token -o jsonpath='{.data.token}' 2>/dev/null | base64 -d || true)"
  [[ -n "$token" ]] && break
  sleep 2
done
[[ -n "${token:-}" ]] || { echo "deployer-kubeconfig: no token in ruum/deployer-token" >&2; exit 1; }
cat >"$out" <<YAML
apiVersion: v1
kind: Config
clusters:
  - name: ruum-staging
    cluster:
      server: $server
      certificate-authority-data: $ca
users:
  - name: deployer
    user:
      token: $token
contexts:
  - name: deployer
    context:
      cluster: ruum-staging
      user: deployer
      namespace: ruum
current-context: deployer
YAML
chmod 600 "$out"
echo "deployer kubeconfig: $out" >&2
