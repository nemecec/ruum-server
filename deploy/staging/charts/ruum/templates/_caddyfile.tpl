{{- define "ruum.caddyfile" -}}
{
	admin off
	http_port 8080
	https_port 8443
	{{- with .Values.acme.email }}
	email {{ . }}
	{{- end }}
	acme_ca {{ include "ruum.acmeCA" . }}
	# A TLS handshake without a server name (the kubelet's HTTPS probe, maybe the load
	# balancer's health check) gets the certificate of the public host, so the readiness probe
	# proves that this edge has loaded its certificates (backlog O-53). Without them the
	# handshake fails.
	default_sni {{ .Values.hosts.public }}
	# Certificates, ACME accounts and challenge tokens in Kubernetes Secrets of the edge
	# namespace (backlog O-26): a restart never issues again, and every edge pod answers the
	# HTTP-01 challenges of the others. Module: deploy/images/caddy-secret-storage.
	storage kubernetes_secret {
		namespace edge
	}
	# After SIGTERM: no new connections, open ones may finish (design 12.7).
	grace_period {{ int .Values.edge.shutdownGraceSeconds }}s
	on_demand_tls {
		ask http://ruum-server-internal.ruum.svc.cluster.local:8493/internal/tls-ask
	}
	servers {
		listener_wrappers {
			proxy_protocol {
				timeout 5s
				allow {{ .Values.proxyProtocolFrom }}
			}
			tls
		}
		protocols h1 h2
	}
}

(platform_headers) {
	header {
		# HSTS only on the staging.ruum.space names, never on ruumapps.net (stgsec S-6).
		# Short while the CA is the untrusted Let's Encrypt staging CA, so a browser that
		# saw it is not locked out for a year.
		Strict-Transport-Security "max-age={{ if eq .Values.acme.issuer "letsencrypt-staging" }}300{{ else }}31536000{{ end }}; includeSubDomains"
		X-Content-Type-Options nosniff
		Referrer-Policy strict-origin-when-cross-origin
		-Server
	}
}

{{ .Values.hosts.public }} {
	import platform_headers
	encode zstd gzip
	# Readiness of this edge (backlog O-53): the kubelet's probes and the load balancer's HTTPS
	# health check. Caddy answers it itself, so it needs this host's certificate but no upstream:
	# a slow ruum-server never takes the edges out of the load balancer.
	handle {{ include "ruum.edgeReadyPath" . }} {
		respond "ready" 200
	}
	@server path /v1/* /healthz /.well-known/* /mcp /mcp/*
	handle @server {
		reverse_proxy ruum-server.ruum.svc.cluster.local:8491
	}
	handle {
		root * /srv
		try_files {path} /index.html
		file_server
	}
}

# WebDAV host (decision 0002-3): long transfers, no body limit, no cookies.
{{ .Values.hosts.dav }} {
	import platform_headers
	reverse_proxy ruum-server.ruum.svc.cluster.local:8492 {
		transport http {
			read_timeout 0
			write_timeout 0
		}
	}
}

# Zitadel: sign-in and OIDC are public; the console and the admin, management and system
# APIs answer only to adminSources, which is empty by default: 403 for every public address
# (review 1 B-8, stgsec M-4).
{{ .Values.hosts.auth }} {
	import platform_headers
	@public path /.well-known/* /oauth/v2/* /oidc/v1/* /ui/login/* /idps/callback /idps/callback/* /assets/v1/*
	handle @public {
		reverse_proxy h2c://zitadel.auth.svc.cluster.local:8080
	}
{{- if .Values.adminSources }}
	@admin remote_ip{{ range .Values.adminSources }} {{ . }}{{ end }}
	handle @admin {
		reverse_proxy h2c://zitadel.auth.svc.cluster.local:8080
	}
{{- end }}
	handle {
		respond "Forbidden" 403
	}
}

# The content domain (design 15): ruum-server sets the sandbox CSP and
# Content-Disposition: attachment on every user file.
{{ .Values.hosts.content }} {
	header X-Content-Type-Options nosniff
	header -Server
	reverse_proxy ruum-server.ruum.svc.cluster.local:8491
}

# Tenant content hosts: on-demand TLS only here (review 1 B-6), never globally. Caddy asks
# ruum-server's internal listener first and fails closed.
*.{{ .Values.hosts.content }} {
	tls {
		on_demand
	}
	header X-Content-Type-Options nosniff
	header -Server
	reverse_proxy ruum-server.ruum.svc.cluster.local:8491
}

{{- if include "ruum.apps" . }}

# App origins (card I-6): every app and preview host <label>.<tenant>.<apps domain> goes to the
# app gateway, which checks every request (docs/STEP3.md "How an app runs" rule 3) and sets the
# answer's headers itself. On-demand TLS only here and for the tenant content hosts: Caddy asks
# ruum-server's internal listener, which answers 200 only for a host of an existing app, and
# fails closed. No HSTS (ruumapps.net, stgsec S-6). The gateway drops X-Forwarded-*, Forwarded
# and X-Real-IP before an app sees the request (decision 0006 G).
*.*.{{ .Values.hosts.apps }} {
	tls {
		on_demand
	}
	header -Server
	reverse_proxy ruum-gateway.ruum.svc.cluster.local:8494
}
{{- end }}

# Port 80 as an explicit site. Without it, automatic HTTPS creates the HTTP redirect server at
# runtime, outside this config, so it never gets the proxy_protocol listener wrapper: the load
# balancer's PROXY header then reaches the HTTP parser, every request on port 80 answers 400,
# and the ACME HTTP-01 challenge fails (staging bring-up, 2026-11-12). Caddy still answers
# HTTP-01 challenges on this server before the redirect.
# The load balancer's HTTP health check of port 80 (backlog O-53) needs no certificate, so a
# new cluster can answer its first ACME HTTP-01 challenges.
http:// {
	handle {{ include "ruum.edgeAlivePath" . }} {
		respond "alive" 200
	}
	handle {
		redir https://{host}{uri} 308
	}
}
{{- end }}
