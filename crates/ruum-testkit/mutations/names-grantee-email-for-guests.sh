# The grant list shows the emails of user grantees to a guest who may share the node.
# Security rule: only members, admins and owners (who may list the members' emails anyway) see
# grantee emails; for guests the grant list is no lookup from user to email (decision 0003-5,
# the reasoning of 0002-5 row 36).
DESCRIPTION="guests see grantee emails"
FILE="crates/ruum-api/src/routes/sharing.rs"
PERL='s/if role\.is_none_or\(\|r\| r < MemberRole::Member\) \{/if role.is_none() {/'
TESTS="-p ruum-testkit --test names"
PLACEHOLDER=0
