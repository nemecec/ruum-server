# Step 2 of a masked copy gives the copy's owner USAGE on ruum_mask again (backlog 2026-11-09
# S-9, review-masking-1 F2). Security rule: tenant code never holds a right on the schema of
# the key; the owner reaches the pseudonym functions only through the entry points.
# Since D-3b no tenant code of the copy runs in step 2, so the test probe of step 2
# (copy::step_2_probe, backlog 2026-11-26 T-90) checks the right where tenant code would run.
DESCRIPTION="the copy's owner holds USAGE on ruum_mask while tenant code of step 2 runs"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/grant usage on schema \{COPY_SCHEMA\} to \{owner_id\};/grant usage on schema {COPY_SCHEMA}, ruum_mask to {owner_id};/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::tenant_types::the_owner_holds_no_right_on_ruum_mask_between_step_2_and_step_3"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
