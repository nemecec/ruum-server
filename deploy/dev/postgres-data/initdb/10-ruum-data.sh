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
# - set_config (review Q-1, S-1): PUBLIC may not run pg_catalog.set_config in template1 nor in
#   any existing database (made before this rule), and ruum_dbhost holds EXECUTE WITH GRANT
#   OPTION, so the engine gives it to the owner kind of each database and to nobody else. Only a
#   superuser can take a right on a pg_catalog function from PUBLIC. A query given as text
#   (query_to_xml, ts_stat) can call set_config whatever the statement rules check, so this is
#   the line that holds. The engine refuses a database where PUBLIC may still run it.
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
REVOKE EXECUTE ON FUNCTION pg_catalog.set_config(text, text, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION pg_catalog.set_config(text, text, boolean) TO ruum_dbhost WITH GRANT OPTION;
DO $$
DECLARE e text;
BEGIN
  FOR e IN SELECT extname FROM pg_extension WHERE extname <> 'plpgsql' LOOP
    RAISE EXCEPTION 'template1 holds the extension %; the data cluster must not', e;
  END LOOP;
END $$;
SQL

# set_config in every other database of the cluster: the data databases made before this rule,
# also the closed ones (restore points, copies being made), which are opened for the moment of
# the change. The owner kind of a live database (ruum_meta.roles) keeps EXECUTE, granted by
# ruum_dbhost as the engine does.
psql -v ON_ERROR_STOP=1 -X -q -At --username "${POSTGRES_USER:-postgres}" --dbname postgres \
  -c "SELECT datname || '|' || CASE WHEN datallowconn THEN 't' ELSE 'f' END FROM pg_database
       WHERE datname NOT IN ('postgres', 'template0', 'template1') ORDER BY datname" |
while IFS='|' read -r db open; do
  [[ -n $db ]] || continue
  qdb="$(printf '%s' "$db" | sed 's/"/""/g')"
  if [[ $open != t ]]; then
    psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname postgres \
      -c "ALTER DATABASE \"$qdb\" ALLOW_CONNECTIONS true"
  fi
  # A tenant may own objects in schema public of a data database: the superuser resolves names
  # in pg_catalog only, and the grant to the owner kind is made as ruum_dbhost, so that the
  # engine can take it back when a copy changes hands.
  psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname "$db" <<'SQL'
SET search_path = pg_catalog, pg_temp;
REVOKE EXECUTE ON FUNCTION pg_catalog.set_config(text, text, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION pg_catalog.set_config(text, text, boolean) TO ruum_dbhost WITH GRANT OPTION;
DO $$
DECLARE r text;
BEGIN
  IF pg_catalog.to_regclass('ruum_meta.roles') IS NOT NULL THEN
    FOR r IN SELECT m.role_name FROM ruum_meta.roles m
              JOIN pg_catalog.pg_roles g ON g.rolname OPERATOR(pg_catalog.=) m.role_name
             WHERE m.kind OPERATOR(pg_catalog.=) 'owner' LOOP
      EXECUTE 'SET LOCAL ROLE ruum_dbhost';
      EXECUTE pg_catalog.format(
        'GRANT EXECUTE ON FUNCTION pg_catalog.set_config(text, text, boolean) TO %I', r);
      EXECUTE 'RESET ROLE';
    END LOOP;
  END IF;
END $$;
SQL
  if [[ $open != t ]]; then
    psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname postgres \
      -c "ALTER DATABASE \"$qdb\" ALLOW_CONNECTIONS false"
  fi
done
