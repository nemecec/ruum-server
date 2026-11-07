# The Postgres tenant key store forgets the tenant filter when it reads a tenant's key
# (card Q-4, decision 0004-16). Security rule: every query filters by tenant_id (CLAUDE.md,
# design section 2.6). The test turns FORCE row-level security off on tenant_keys, so only the
# query filter keeps tenant B away from tenant A's row. Rule from review-Q-4 note 1 (verified
# KILLED by the reviewer). Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres tenant_keys read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/privacy/keys.rs"
PERL='s/where tenant_id = \$1 and purpose = \$2/where \$1::uuid is not null and purpose = \$2/'
TESTS="-p ruum-core --lib privacy::keys::tests::postgres_store_filters_by_tenant_without_rls"
PLACEHOLDER=0
