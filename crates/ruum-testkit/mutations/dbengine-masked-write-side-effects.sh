# The engine checks for tenant code only after the statement (decision 0004-38, S-21; card Q-2).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="the engine starts a masked write in a database with tenant code (only the check after the statement is left)"
FILE="crates/ruum-dbengine/src/postgres/session.rs"
PERL='s/if guarded && let Err\(e\) = refuse_tenant_code/if false \&\& guarded \&\& let Err(e) = refuse_tenant_code/'
TESTS="-p ruum-dbengine --lib postgres::tests::tenant_code::masked_write_kinds_refuse_tenant_code_before_and_after_the_statement"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
