#!/usr/bin/env bash
# The cluster set-up of the data cluster (docs/STEP2.md "Infrastructure work" 2). It is
# idempotent: Postgres runs it on the first start, and `scripts/dev.sh up` runs it again (so a
# changed RUUM_DBHOST_PASSWORD in deploy/dev/.env reaches the role).
#
# - ruum_dbhost: CREATEDB and CREATEROLE, no superuser. The engine creates every database and its
#   per-database roles as ruum_dbhost.
# - No role but ruum_dbhost may connect to postgres; nobody may connect to template1.
# - template1 has no extension (besides plpgsql) and PUBLIC has no right on its schema public, so
#   each new database starts empty and closed.
set -euo pipefail

: "${RUUM_DBHOST_PASSWORD:?RUUM_DBHOST_PASSWORD is not set}"

psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname postgres \
  -v dbhost_pw="$RUUM_DBHOST_PASSWORD" <<'SQL'
SELECT 'CREATE ROLE ruum_dbhost'
  WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ruum_dbhost') \gexec
SELECT format('ALTER ROLE ruum_dbhost LOGIN NOSUPERUSER CREATEDB CREATEROLE NOREPLICATION NOBYPASSRLS INHERIT PASSWORD %L', :'dbhost_pw') \gexec
REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC;
REVOKE CONNECT, TEMPORARY ON DATABASE template1 FROM PUBLIC;
GRANT CONNECT ON DATABASE postgres TO ruum_dbhost;
SQL

psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname template1 <<'SQL'
REVOKE ALL ON SCHEMA public FROM PUBLIC;
DO $$
DECLARE e text;
BEGIN
  FOR e IN SELECT extname FROM pg_extension WHERE extname <> 'plpgsql' LOOP
    RAISE EXCEPTION 'template1 holds the extension %; the data cluster must not', e;
  END LOOP;
END $$;
SQL
