# Step 3 of a masked copy goes on while an object outside the lens schemas depends on an object
# inside them (for example a view that tenant code in step 2 made on an entry point of
# ruum_copy). Dropping ruum_copy then drops the tenant object silently, or the object keeps a
# reference into the platform's schemas.
# Security rule: decision 0004-37 (d) and review-masking-1 F2: such a copy is refused.
# Since card FU-1 the copy refuses it in step 3 (copy.rs, drop_copy_entry_points), before the
# lens build of setup.rs, whose own check is now a second line (backlog 2026-11-17 T-57).
# Since D-3b no tenant code of the copy runs in step 2; the test probe of step 2
# (copy::step_2_probe, backlog 2026-11-26 T-90) makes such a view in
# tenant_types::step_3_refuses_an_object_that_step_2_made_over_ruum_copy.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="a masked copy ignores objects that depend on the lens schemas"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/if !foreign\.is_empty\(\) \{/if false && !foreign.is_empty() {/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
