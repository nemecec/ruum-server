# The invitation preview sends the invitation's email in clear.
# Security rule: `previewInvitation` needs no sign-in, so it shows the email only masked
# (`k***@e***.ee`), never in clear (decision 0003-6, C-9).
DESCRIPTION="previewInvitation does not mask the email (Postgres store)"
FILE="crates/ruum-core/src/tenants/postgres.rs"
PERL='s/email_masked: rules::mask_email\(&inv\.email\),/email_masked: inv.email.clone(),/'
TESTS="-p ruum-core --lib tenants::tests::postgres_impl::previewing_an_invitation_shows_what_it_is_for -- --exact"
PLACEHOLDER=0
