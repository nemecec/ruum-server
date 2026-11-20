# The binding repair ends the live rows it found unplanned without reading the live manifest
# again under the live switch, so a row that a rollback kept meanwhile ends and the live
# version loses its grant (Kimi re-check of the run 13 B-1 fixes, finding 2). Security rule
# (availability of granted access): the repair ends only rows the live manifest does not plan.
DESCRIPTION="the binding repair ends stale live rows without a second look under the live switch"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/let still = self\.unplanned_live_rows\(tenant, &current\)\.await\?;/let still = current.clone();/'
TESTS="-p ruum-core --lib apps::bindings::tests::the_repair_keeps_a_stale_row_that_a_rollback_adopted_meanwhile -- --exact"
PLACEHOLDER=0
