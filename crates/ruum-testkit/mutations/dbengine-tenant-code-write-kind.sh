# The engine line covers masked_write only (review-Q-2-3 N-2, S-21 review 1 H-1) (decision 0004-38, S-21; card Q-2).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="the engine lets the write kind (read_personal agents, editors) run tenant code on live"
FILE="crates/ruum-dbengine/src/postgres/session.rs"
PERL='s/matches!\(kind, RoleKind::MaskedWrite \| RoleKind::Write\)/matches!(kind, RoleKind::MaskedWrite)/'
TESTS="-p ruum-dbengine --lib postgres::tests::tenant_code::masked_write_kinds_refuse_tenant_code_before_and_after_the_statement"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
