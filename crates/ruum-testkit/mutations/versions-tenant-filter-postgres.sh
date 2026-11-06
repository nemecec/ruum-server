# The Postgres content store forgets the tenant filter of its version queries and of the file
# lookup that every version query starts with.
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# The statements on the table versions change ("from versions where tenant_id = $1 and",
# "update versions set ... where tenant_id = $1 and"), and node_row ("... owner_id from nodes
# where tenant_id = $1 and id = $2"). The version statements alone are an equivalent mutation:
# each runs after node_row found the file in the caller's tenant and binds its node_id, and the
# foreign key (tenant_id, node_id) ties every version of that file to the same tenant, so no
# request can tell them apart.
# Runs on the Postgres backend with row-level security off (see nodes-tenant-filter.sh). Needs
# DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres version queries and their file lookup drop 'tenant_id = \$n and'"
FILE="crates/ruum-core/src/blobs/postgres.rs"
PERL='s/(\bversions\s+(?:set\s+[^"]*?\s+)?where\s+)tenant_id\s*=\s*\$\d+\s+and\s+/$1/gi; s/(owner_id from nodes where )tenant_id = \$1 and /$1/'
TESTS="-p ruum-testkit --test isolation"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_SUITE_RLS=off"
PLACEHOLDER=0
