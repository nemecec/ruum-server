# The error of a failed migration is not sanitized: the engine's text (with DETAIL and the row
# value) is stored and shown to agents (card D-2). Security rule: decision 0004-8, agents and
# people without manage read the SQLSTATE and the fixed text only. Needs the data cluster
# (RUUM_DBHOST_URL, as on the build server).
DESCRIPTION="migration error not sanitized for agents"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/engine_problem\(copy, false\)/engine_problem(copy, true)/; s/(\*inner,\s*)run\.engine_text,/${1}true,/'
TESTS="-p ruum-core --lib databases::migrations::tests::migrations_on_the_postgres_engine -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
