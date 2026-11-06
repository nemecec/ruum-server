# The idempotency store keeps share link tokens (`ruum_link_...`) in idempotency_keys.
# Security rule: a secret shown once is never stored in plain text (decision 0001-7).
DESCRIPTION="SECRET_PREFIXES forgets the share link token prefix"
FILE="crates/ruum-core/src/identity/idempotency.rs"
PERL='s/\s*crate::sharing::rules::LINK_TOKEN_PREFIX,//'
TESTS="-p ruum-core --lib identity::idempotency"
