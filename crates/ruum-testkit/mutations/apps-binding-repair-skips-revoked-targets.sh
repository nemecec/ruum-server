# The binding repair round never visits a target whose rows are all revoked, so a tuple left
# there (a crash, a stuck outbox, a manual write) keeps its access for good (Kimi re-check of
# the run 13 B-1 fixes). Security rule: no binding tuple stays without an active row.
DESCRIPTION="the binding repair round skips targets whose rows are all revoked"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/\.repair_revoked_targets\(\*t, cursor, REPAIR_REVOKED_TARGETS_PER_TENANT\)/.repair_revoked_targets(*t, cursor, 0)/'
TESTS="-p ruum-core --lib apps::bindings::tests::the_repair_round_deletes_a_tuple_on_a_target_whose_rows_are_all_revoked -- --exact"
PLACEHOLDER=0
