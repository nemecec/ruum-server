# The CSRF check compares the Ruum-Csrf header only with the ruum_csrf cookie again (plain
# double submit).
# Security rule: the CSRF value is bound to the session, so a cookie set by a sibling host
# (cookie tossing, for example from dav.<domain>) cannot pass the check (decision 0002-3).
DESCRIPTION="the CSRF check accepts any header that equals the ruum_csrf cookie"
FILE="crates/ruum-api/src/auth.rs"
PERL='s/let expected = ruum_core::identity::csrf_token\(&secret\);/let expected = cookie(headers, ruum_core::identity::CSRF_COOKIE).unwrap_or_default();/'
TESTS="-p ruum-api --lib routes::auth::tests::the_csrf_value_is_bound_to_the_session -- --exact"
PLACEHOLDER=0
