# A change of the column classes does not rebuild the lens of a Postgres database (card Q-2).
# Security rule: the lens follows the confirmed classes: a column confirmed as secret is masked at once (design 8.5, card Q-2).
DESCRIPTION="a confirmation does not rebuild the Postgres lens"
FILE="crates/ruum-core/src/privacy/postgres.rs"
PERL='s/rebuild_lens\(&svc, tenant, node\)\.await\n/let _ = (svc, tenant, node);\n        Ok(())\n/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
