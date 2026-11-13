# The unpacker drops its file count limit (card W-2s, backlog TK-1). Security rule: STEP3 card
# W-2: unpack has hard limits (bytes, one entry, ratio, file count); an entry-count bomb is a
# direct 413.
DESCRIPTION="unpack has no file count limit"
FILE="crates/ruum-core/src/apps/archive.rs"
PERL='s/self\.count > u64::from\(self\.limits\.max_files\)/false/; s/u64::from\(n_total\) > u64::from\(limits\.max_files\)/false/'
TESTS="-p ruum-core --lib apps::archive::tests::bombs_and_counts_hit_the_limits -- --exact"
PLACEHOLDER=0
