# The fixture app of the Docker driver's tests (card R-2a): the conformance program and the
# sandbox probes (main.js). The base is pinned by the digest of its multi-arch index (arm64 build
# server, amd64 runners). No USER: the driver runs every sandbox as 10001:10001.
FROM node:22-alpine@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a
# A folder the sandbox user owns, so a failed write there proves the read-only root (not a
# missing permission).
RUN mkdir -p /probe-home && chown 10001:10001 /probe-home && chmod 0777 /probe-home
COPY main.js /app/main.js
ENTRYPOINT ["node", "/app/main.js"]
