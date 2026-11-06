# Postgres 17 with PostGIS and pgvector, for the laptop stack.
# The official image has the PGDG apt repository, which has both extensions for arm64 and amd64.
FROM postgres:17.11-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends postgresql-17-postgis-3 postgresql-17-pgvector \
 && rm -rf /var/lib/apt/lists/*
COPY initdb/ /docker-entrypoint-initdb.d/
