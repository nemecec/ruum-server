# Step 2 of a masked copy gives the copy's owner USAGE on ruum_mask again (backlog 2026-11-09
# S-9, review-masking-1 F2). Security rule: tenant code never holds a right on the schema of
# the key; the owner reaches the pseudonym functions only through the entry points.
DESCRIPTION="the copy's owner holds USAGE on ruum_mask while tenant code of step 2 runs"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/grant usage on schema \{COPY_SCHEMA\} to \{owner_id\};/grant usage on schema {COPY_SCHEMA}, ruum_mask to {owner_id};/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::tenant_types::step_2_of_a_masked_copy_has_no_right_on_ruum_mask"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
