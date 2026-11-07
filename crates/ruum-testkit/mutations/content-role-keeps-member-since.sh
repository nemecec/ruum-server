# A change of the content role alone does not move memberships.updated_at (Postgres store).
# Security rule: the changes feed dates a membership from its last change of role or content
# role (decisions 0002-6 and 0003-1); a date that is too early lets a member who was just
# upgraded to manage learn about nodes that were trashed while it could not restore them.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="the Postgres tenant store keeps updated_at when only the content role changes"
FILE="crates/ruum-core/src/tenants/postgres.rs"
PERL='s/set role = \$3, content_role = \$4, updated_at = now\(\)/set role = \$3, content_role = \$4, updated_at = case when role = \$3 then updated_at else now() end/'
TESTS="-p ruum-core --lib tenants::tests::postgres_impl::member_since_dates_the_membership -- --exact"
PLACEHOLDER=0
