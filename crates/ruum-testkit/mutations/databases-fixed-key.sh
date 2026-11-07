# createDatabase installs a fixed pseudonym key instead of the tenant key (card D-1). Security
# rule: decision 0004-30: the key of a new database comes from the tenant keys (card Q-4) and
# the engine installs it; two tenants never share pseudonyms. The test reads pseudonyms
# through the masked lens of databases the provider made. Needs the data cluster
# (RUUM_DBHOST_URL of scripts/dev.sh env); RUUM_DBENGINE_REQUIRE_POSTGRES makes a missing
# cluster fail the test instead of skipping it.
DESCRIPTION="the database provider hands the engine a fixed key"
FILE="crates/ruum-core/src/databases/provider.rs"
PERL='s/let key = self\.deps\.keys\.engine_key\(tenant, None\)\.await\?;/let key = ruum_dbengine::PseudonymKey::from_bytes([7u8; 32]);/'
TESTS="-p ruum-core --lib databases::tests::databases_of_a_tenant_share_the_tenant_key -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
