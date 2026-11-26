# The log ring cuts a long line with `truncate` and counts its length (Kimi review of run 12, K11
# finding 1): the cut line keeps its whole first allocation, so the byte caps of the ring and of
# ended instances bound the counted bytes, not the memory. Since I6-20 the ring counts through
# `let n = ...capacity()` (the live log budget); the old `self.bytes` patterns matched nothing.
DESCRIPTION="the log ring keeps the allocation of a cut line"
FILE="crates/ruum-runtime/src/logs.rs"
PERL='s/text = text\[\.\.cut\]\.to_owned\(\);/text.truncate(cut);/; s/text\.shrink_to_fit\(\);//; s/let n = text\.capacity\(\);/let n = text.len();/; s/let n = l\.text\.capacity\(\);/let n = l.text.len();/'
TESTS="-p ruum-runtime --lib logs::"
PLACEHOLDER=0
