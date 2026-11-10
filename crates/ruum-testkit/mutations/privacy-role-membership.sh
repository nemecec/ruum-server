# The masked_read kind is made a member of the owner kind with INHERIT, so it reads the base tables (through dynamic SQL in a string, which the statement rules do not read).
# Security rule: Untrusted SQL never reaches another role, also not with SET ROLE, set_config or a DO block (decision 0004-1). E-2's tests guard the engine; this mutation shows that the privacy role cases see the break through the API.
# The T-2 cases of this rule (CASES) refuse the statement in the rules before the engine sees it, so
# they cannot see this break; the hostile engine tests of card Q-2 do (dynamic SQL in a string).
CARD="Q-2"
CASES="role."
DESCRIPTION="the masked_read kind is made a member of the owner kind with INHERIT, so it reads the base tables (through dynamic SQL in a string, which the statement rules do not read)"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/(    let mut admin = engine\.admin\(\)\.await\?;\n    run\(&mut admin, \"create the roles\", sql\)\.await)/    sql.push_str(&format!(\"grant {} to {} with inherit true;\", ident(role_of(roles, RoleKind::Owner)), ident(role_of(roles, RoleKind::MaskedRead))));\n${1}/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
