# Functional notation email(p) is read as a function of the whole row (card Q-2, review-Q-2-2 P-3).
# Security rule: the class of a result column is the class of the columns it reads (design 8.5).
DESCRIPTION="email(p) is labelled like the whole row instead of the column"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/&& self\.is_relation\(&arg, tables\)/\&\& self.is_relation(\&arg, tables) \&\& false/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::tests"
PLACEHOLDER=0
