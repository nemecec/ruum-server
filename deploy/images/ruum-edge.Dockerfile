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

FROM caddy:2.11.9-alpine
# The official binary carries the cap_net_bind_service file capability. The edge listens on
# 8080/8443 and runs with all capabilities dropped and no_new_privs (Pod Security restricted),
# so remove the capability, or exec could be refused.
RUN apk add --no-cache libcap && setcap -r /usr/bin/caddy && apk del libcap
COPY --from=web /web/dist /srv
USER 65532:65532
EXPOSE 8080 8443
CMD ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
