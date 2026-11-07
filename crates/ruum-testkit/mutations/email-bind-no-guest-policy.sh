# The bind makes a guest also in a tenant that does not allow guests.
# Security rule: a bind makes the person a guest only while `allow_guests` is true; otherwise
# the grant stays an email grant (decision 0003-2 (c)).
DESCRIPTION="a bind makes a guest while guests are off"
FILE="crates/ruum-core/src/tenants/postgres.rs"
PERL='s/if old\.is_none\(\) && !tenant\.policy\.allow_guests \{/if false {/'
TESTS="-p ruum-core --lib tenants::tests::email_grants::unverified_emails_and_closed_tenants_bind_nothing -- --exact"
PLACEHOLDER=0
