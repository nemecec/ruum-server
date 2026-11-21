# The Postgres page of telemetry_fields forgets its tenant filter (card O-2). Security rule: every
# query filters by tenant_id (CLAUDE.md). The test turns FORCE row-level security off on the
# table, so only the query filter keeps tenant B away from tenant A's rows.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres telemetry_fields page drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/where tenant_id = \$1 and app_node_id = \$2 \\\n(\s*)and \(\$3::bool/where \$1::uuid is not null and app_node_id = \$2 \\\n$1and (\$3::bool/'
TESTS="-p ruum-core --lib telemetry::classes::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
