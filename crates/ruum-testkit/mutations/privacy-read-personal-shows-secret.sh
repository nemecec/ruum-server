# An agent with read_personal reads as secret_read (the tables), so secrets show.
# Security rule: A read_personal grant never shows a secret to an agent (design 8.5).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="query.read_personal_grant"
DESCRIPTION="an agent with read_personal reads as secret_read (the tables), so secrets show"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/kind: RoleKind::Read,\n(\s*)grant_id: Some\(g\.id\),/kind: RoleKind::SecretRead,\n${1}grant_id: Some(g.id),/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=query.read_personal_grant"
PLACEHOLDER=0
