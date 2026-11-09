# A lens build goes on while an object outside the lens schemas depends on an object inside
# them (for example a view that tenant code in step 2 of a masked copy made on ruum_mask.key).
# The rebuild then drops the tenant object silently, or the object keeps a reference into
# ruum_mask.
# Security rule: decision 0004-37 (d) and review-masking-1 F2: such a copy is refused.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="a lens build ignores objects that depend on the lens schemas"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/if !foreign\.is_empty\(\) \{/if false && !foreign.is_empty() {/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
