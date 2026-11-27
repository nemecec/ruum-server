# The lexing options are not pinned again before each statement of a migration (T-97, review of
# D-2 landing in run 17, finding A). Security rule: the server lexes every untrusted statement as
# the engine's splitter and the statement rules did (standard strings, UTF-8); a DO block of one
# statement cannot change how the next one is read.
DESCRIPTION="standard_conforming_strings and client_encoding are not set again before each statement of up"
FILE="crates/ruum-dbengine/src/postgres/session.rs"
PERL='s/set local lock_timeout = \{ms\}; \{LEXING\}/set local lock_timeout = {ms}/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::security::a_migration_cannot_change_how_the_next_statement_is_lexed"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
