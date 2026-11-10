# A lens rebuild that meets a running change gives up at once (card Q-2, review-Q-2-1 F-7).
# Security rule: every change of classes reaches the lens (design 8.5, card K-1).
DESCRIPTION="a lens rebuild during a migration or a copy drain is lost"
FILE="crates/ruum-core/src/privacy/postgres.rs"
PERL='s/if !matches!\(last, Err\(EngineError::NotReady\)\) \{/if true {/'
TESTS="-p ruum-core --lib privacy::postgres"
PLACEHOLDER=0
