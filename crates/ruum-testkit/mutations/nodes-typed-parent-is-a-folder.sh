# A typed node counts as a folder: createNode, moveNode and copyNode put nodes inside a
# database.
# Rule: only the type's own operations make the children of a typed node (decision 0004-18,
# card P-3). Runs the node suites on memory and Postgres. Needs DATABASE_URL.
DESCRIPTION="check_parent lets a typed node be a parent"
FILE="crates/ruum-core/src/nodes/typed.rs"
PERL='s/NodeKind::Typed => Err\(not_a_folder\(\)\),/NodeKind::Typed => Ok(()),/'
TESTS="-p ruum-core --lib nodes::"
PLACEHOLDER=0
