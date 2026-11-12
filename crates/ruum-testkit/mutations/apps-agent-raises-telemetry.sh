# An agent raises the telemetry mode of an app to full (card W-1). Security rule: design 9.6,
# review R9, docs/STEP3.md permission table: only a person raises telemetry_mode to full or
# raw_copy_days; an agent gets 403 user_required (it may lower them).
DESCRIPTION="an agent may raise telemetry_mode"
FILE="crates/ruum-core/src/apps/settings.rs"
PERL='s/if next\.telemetry_mode > current\.telemetry_mode \{/if false {/'
TESTS="-p ruum-core --lib apps:: -- an_agent_never_sets_access_nor_raises_telemetry an_agent_cannot_raise_telemetry_but_may_lower_it"
PLACEHOLDER=0
