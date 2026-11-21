# The Postgres suggestion overwrites a stricter one (review O-2, 2). Security rule: the archive
# only suggests and a suggestion never goes down (see telemetry-classes-suggestion-goes-down).
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="the Postgres suggestion overwrites a stricter stored one"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/telemetry_fields\.suggested_class is null or \{stricter\}/true/g'
TESTS="-p ruum-core --lib telemetry::classes::tests::postgres_suggestions_go_up_and_only_a_person_relaxes -- --exact"
PLACEHOLDER=0
