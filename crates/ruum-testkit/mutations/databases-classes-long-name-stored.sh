# The catalog sync takes every name, also one over 63 characters (review K-1, C-2; backlog
# 2026-11-08 P-2). Rule: a name that database_columns cannot hold gets no row and counts as
# personal; with the mutation the Postgres store's check constraint stops the whole sync.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="fits_store takes every name, so a long SQLite name stops the sync on Postgres"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/\(1\.\.=MAX_NAME_CHARS\)\.contains\(&chars\) && !n\.contains/true || !n.contains/'
TESTS="-p ruum-core --lib databases::classes"
PLACEHOLDER=0
