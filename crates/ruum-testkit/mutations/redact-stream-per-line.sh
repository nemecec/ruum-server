# The streaming redactor checks each line on its own after the replacement, not against the lines
# it let out before (Kimi review of run 11, finding 2): the marker can build a value with a line
# end again across two lines.
DESCRIPTION="the streaming redactor checks each line alone"
FILE="crates/ruum-core/src/runtime/redact.rs"
PERL='s/self\.(joined|written)\.try_feed\(&self\.ac, candidate, (joined_end|written_end)\)/Emitted::default().try_feed(&self.ac, candidate, "")/g'
TESTS="-p ruum-core --lib runtime::"
PLACEHOLDER=0
