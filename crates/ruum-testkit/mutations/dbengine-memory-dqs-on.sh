# The memory engine leaves double-quoted strings (DQS) on (review Q-1, R-3; backlog 2026-11-08
# S-7). Rule: on SQLite a double-quoted word is a name only, never a string.
DESCRIPTION="the memory engine reads a double-quoted word as a string"
FILE="crates/ruum-dbengine/src/memory/conn.rs"
PERL='s/SQLITE_DBCONFIG_DQS_DML, false/SQLITE_DBCONFIG_DQS_DML, true/; s/SQLITE_DBCONFIG_DQS_DDL, false/SQLITE_DBCONFIG_DQS_DDL, true/'
TESTS="-p ruum-testkit --test databases_query_api sqlite_double_quoted_words_are_names_only"
PLACEHOLDER=0
