# The redactor checks a line before the store cuts it (Kimi review of run 12, K11 finding 2): the
# kept start of a cut line joins the next line and rebuilds a multiline value.
DESCRIPTION="the streaming redactor checks lines before the line cap"
FILE="crates/ruum-core/src/runtime/redact.rs"
PERL='s/let max = self\.max_out;/let max = usize::MAX;/'
TESTS="-p ruum-core --lib runtime::"
PLACEHOLDER=0
