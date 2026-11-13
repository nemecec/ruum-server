# An email that resolves to a guest skips the guest policy (card B-2, review of B-2). Security
# rule: while the tenant does not allow guests, a guest gets no new access, whether the grantee
# names the guest's user ID or the guest's verified email (as addGrant).
DESCRIPTION="an email grantee that is a guest skips allow_guests"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/Some\(id\) => Grantee::User \{ id \},/Some(id) => return Ok(Grantee::User { id }),/'
TESTS="-p ruum-core --lib apps::roles::tests::an_email_of_a_guest_is_refused_while_guests_are_off -- --exact"
PLACEHOLDER=0
