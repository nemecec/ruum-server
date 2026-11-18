# The names middleware walks into the rows of a query result (backlog S-3).
# Security rule: data of a database is never read nor changed by the platform on the way out; a
# caller who builds {"kind": "user", "id": ...} in a query must not learn the name of a user (or
# of another tenant's user, with a tenant_id in the row).
# Since card FU-1 the middleware names only the positions of principal_paths.json (made from the
# contract; fields marked x-ruum-content-trust: untrusted are never entered). The mutation adds
# the cells of the query rows to the positions of queryDatabase.
DESCRIPTION="the names middleware fills names into the rows of an untrusted query result"
FILE="crates/ruum-api/src/principal_paths.json"
PERL='s/("operation": "queryDatabase",\s*"refs": \[)/$1\n    ["rows", "*", "*"],/'
TESTS="-p ruum-api --lib names"
PLACEHOLDER=0
