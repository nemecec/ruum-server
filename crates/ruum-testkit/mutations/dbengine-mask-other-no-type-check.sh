# ruum_mask.pseudonym_other converts any argument with v::text, without the check that its type
# is a built-in base type. A direct call with a value of a tenant type (tenant code in step 2 of
# a masked copy may make one) then runs the tenant's cast as mask.
# Security rule: decision 0004-37 (backlog S-1): only built-in base types reach a definer function.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="pseudonym_other converts a value of any type, a tenant-defined one too"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/if not exists \(select 1 from pg_catalog\.pg_type t/if false and not exists (select 1 from pg_catalog.pg_type t/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
