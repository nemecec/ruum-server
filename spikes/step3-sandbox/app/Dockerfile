# Spike S-2: the small Node app as an image (no build step, no dependencies).
FROM node:22-alpine
COPY server.js /app/server.js
USER 10001:10001
EXPOSE 8080
CMD ["node", "/app/server.js"]
