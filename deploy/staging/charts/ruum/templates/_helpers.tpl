{{- define "ruum.restricted" -}}
securityContext:
  allowPrivilegeEscalation: false
  readOnlyRootFilesystem: true
  runAsNonRoot: true
  capabilities:
    drop: [ALL]
  seccompProfile:
    type: RuntimeDefault
{{- end }}
{{- define "ruum.byDigest" -}}
{{- if not (regexMatch "^[a-z0-9./-]+@sha256:[a-f0-9]{64}$" .) -}}
{{- fail (printf "image %q is not a reference by digest (repo@sha256:...)" .) -}}
{{- end -}}
{{ . }}
{{- end }}
{{- define "ruum.acmeCA" -}}
{{- if eq .Values.acme.issuer "letsencrypt-staging" -}}
https://acme-staging-v02.api.letsencrypt.org/directory
{{- else if eq .Values.acme.issuer "zerossl" -}}
{{- if not .Values.acme.email -}}{{- fail "acme.issuer zerossl needs acme.email (Caddy gets the EAB credentials with it)" -}}{{- end -}}
https://acme.zerossl.com/v2/DV90
{{- else -}}
{{- fail "acme.issuer must be letsencrypt-staging or zerossl (never production Let's Encrypt on staging)" -}}
{{- end -}}
{{- end }}
