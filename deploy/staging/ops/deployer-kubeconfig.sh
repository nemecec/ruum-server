#!/usr/bin/env bash
# Writes the kubeconfig of the ServiceAccount ruum/deployer (since backlog O-34 it may only write
# release requests for the deploy controller, charts/platform/templates/deployer-rbac.yaml) to
# ~/.config/ruum/staging/deployer.kubeconfig (0600, on the Mac). deploy.sh uses it; the admin
# kubeconfig stays for up.sh, upgrades and destroy (review 1 S-2, review 2 O-1).
# Pass observer for the separate troubleshooting identity (backlog O-51).
set -euo pipefail
identity="${1:-deployer}"
case "$identity" in
  deployer | observer) ;;
  *) echo "usage: deployer-kubeconfig.sh [deployer|observer]" >&2; exit 2 ;;
esac
WORK="${RUUM_STAGING_WORK:-$HOME/.config/ruum/staging}"
out="$WORK/$identity.kubeconfig"
umask 077
server="$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
ca="$(kubectl config view --raw --minify -o jsonpath='{.clusters[0].cluster.certificate-authority-data}')"
for _ in $(seq 1 30); do
  token="$(kubectl -n ruum get secret "$identity-token" -o jsonpath='{.data.token}' 2>/dev/null | base64 -d || true)"
  [[ -n "$token" ]] && break
  sleep 2
done
[[ -n "${token:-}" ]] || { echo "$identity-kubeconfig: no token in ruum/$identity-token" >&2; exit 1; }
# Restrict an existing file before writing the credential too.
touch "$out"
chmod 600 "$out"
cat >"$out" <<YAML
apiVersion: v1
kind: Config
clusters:
  - name: ruum-staging
    cluster:
      server: $server
      certificate-authority-data: $ca
users:
  - name: $identity
    user:
      token: $token
contexts:
  - name: $identity
    context:
      cluster: ruum-staging
      user: $identity
      namespace: ruum
current-context: $identity
YAML
chmod 600 "$out"
echo "$identity kubeconfig: $out" >&2
