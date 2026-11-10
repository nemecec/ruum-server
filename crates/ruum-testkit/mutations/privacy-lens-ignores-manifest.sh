# The lens takes the classes of the column store only, not the stricter of the store and the manifest (card Q-2, review-K-1 C-1).
# Security rule: a class that a person took back in the manifest is masked again, also when the store still holds the old confirmation (backlog P-1).
DESCRIPTION="the lens ignores a class that the manifest took back"
FILE="crates/ruum-core/src/privacy/postgres.rs"
PERL='s/Ok\(stricter_spec\(self\.engine, catalog, spec, &self\.manifest\)\)/Ok(spec)/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
