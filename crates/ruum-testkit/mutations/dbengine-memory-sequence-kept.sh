# A masked copy of the memory engine keeps the source's AUTOINCREMENT counters (Kimi review of
# D-3b on main, finding 4). Security rule: a counter of a masked copy tells no row ID of the
# source that the copy does not hold.
DESCRIPTION="a masked memory copy keeps the source's AUTOINCREMENT counters"
FILE="crates/ruum-dbengine/src/memory/mask.rs"
PERL='s/reset_sequences\(conn\)\?;/let _ = reset_sequences;/'
TESTS="-p ruum-dbengine --lib -- memory::tests::a_masked_copy_keeps_no_autoincrement_counter_of_the_source"
ENV=""
PLACEHOLDER=0
