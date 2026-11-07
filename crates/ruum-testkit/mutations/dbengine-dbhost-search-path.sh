# The Postgres engine opens its ruum_dbhost sessions without the fixed search_path, so they search
# the default "$user", public, where a tenant may create operators and functions.
# Security rule: no tenant code runs with the rights of ruum_dbhost (decision 0004-1; review E-2
# B-1 and its re-review). A tenant operator such as `oid = integer` or `name !~ name` in public
# is then picked by the platform's catalog queries (the hand-over of a copy, the lens rebuild)
# and grants roles of another database.
# Runs the hostile-copy cases of ruum-dbengine against the data cluster postgres-data: it needs
# RUUM_DBHOST_URL (eval "$(scripts/dev.sh env)"; in CI the nightly job starts the cluster).
DESCRIPTION="ruum_dbhost sessions keep the default search_path (\"\$user\", public)"
FILE="crates/ruum-dbengine/src/postgres/mod.rs"
PERL='s/\s*\.options\(\[\("search_path", DBHOST_SEARCH_PATH\)\]\)//'
TESTS="-p ruum-dbengine --lib hostile_copy"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
