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
{{- /* ruum.appImage (list KIND REF): REF is ghcr.io/nemecec/ruum-KIND@sha256:<digest>, KIND gateway
       or egress (card I-6). */ -}}
{{- define "ruum.appImage" -}}
{{- $kind := index . 0 }}{{- $ref := index . 1 }}
{{- if not (has $kind (list "gateway" "egress")) }}{{- fail (printf "unknown app image kind %q" $kind) }}{{- end }}
{{- if not (regexMatch (printf "^ghcr\\.io/nemecec/ruum-%s@sha256:[a-f0-9]{64}$" $kind) $ref) -}}
{{- fail (printf "image %q is not ghcr.io/nemecec/ruum-%s@sha256:<digest> (card I-6)" $ref $kind) -}}
{{- end -}}
{{ $ref }}
{{- end }}
{{- /* ruum.apps: "true" when the app runtime renders (both images set); fails when only one is. */ -}}
{{- define "ruum.apps" -}}
{{- $g := .Values.images.gateway }}{{- $e := .Values.images.egress }}
{{- if and $g $e }}true{{- else if or $g $e }}{{- fail "images.gateway and images.egress are set together or not at all (card I-6)" }}{{- end }}
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

{{- /* The paths that the edge answers itself (backlog O-53). tofu/cluster/main.tf uses the same
       two paths in the load balancer's health checks (tests/run.sh checks that they agree). */}}
{{- define "ruum.edgeReadyPath" -}}
/_edge/ready
{{- end }}

{{- define "ruum.edgeAlivePath" -}}
/_edge/alive
{{- end }}

{{- /* The probe of the edge (backlog O-53): the ready path over HTTPS on the pod's address.
       The kubelet sends no server name, so Caddy answers with the public host's certificate
       (default_sni) or fails the handshake; the Host header picks the public site. */}}
{{- define "ruum.edgeProbe" -}}
httpGet:
  scheme: HTTPS
  port: https
  path: {{ include "ruum.edgeReadyPath" . }}
  httpHeaders:
    - { name: Host, value: {{ .Values.hosts.public }} }
timeoutSeconds: 3
{{- end }}
