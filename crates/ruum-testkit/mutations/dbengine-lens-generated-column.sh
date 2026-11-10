# A generated column computed from a secret column is masked like any personal column (card Q-2, review-Q-2-1 F-5).
# Security rule: people without manage and agents never read a secret, also not through a generated column (design 8.5).
DESCRIPTION="a generated column over a secret shows it to viewers"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/\|\| secret_reading\.contains\(&\(i\.rel_oid, i\.attnum\)\)/|| false/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::computed_columns_over_a_secret_never_show_it"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
