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
# - Volatile functions (review Q-1, S-1; review-Q-2-s21-2 R-1, R-2; review-Q-2-harden H-1; decision
#   0004-38): PUBLIC may not run any VOLATILE function of pg_catalog in template1 nor in any
#   existing database (made before this rule), except the few that carry no state a secret can
#   reach (STATEMENT_VOLATILE in crates/ruum-dbengine/src/postgres/allow.rs; keep both lists the
#   same). ruum_dbhost holds EXECUTE WITH GRANT OPTION, so the engine gives them to the owner
#   kind of each database and to nobody else. Postgres marks a function IMMUTABLE or STABLE only
#   when it cannot write, so what stays cannot carry a value from one expression to another.
#   What is revoked: set_config, setseed, the advisory lock functions, the large object
#   functions, pg_notify, the functions that run a query given as text (query_to_xml, ts_stat) and
#   the like. current_setting (STABLE) is revoked too, so code that wrote a custom setting cannot
#   hand it to a reader; the engine gives it back to the owner, secret_read and mask kinds.
#   Postgres checks the right of a function in a view against the reader, so this holds
#   inside views too. Only a superuser can take a right on a pg_catalog function from PUBLIC. The
#   engine refuses a database where PUBLIC may still run one.
set -euo pipefail

# The volatile functions: revoked from PUBLIC, given to ruum_dbhost WITH GRANT OPTION. The list
# of the functions that stay is the one in allow.rs (STATEMENT_VOLATILE).
KEEP_VOLATILE='{nextval,currval,lastval,gen_random_uuid,uuidv7,clock_timestamp,timeofday,random,pg_sleep,pg_sleep_for,pg_sleep_until,pg_cancel_backend,pg_terminate_backend}'
# The readers of a session setting (SETTING_READERS in allow.rs): not volatile, but revoked with
# the volatile functions; the engine gives them back to the owner, secret_read and mask kinds.
SETTING_READERS='{current_setting}'
REVOKED_WHERE="p.pronamespace OPERATOR(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace
              AND p.prokind OPERATOR(pg_catalog.=) 'f'::pg_catalog.\"char\"
              AND ((p.provolatile OPERATOR(pg_catalog.=) 'v'::pg_catalog.\"char\"
                    AND p.proname::pg_catalog.text OPERATOR(pg_catalog.<>) ALL ('$KEEP_VOLATILE'::pg_catalog.text[]))
                   OR p.proname::pg_catalog.text OPERATOR(pg_catalog.=) ANY ('$SETTING_READERS'::pg_catalog.text[]))"
READERS_WHERE="p.pronamespace OPERATOR(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace
              AND p.prokind OPERATOR(pg_catalog.=) 'f'::pg_catalog.\"char\"
              AND p.proname::pg_catalog.text OPERATOR(pg_catalog.=) ANY ('$SETTING_READERS'::pg_catalog.text[])"
REVOKE_SQL=$(cat <<SQL
DO \$volatile\$
DECLARE f pg_catalog.text;
BEGIN
  SELECT pg_catalog.string_agg(p.oid::pg_catalog.regprocedure::pg_catalog.text, ', ') INTO f
    FROM pg_catalog.pg_proc p WHERE $REVOKED_WHERE;
  EXECUTE pg_catalog.format('REVOKE EXECUTE ON FUNCTION %s FROM PUBLIC', f);
  EXECUTE pg_catalog.format('GRANT EXECUTE ON FUNCTION %s TO ruum_dbhost WITH GRANT OPTION', f);
END \$volatile\$;
SQL
)
# The same functions, for the owner kind of a live database (granted by ruum_dbhost, as the
# engine does).
GRANT_OWNER_SQL=$(cat <<SQL
DO \$\$
DECLARE r pg_catalog.text; f pg_catalog.text;
BEGIN
  IF pg_catalog.to_regclass('ruum_meta.roles') IS NOT NULL THEN
    SELECT pg_catalog.string_agg(p.oid::pg_catalog.regprocedure::pg_catalog.text, ', ') INTO f
      FROM pg_catalog.pg_proc p WHERE $REVOKED_WHERE;
    FOR r IN SELECT m.role_name FROM ruum_meta.roles m
              JOIN pg_catalog.pg_roles g ON g.rolname OPERATOR(pg_catalog.=) m.role_name
             WHERE m.kind OPERATOR(pg_catalog.=) 'owner' LOOP
      EXECUTE 'SET LOCAL ROLE ruum_dbhost';
      EXECUTE pg_catalog.format('GRANT EXECUTE ON FUNCTION %s TO %I', f, r);
      EXECUTE 'RESET ROLE';
    END LOOP;
    SELECT pg_catalog.string_agg(p.oid::pg_catalog.regprocedure::pg_catalog.text, ', ') INTO f
      FROM pg_catalog.pg_proc p WHERE $READERS_WHERE;
    FOR r IN SELECT m.role_name FROM ruum_meta.roles m
              JOIN pg_catalog.pg_roles g ON g.rolname OPERATOR(pg_catalog.=) m.role_name
             WHERE m.kind OPERATOR(pg_catalog.=) ANY ('{secret_read,mask}'::pg_catalog.text[]) LOOP
      EXECUTE 'SET LOCAL ROLE ruum_dbhost';
      EXECUTE pg_catalog.format('GRANT EXECUTE ON FUNCTION %s TO %I', f, r);
      EXECUTE 'RESET ROLE';
    END LOOP;
  END IF;
END \$\$;
SQL
)

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

psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname template1 \
  -c "SET search_path = pg_catalog, pg_temp" -c "$REVOKE_SQL"
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

# The volatile functions in every other database of the cluster: the data databases made before this rule,
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
  psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname "$db" \
    -c "SET search_path = pg_catalog, pg_temp" -c "$REVOKE_SQL"
  psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname "$db" \
    -c "SET search_path = pg_catalog, pg_temp" -c "$GRANT_OWNER_SQL"
  if [[ $open != t ]]; then
    psql -v ON_ERROR_STOP=1 -X -q --username "${POSTGRES_USER:-postgres}" --dbname postgres \
      -c "ALTER DATABASE \"$qdb\" ALLOW_CONNECTIONS false"
  fi
done
