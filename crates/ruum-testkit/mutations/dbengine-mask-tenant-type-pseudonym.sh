# The masked lens view gives a personal column of a tenant-defined type (enum, domain,
# composite, range, array of one) a pseudonym through ruum_mask.pseudonym_other, as before the
# fix. The tenant's cast of its type then runs as mask inside the SECURITY DEFINER function.
# Security rule: decision 0005 B-2 and 0004-37 (review-dbhost-2 item 2): such a column is masked
# like secret, and no tenant conversion runs in a definer context.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="the masked lens pseudonymizes a personal column of a tenant-defined type"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/None => masked\.push\(format!\("\{\} as \{name\}", hidden_expr\(&name\)\)\)/None => masked.push(format!("{} as {name}", pseudonym_call(&name, 0)))/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
