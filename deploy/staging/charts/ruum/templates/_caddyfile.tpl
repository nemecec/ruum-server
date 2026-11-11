{{- define "ruum.caddyfile" -}}
{
	admin off
	http_port 8080
	https_port 8443
	{{- with .Values.acme.email }}
	email {{ . }}
	{{- end }}
	acme_ca {{ include "ruum.acmeCA" . }}
	storage file_system /data/caddy
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
# APIs answer only to the build server (review 1 B-8, stgsec M-4).
{{ .Values.hosts.auth }} {
	import platform_headers
	@public path /.well-known/* /oauth/v2/* /oidc/v1/* /ui/login/* /idps/callback /idps/callback/* /assets/v1/*
	@admin remote_ip{{ range .Values.adminSources }} {{ . }}{{ end }}
	handle @public {
		reverse_proxy h2c://zitadel.auth.svc.cluster.local:8080
	}
	handle @admin {
		reverse_proxy h2c://zitadel.auth.svc.cluster.local:8080
	}
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
{{- end }}
