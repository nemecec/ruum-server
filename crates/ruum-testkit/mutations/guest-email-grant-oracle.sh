# While guests are off, a guest's email grant to a member's email succeeds and one to any other
# email fails: the answer tells the guest which emails belong to the tenant.
# Security rule: while `allow_guests` is false, a guest (or an agent of a guest) gets the same
# `403 guests_refused` for every email grant (decision 0003-2, reviewer 1 #1).
DESCRIPTION="the guest email grant oracle while guests are off (Postgres)"
FILE="crates/ruum-core/src/sharing/postgres.rs"
PERL='s/(tell the email of a member from any other\.\n)(\s*)if !policy\.allow_guests \{/$1$2if false {/'
TESTS="-p ruum-core --lib sharing::conformance::postgres_email_grants_conform -- --exact"
PLACEHOLDER=0
