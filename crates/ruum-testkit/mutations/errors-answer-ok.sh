# Every error answer gets status 200, so a refusal looks like success.
# Security rule: a denied or unknown request never answers 2xx.
DESCRIPTION="error answers use status 200 instead of their HTTP status"
FILE="crates/ruum-api/src/error.rs"
PERL='s/let status =\s*StatusCode::from_u16\(e\.code\.http_status\(\)\)\s*\.unwrap_or\(StatusCode::INTERNAL_SERVER_ERROR\);/let status = StatusCode::OK;/s'
TESTS="-p ruum-testkit --test isolation --test permissions"
PLACEHOLDER=0
