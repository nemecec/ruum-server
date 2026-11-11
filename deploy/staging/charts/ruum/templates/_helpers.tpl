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
{{- if not (regexMatch "^ghcr\\.io/nemecec/ruum-(server|edge)@sha256:[a-f0-9]{64}$" .) -}}
{{- fail (printf "image %q is not ghcr.io/nemecec/ruum-{server,edge}@sha256:<digest> (code review F-3)" .) -}}
{{- end -}}
{{ . }}
{{- end }}
{{- define "ruum.acmeCA" -}}
{{- if and .Values.acme.email (not (regexMatch "^[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9-]{1,63}(\\.[A-Za-z0-9-]{1,63})*\\.[A-Za-z]{2,24}$" .Values.acme.email)) -}}
{{- fail "acme.email is not a plain email address (code review F-3)" -}}
{{- end -}}
{{- if eq .Values.acme.issuer "letsencrypt-staging" -}}
https://acme-staging-v02.api.letsencrypt.org/directory
{{- else if eq .Values.acme.issuer "zerossl" -}}
{{- if not .Values.acme.email -}}{{- fail "acme.issuer zerossl needs acme.email (Caddy gets the EAB credentials with it)" -}}{{- end -}}
https://acme.zerossl.com/v2/DV90
{{- else -}}
{{- fail "acme.issuer must be letsencrypt-staging or zerossl (never production Let's Encrypt on staging)" -}}
{{- end -}}
{{- end }}
{{- /* The commit of the release: empty (a test render) or 40 hex characters (code review F-3). */ -}}
{{- define "ruum.checkCommit" -}}
{{- if and .Values.commit (not (regexMatch "^[0-9a-f]{40}$" .Values.commit)) -}}
{{- fail "commit must be 40 lowercase hex characters (code review F-3)" -}}
{{- end -}}
{{- end }}
