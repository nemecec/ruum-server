# Every role gets USAGE on ruum_mask, so the agent kinds call the pseudonym function by name (through dynamic SQL in a string, which the statement rules do not read).
# Security rule: No role calls the pseudonym function by name, so no one gets the pseudonym of a chosen value (decision 0004-4).
# The T-2 cases of this rule (CASES) refuse the statement in the rules before the engine sees it, so
# they cannot see this break. The query tests of card Q-2 (dynamic SQL in a string) cannot see it
# either since `query_to_xml` is revoked from the masked kinds: the engine test that calls the
# function and reads the key by name, as a masked role, does.
CARD="Q-2"
CASES="pseudonym.direct_call"
DESCRIPTION="every role gets USAGE on ruum_mask, so the agent kinds call the pseudonym function by name (through dynamic SQL in a string, which the statement rules do not read)"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/revoke all on schema ruum_mask, ruum_masked, ruum_nosecret from public;/revoke all on schema ruum_masked, ruum_nosecret from public; grant usage on schema ruum_mask to public;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::security::masked_roles_reach_neither_the_base_tables_nor_the_key"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
