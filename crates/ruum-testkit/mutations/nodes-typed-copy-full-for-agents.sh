# copy_data gives full data to an agent whose user has manage on the source: the agent reads
# the source's secrets and personal values in the copy.
# Rule: an agent's copy of a database is always masked; a copy holds full data only for a
# person with manage on the source (decision 0004-7, card P-3, review-wave5 B-1). Runs the node
# suites on memory and Postgres. Needs DATABASE_URL. The privacy suite's own case for the same
# rule (T-2, copy.agent) stays in mutations/planned/privacy-copy-full-for-agent.sh.
DESCRIPTION="copy_data gives full data to an agent"
FILE="crates/ruum-core/src/nodes/typed.rs"
PERL='s/return Ok\(DataCopy::Masked\);/return Ok(DataCopy::Full);/'
TESTS="-p ruum-core --lib nodes::"
PLACEHOLDER=0
