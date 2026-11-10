# The engine does not check again after the statement (the race of S-21 review 1 M-2) (decision 0004-38, S-21; card Q-2).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="a trigger made between the engine check and the statement fires and commits"
FILE="crates/ruum-dbengine/src/postgres/session.rs"
PERL='s/Ok\(outcome\) if guarded && tx == TransactionState::Open =>/Ok(outcome) if false \&\& guarded \&\& tx == TransactionState::Open =>/'
TESTS="-p ruum-dbengine --lib postgres::tests::tenant_code::masked_write_kinds_refuse_tenant_code_before_and_after_the_statement"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
