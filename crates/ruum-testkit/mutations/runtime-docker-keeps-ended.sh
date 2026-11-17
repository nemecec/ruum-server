# The Docker driver never forgets an ended instance (backlog R2A-5, Kimi review of run 11,
# finding 3): every exited job and stopped service keeps its log ring, so memory grows without
# bound.
DESCRIPTION="the Docker driver keeps every ended instance"
FILE="crates/ruum-runtime/src/driver.rs"
PERL='s/(        for id in evict \{\n            map\.remove\(&id\);\n        \})/        let _ = &evict;/'
TESTS="-p ruum-runtime --lib"
PLACEHOLDER=0
