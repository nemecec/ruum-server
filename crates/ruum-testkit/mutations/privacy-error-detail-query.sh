# The query layer gives the engine's text to every caller.
# Security rule: Agents and people without manage get the SQLSTATE, a fixed text and object names only (decision 0004-8).
#
# The query layer's line of the rule (see privacy-error-detail.sh for the engine's line); the
# unit test `engine_errors_are_sanitized_without_manage` holds it.
CARD="Q-2"
CASES="leak.unique_violation,leak.check_violation"
DESCRIPTION="agents and people without manage read the engine's text of an error"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/let message = if manage \{/let message = if true {/'
TESTS="-p ruum-core --lib databases::query::tests::engine_errors_are_sanitized_without_manage"
PLACEHOLDER=0
