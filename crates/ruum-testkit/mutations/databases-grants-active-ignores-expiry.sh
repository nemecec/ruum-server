# The grant check counts a grant after its end time (card A-1). Security rule: an expired grant
# gives nothing, also before the sweeper deletes its tuple (design 8.5; the grant tuples in
# contracts/authz/schema.zed: "a tuple that the sweeper has not deleted yet gives nothing").
DESCRIPTION="grants::active ignores expires_at"
FILE="crates/ruum-core/src/databases/access/grants.rs"
PERL='s/self\.starts_at <= at && at < self\.expires_at/self.starts_at <= at/'
TESTS="-p ruum-core --lib databases::access::grants::tests::expired_or_revoked_grants_give_nothing_before_the_sweeper -- --exact"
PLACEHOLDER=0
