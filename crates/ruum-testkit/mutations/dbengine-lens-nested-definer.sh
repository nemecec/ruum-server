# The lens does not follow a function to the functions it calls (card Q-2, review-Q-2-1 F-4).
# Security rule: people without manage and agents never read a secret, also not through a SECURITY DEFINER function behind another function (design 8.5).
DESCRIPTION="a view that calls a definer function through a plain function shows the secret to viewers"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL="s/and reach.classid operator\\(pg_catalog.<>\\) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid/and reach.classid operator(pg_catalog.<>) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid and reach.classid operator(pg_catalog.<>) 'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid/"
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::computed_columns_over_a_secret_never_show_it"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
