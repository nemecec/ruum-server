# The catalog rule of the query API lets every write through (review Q-1-3, N-B; backlog
# 2026-11-09 S-12). Security rule: a write to a catalog relation (`UPDATE pg_settings`, which
# is set_config through the view's rewrite rule) is `422 statement_not_allowed` in the rules,
# not only refused by the engine.
DESCRIPTION="the statement rules let writes to pg_catalog relations through"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/let hit = t\.names/let hit = false \&\& t.names/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
