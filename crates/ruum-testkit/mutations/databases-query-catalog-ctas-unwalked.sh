# The catalog rule of the query API does not walk the query of CREATE TABLE ... AS (backlog
# 2026-11-09 S-16). Security rule: a data-modifying WITH inside the query of a schema change
# that writes a catalog relation is `422 statement_not_allowed` in the rules.
DESCRIPTION="the statement rules skip the query of CREATE TABLE ... AS"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/if let Some\(q\) = &c\.query \{\n(\s+)self\.query\(q\);/if let Some(q) = &c.query {\n\1let _ = q;/'
TESTS="-p ruum-core --lib databases::query::rules"
PLACEHOLDER=0
