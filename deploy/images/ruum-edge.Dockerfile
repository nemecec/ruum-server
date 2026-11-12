# syntax=docker/dockerfile:1.7
# The staging edge (plan 5.7): Caddy with the built web UI in /srv. The Caddyfile comes from the
# chart (ConfigMap), so a route change needs no new image. Built by images.yml on GitHub.
FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web ./
COPY contracts /contracts
RUN npm run build

# Caddy with the certificate storage in Kubernetes Secrets (backlog O-26):
# deploy/images/caddy-secret-storage, tested here before the build.
FROM caddy:2.11.9-builder-alpine AS caddy
WORKDIR /src/caddy-secret-storage
COPY deploy/images/caddy-secret-storage ./
RUN go vet ./... && go test -count=1 ./...
# XCADDY_SETCAP=0: the builder image would add cap_net_bind_service (see the stage below).
RUN XCADDY_SETCAP=0 xcaddy build v2.11.9 --output /out/caddy \
      --with github.com/nemecec/ruum-server/deploy/images/caddy-secret-storage=/src/caddy-secret-storage && \
    /out/caddy list-modules | grep -qx caddy.storage.kubernetes_secret && \
    printf '{\n\tstorage kubernetes_secret {\n\t\tnamespace edge\n\t}\n}\n:8080\n' >/tmp/Caddyfile && \
    /out/caddy adapt --config /tmp/Caddyfile --adapter caddyfile | grep -q '"module":"kubernetes_secret"'

FROM caddy:2.11.9-alpine
# The edge listens on 8080/8443 and runs with all capabilities dropped and no_new_privs (Pod
# Security restricted). The binary built above carries no file capability (the official one
# has cap_net_bind_service, which exec could refuse).
COPY --from=caddy /out/caddy /usr/bin/caddy
COPY --from=web /web/dist /srv
USER 65532:65532
EXPOSE 8080 8443
CMD ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
