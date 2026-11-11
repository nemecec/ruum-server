# The lens schemas give USAGE on ruum_mask and SELECT on its key to every role. The statement rules refuse the name ruum_mask, so the test reads the key through dynamic SQL in a string (query_to_xml), which only the engine's rights stop.
# Security rule: The key is not readable from any agent role (decision 0004-4).
# The T-2 cases of this rule (CASES) refuse the statement in the rules before the engine sees it, so
# they cannot see this break. The query tests of card Q-2 (dynamic SQL in a string) cannot see it
# either since `query_to_xml` is revoked from the masked kinds: the engine test that calls the
# function and reads the key by name, as a masked role, does.
CARD="Q-2"
CASES="pseudonym.key_unreadable"
DESCRIPTION="the lens schemas give USAGE on ruum_mask and SELECT on its key to every role"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/revoke all on schema ruum_mask, ruum_masked, ruum_nosecret from public;/revoke all on schema ruum_masked, ruum_nosecret from public; grant usage on schema ruum_mask to public;/; s/(\(LensKey::Keep, None\) => unreachable!\(\"the key was read above\"\),\n\s*\}\n\s*)run\(conn, \"leave mask\", \"reset role\"\.to_string\(\)\)/${1}run(conn, \"leave mask\", \"grant select on ruum_mask.key to public; reset role\".to_string())/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::security::masked_roles_reach_neither_the_base_tables_nor_the_key"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
