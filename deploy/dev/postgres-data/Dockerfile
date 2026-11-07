# The data cluster of the laptop stack (docs/STEP2.md "Infrastructure work" 1 and 2): Postgres 17
# with PostGIS and pgvector, for customer databases only. It is not the platform database.
# No PostgreSQL Anonymizer (decision 0004-4): the lens views replace it.
FROM postgres:17.11-bookworm

RUN apt-get update \
 && apt-get install -y --no-install-recommends postgresql-17-postgis-3 postgresql-17-pgvector \
 && rm -rf /var/lib/apt/lists/*

# ruum_dbhost is not a superuser, so it can create only trusted extensions. Mark postgis and
# vector as trusted (decision 0004-34 g). This is safe here: only ruum_dbhost has CREATE on a
# database, and no per-database role does. Nothing goes into template1 (unlike deploy/dev/postgres).
RUN set -eu; \
    dir="$(pg_config --sharedir)/extension"; \
    for ext in postgis vector; do \
      f="$dir/$ext.control"; \
      sed -i '/^[[:space:]]*trusted[[:space:]]*=/d' "$f"; \
      echo 'trusted = true' >>"$f"; \
      grep -qx 'trusted = true' "$f"; \
    done; \
    install -d -o postgres -g postgres -m 0755 /etc/postgresql/ruum

COPY ruum-data-entrypoint.sh /usr/local/bin/ruum-data-entrypoint.sh
COPY initdb/ /docker-entrypoint-initdb.d/

ENTRYPOINT ["ruum-data-entrypoint.sh"]

# The settings are on the command line, so ALTER SYSTEM cannot change them.
# - hba_file: written by the entrypoint on every start (the platform address can change).
# - listen_addresses: IPv4 inside the container; the host port binds to 127.0.0.1 only (compose).
# - max_prepared_transactions = 0 (decision 0004-34 d).
# - WAL archiving is on. Until card I-2 (pgBackRest to Garage) the archive command keeps
#   nothing; I-2 changes only archive_command, which needs no restart.
# - shared_preload_libraries is empty: no pg_stat_statements.
CMD ["postgres", \
     "-c", "hba_file=/etc/postgresql/ruum/pg_hba.conf", \
     "-c", "listen_addresses=0.0.0.0", \
     "-c", "password_encryption=scram-sha-256", \
     "-c", "max_prepared_transactions=0", \
     "-c", "max_connections=200", \
     "-c", "shared_preload_libraries=", \
     "-c", "wal_level=replica", \
     "-c", "archive_mode=on", \
     "-c", "archive_command=/bin/true"]
