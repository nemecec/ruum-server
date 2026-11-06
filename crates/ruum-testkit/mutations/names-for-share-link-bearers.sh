# The names middleware fills names for the bearer of a share link.
# Security rule: share-link bearers are anonymous visitors; answers to them carry no names of
# the tenant's people (decision 0003-5).
DESCRIPTION="share-link bearers get names"
FILE="crates/ruum-api/src/names.rs"
PERL='s/if ctx\.actor\.actor\.kind != PrincipalKind::ShareLink \{/if true {/'
TESTS="-p ruum-testkit --test names"
PLACEHOLDER=0
