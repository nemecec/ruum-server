# The host lookup function runs without the system flag (card G-1a). Security rule:
# docs/STEP3.md "System lookups": the lookup is a narrow SECURITY DEFINER function that sets the
# system flag for its own body only; without it the gateway (or anything that skips the flag)
# would need general system access. The mutated function finds nothing under row-level security.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="ruum_app_host_lookup runs without the system flag"
FILE="migrations/0013_app_host_lookup.sql"
PERL='s/perform set_config\(\x27ruum\.system\x27, \x27on\x27, true\);/perform set_config(\x27ruum.system\x27, \x27off\x27, true);/'
TESTS="-p ruum-gateway --test host_lookup_pg"
PLACEHOLDER=0
