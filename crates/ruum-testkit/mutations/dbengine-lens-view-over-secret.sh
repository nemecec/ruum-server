# The lens shows the personal columns of a tenant view over a secret column like any personal column (card Q-2, review-Q-1 N-9).
# Security rule: people without manage and agents never read a secret, also not through a view the tenant made (design 8.5, decision 0004-3).
DESCRIPTION="a tenant view over a secret column shows it to people without manage"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/DataClass::Personal if reads_secret => DataClass::Secret,/DataClass::Personal if reads_secret \&\& false => DataClass::Secret,/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
