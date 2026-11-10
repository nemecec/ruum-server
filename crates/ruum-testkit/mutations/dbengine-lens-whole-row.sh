# A whole-row reference in a tenant view does not count as reading a secret (card Q-2, review-Q-2-1 F-3).
# Security rule: people without manage and agents never read a secret, also not through to_json(t) in a view (design 8.5).
DESCRIPTION="a view with a whole-row reference shows every secret of the row to viewers"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/\|\| \(f\.whole_row && names_secret_relation\(\)\)/|| (false \&\& f.whole_row \&\& names_secret_relation())/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::computed_columns_over_a_secret_never_show_it"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
