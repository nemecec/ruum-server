# An agent writes to a live database without a write_live grant (card Q-2).
# Security rule: an agent writes live data only with a write_live grant from its user (design 8.5, contract queryDatabase).
DESCRIPTION="an agent write runs without a write_live grant"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/let live = if draft \{/let live = if true {/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
