# The Postgres engine does not check who may run pg_catalog.set_config in a database (review Q-1,
# S-1). Security rule: set_config of any option never runs for a read or masked kind (decision
# 0004-35 a), also not through query_to_xml or ts_stat, whose query text no statement rule reads.
# The cluster set-up revokes it from PUBLIC; the engine must refuse a database (made before that
# set-up, or a copy of one) where PUBLIC or another kind than owner may still run it.
# Runs against the data cluster postgres-data: it needs RUUM_DBHOST_URL.
DESCRIPTION="the engine serves a database where PUBLIC may run set_config"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/(\) -> Result<\(\), EngineError> \{\n)(    let others: Vec<String> = roles)/$1    if true { return Ok(()); }\n$2/'
TESTS="-p ruum-dbengine --lib set_config"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
