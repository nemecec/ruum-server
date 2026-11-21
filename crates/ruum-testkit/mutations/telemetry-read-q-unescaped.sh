# The words of searchAppLogs q go raw into the LogQL line filter (card O-2). Security rule: every
# filter is escaped into the store's query language by the server; q is only ever a line filter
# literal, never a regex or a pipeline stage (docs/STEP3.md "Telemetry and privacy").
DESCRIPTION="the words of q go into LogQL without quote()"
FILE="crates/ruum-core/src/telemetry/lgtm.rs"
PERL='s/for w in f\.words\.iter\(\)\.filter\(\|w\| line_safe\(w\)\) \{\n(\s*)q\.push_str\(&format!\(" \|= \{\}", quote\(w\)\)\);/for w in f.words.iter() {\n$1q.push_str(&format!(" |= \\"{w}\\""));/'
TESTS="-p ruum-core --lib telemetry::lgtm::o2_read_tests"
PLACEHOLDER=0
