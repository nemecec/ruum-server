# A role made from a stale read of an active row stays after the row ended (Kimi run 17 B-1d,
# finding 2). Security rule: no role outlives a completed end of its binding.
DESCRIPTION="ensure_role does not read its row again"
FILE="crates/ruum-core/src/apps/bindings/database.rs"
PERL='s/if !still \{/if false \&\& !still {/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_role_made_for_a_row_that_ended_goes_again -- --exact"
PLACEHOLDER=0
