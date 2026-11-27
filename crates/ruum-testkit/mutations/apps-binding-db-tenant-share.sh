# The engine forgets the tenant's share of the app connection budget (backlog D5-10, card
# B-1d). Security rule: one tenant's app roles never take the room of every other tenant's
# bindings on the data cluster. Needs the data cluster (RUUM_DBHOST_URL).
DESCRIPTION="a tenant's app roles may take the whole app budget"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/if tenant_used\(&mut admin, &tag\)\.await\? \+ limit > tenant_share\(engine, budget\) \{/if tenant_used(&mut admin, &tag).await? + limit < 0 && tenant_share(engine, budget) > 0 {/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::a_tenant_holds_at_most_its_share_of_the_budget -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
