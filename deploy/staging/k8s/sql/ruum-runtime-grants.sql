-- The rights of ruum_app, the database role of the serving ruum-server pods (Kimi review of
-- O-25, findings 1 and 5). k8s/apply.sh runs this file in the database ruum on the CNPG primary,
-- as the superuser over the local socket, on every platform step. Every statement is idempotent.
--
-- ruum owns the database, the schema public and every table, because the migrations run as ruum
-- (the migration Job, Secret ruum/ruum-migrate-db). ruum_app gets data rights only:
-- - no CREATE on the database or the schema public, and it does not own anything: no DDL;
-- - SELECT only on _sqlx_migrations: a compromised server cannot fake the migration records
--   that the start-up check and the migration step read;
-- - no membership in ruum (CNPG makes the role with inherit: false and no inRoles).
-- Row-level security applies to both roles (the tables FORCE it), so nothing changes there.
--
-- Tables that later migrations create get the same rights through the default privileges of
-- ruum. A fresh cluster has no _sqlx_migrations yet when this runs; the migration step revokes
-- the write rights on it from ruum_app after each run (ruum-server migrate, RUUM_DB_RUNTIME_ROLE).
\set ON_ERROR_STOP on

GRANT CONNECT ON DATABASE ruum TO ruum_app;
REVOKE CREATE ON DATABASE ruum FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO ruum_app;

GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO ruum_app;
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO ruum_app;
ALTER DEFAULT PRIVILEGES FOR ROLE ruum IN SCHEMA public
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO ruum_app;
ALTER DEFAULT PRIVILEGES FOR ROLE ruum IN SCHEMA public
  GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO ruum_app;

DO $$
BEGIN
  IF to_regclass('public._sqlx_migrations') IS NOT NULL THEN
    REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER ON public._sqlx_migrations FROM ruum_app;
    GRANT SELECT ON public._sqlx_migrations TO ruum_app;
  END IF;
END
$$;

-- Fail the platform step when ruum_app could still change the schema or the migration records.
DO $$
BEGIN
  IF pg_has_role('ruum_app', 'ruum', 'MEMBER') THEN
    RAISE EXCEPTION 'ruum_app has the rights of ruum (role membership)';
  END IF;
  IF has_schema_privilege('ruum_app', 'public', 'CREATE') OR has_database_privilege('ruum_app', 'ruum', 'CREATE') THEN
    RAISE EXCEPTION 'ruum_app may create objects in the database ruum';
  END IF;
  IF to_regclass('public._sqlx_migrations') IS NOT NULL
     AND has_table_privilege('ruum_app', 'public._sqlx_migrations', 'INSERT, UPDATE, DELETE, TRUNCATE') THEN
    RAISE EXCEPTION 'ruum_app may write _sqlx_migrations';
  END IF;
END
$$;
