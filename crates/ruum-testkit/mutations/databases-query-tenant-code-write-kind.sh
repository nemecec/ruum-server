# The query layer covers masked_write only (S-21 review 1 H-1, review 2 R-1, S-23) (decision 0004-38, S-21; card Q-2).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="the query layer lets read_personal agents and editors write where tenant code runs"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/let masked_writer = matches!\(kind, RoleKind::MaskedWrite \| RoleKind::Write\);/let masked_writer = matches!(kind, RoleKind::MaskedWrite);/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::masked_sessions_never_run_tenant_code_on_live"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
