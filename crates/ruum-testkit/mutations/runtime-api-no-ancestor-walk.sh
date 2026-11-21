# Bound content without the walk from the node up to a binding's target (card R-3): any node
# the SpiceDB check lets through, also one with a tuple but no binding row. Security rule:
# bound content only; the row and the tuple are both needed (STEP3 "How an app runs" rule 5).
DESCRIPTION="a read skips the binding rows and the path to the bound target"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/\.any\(\|r\| r\.target_node_id == q\.node_id \|\| above\.contains\(&r\.target_node_id\)\)/.any(|_| true)/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
