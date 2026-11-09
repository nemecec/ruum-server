# The names middleware walks into the rows of a query result (backlog S-3).
# Security rule: data of a database is never read nor changed by the platform on the way out; a
# caller who builds {"kind": "user", "id": ...} in a query must not learn the name of a user (or
# of another tenant's user, with a tenant_id in the row).
DESCRIPTION="the names middleware fills names into the rows of an untrusted query result"
FILE="crates/ruum-api/src/names.rs"
PERL='s/key == "rows" && obj\.get\("content_trust"\)/false \&\& obj.get("content_trust")/'
TESTS="-p ruum-api --lib names"
PLACEHOLDER=0
