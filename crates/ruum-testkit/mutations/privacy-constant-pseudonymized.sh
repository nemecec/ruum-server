# A result column that reads no table column gets the class personal and is masked.
# Security rule: Constants and parameters are normal and never pseudonymized, so SELECT 'guess' gives no pseudonym of a guess (decision 0004-4).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="pseudonym.constant"
DESCRIPTION="a result column that reads no table column gets the class personal and is masked"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/(\.map\(\|r\| self\.class_of\(r, tables\)\)\n\s*)\.fold\(DataClass::Normal, stricter\)/${1}.fold(DataClass::Personal, stricter)/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=pseudonym.constant"
PLACEHOLDER=0
