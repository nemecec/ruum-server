# The log ring cuts a long line with `truncate` and counts its length (Kimi review of run 12, K11
# finding 1): the cut line keeps its whole first allocation, so the byte caps of the ring and of
# ended instances bound the counted bytes, not the memory.
DESCRIPTION="the log ring keeps the allocation of a cut line"
FILE="crates/ruum-runtime/src/logs.rs"
PERL='s/text = text\[\.\.cut\]\.to_owned\(\);/text.truncate(cut);/; s/text\.shrink_to_fit\(\);//; s/self\.bytes \+= text\.capacity\(\);/self.bytes += text.len();/; s/self\.bytes -= l\.text\.capacity\(\)/self.bytes -= l.text.len()/'
TESTS="-p ruum-runtime --lib logs::"
PLACEHOLDER=0
