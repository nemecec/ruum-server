# A destructive statement of an agent (DELETE or UPDATE without WHERE, TRUNCATE) runs at once (card Q-2).
# Security rule: destructive statements of an agent never run without a person's approval (contract queryDatabase, decision 0004-5).
DESCRIPTION="a destructive statement of an agent runs at once"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/if is_agent && footprint\.destructive\(\) \{/if false {/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
