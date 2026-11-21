-- The rights of ruum_gateway, the database role of the app gateway (card I-6; backlog G1A-4;
-- migration 0013). k8s/apply.sh runs this file in the database ruum on the CNPG primary, as the
-- superuser over the local socket, on every platform step. Every statement is idempotent.
--
-- The gateway holds no general system access (decision 0006-1): its role may execute
-- ruum_app_host_lookup(text, text, text) and nothing else. CNPG makes the role (managed roles,
-- charts/platform cnpg.yaml, with LOGIN and no inherit). Migration 0013 grants EXECUTE itself when
-- the role exists while it runs; this file grants it again for a database where 0013 ran before
-- the role existed. On a fresh database (no function yet) the grant is skipped here, and 0013
-- grants it when the migration Job runs.
\set ON_ERROR_STOP on

GRANT CONNECT ON DATABASE ruum TO ruum_gateway;

DO $$
BEGIN
  IF to_regprocedure('public.ruum_app_host_lookup(text, text, text)') IS NOT NULL THEN
    GRANT EXECUTE ON FUNCTION public.ruum_app_host_lookup(text, text, text) TO ruum_gateway;
  END IF;
END
$$;

-- Fail the platform step when the gateway's role could do more.
DO $$
DECLARE
  n int;
BEGIN
  IF pg_has_role('ruum_gateway', 'ruum', 'MEMBER') OR pg_has_role('ruum_gateway', 'ruum_app', 'MEMBER') THEN
    RAISE EXCEPTION 'ruum_gateway has the rights of ruum or ruum_app (role membership)';
  END IF;
  IF has_schema_privilege('ruum_gateway', 'public', 'CREATE') OR has_database_privilege('ruum_gateway', 'ruum', 'CREATE') THEN
    RAISE EXCEPTION 'ruum_gateway may create objects in the database ruum';
  END IF;
  -- No table, view or sequence: not directly, not through PUBLIC.
  SELECT count(*) INTO n
  FROM pg_class c JOIN pg_namespace s ON s.oid = c.relnamespace
  WHERE s.nspname = 'public' AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
    AND CASE WHEN c.relkind IN ('r', 'p', 'v', 'm', 'f')
        THEN has_table_privilege('ruum_gateway', c.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER') ELSE false END;
  IF n > 0 THEN
    RAISE EXCEPTION 'ruum_gateway has rights on % tables of the database ruum', n;
  END IF;
  SELECT count(*) INTO n
  FROM pg_class c JOIN pg_namespace s ON s.oid = c.relnamespace
  WHERE s.nspname = 'public' AND c.relkind = 'S'
    -- CASE: the planner may call the function before the relkind filter, and it fails on a
    -- relation that is not a sequence.
    AND CASE WHEN c.relkind = 'S' THEN has_sequence_privilege('ruum_gateway', c.oid, 'USAGE, SELECT, UPDATE') ELSE false END;
  IF n > 0 THEN
    RAISE EXCEPTION 'ruum_gateway has rights on % sequences of the database ruum', n;
  END IF;
  -- No other function that runs with its owner's rights.
  SELECT count(*) INTO n
  FROM pg_proc p JOIN pg_namespace s ON s.oid = p.pronamespace
  WHERE s.nspname = 'public' AND p.prosecdef
    AND p.oid <> coalesce(to_regprocedure('public.ruum_app_host_lookup(text, text, text)')::oid, 0)
    AND has_function_privilege('ruum_gateway', p.oid, 'EXECUTE');
  IF n > 0 THEN
    RAISE EXCEPTION 'ruum_gateway may execute % other security definer functions', n;
  END IF;
END
$$;
