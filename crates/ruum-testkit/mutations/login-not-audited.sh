# The sign-in writes its auth.login row into another tenant, not the user's home tenant.
# Security rule: every sign-in is in the audit log of the user's home tenant, synchronous and
# fail-closed (decision 0003-4, contract need C-14; design 15).
DESCRIPTION="the sign-in is not audited in the home tenant"
FILE="crates/ruum-api/src/routes/auth.rs"
PERL='s/(events::sign_in\(\s*state\.services\.audit\.as_ref\(\),\s*)user\.home_tenant_id,/${1}ruum_core::TenantId::new(),/'
TESTS="-p ruum-api --lib routes::auth::tests::sign_ins_are_audited_in_the_home_tenant_and_at_first_use -- --exact"
PLACEHOLDER=0
