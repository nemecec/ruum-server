# Spike S-2: the small Node app as an image (no build step, no dependencies).
# The base image by digest (node:22-alpine, multi-arch index, 2026-11-13); run.sh uses the same.
FROM node:22-alpine@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a
COPY server.js /app/server.js
USER 10001:10001
EXPOSE 8080
CMD ["node", "/app/server.js"]
