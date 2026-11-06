#!/usr/bin/env bash
# Creates the roles and databases of the laptop stack. It is idempotent:
# Postgres runs it on the first start, and `scripts/dev.sh seed` runs it again.
set -euo pipefail

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname postgres \
  -v ruum_pw="$RUUM_DB_PASSWORD" -v zitadel_pw="$ZITADEL_DB_PASSWORD" -v spicedb_pw="$SPICEDB_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE ruum LOGIN CREATEDB PASSWORD %L', :'ruum_pw')
  WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ruum') \gexec
SELECT format('CREATE ROLE zitadel LOGIN PASSWORD %L', :'zitadel_pw')
  WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'zitadel') \gexec
SELECT format('CREATE ROLE spicedb LOGIN PASSWORD %L', :'spicedb_pw')
  WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'spicedb') \gexec

-- Zitadel and SpiceDB get clean databases from template0, without the Ruum extensions.
SELECT 'CREATE DATABASE zitadel OWNER zitadel TEMPLATE template0'
  WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'zitadel') \gexec
SELECT 'CREATE DATABASE spicedb OWNER spicedb TEMPLATE template0'
  WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'spicedb') \gexec
SQL

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname postgres <<'SQL'
SELECT 'CREATE DATABASE ruum OWNER ruum'
  WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'ruum') \gexec
SELECT 'CREATE DATABASE ruum_test OWNER ruum'
  WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'ruum_test') \gexec
SQL

# The ruum role is not a superuser, so it cannot create PostGIS or pgvector itself.
# The extensions also go into template1: the databases that #[sqlx::test] creates then have them.
for db in template1 ruum ruum_test; do
  psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$db" <<'SQL'
CREATE EXTENSION IF NOT EXISTS postgis;
CREATE EXTENSION IF NOT EXISTS vector;
SQL
done
