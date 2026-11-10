# The lens builds the normal view for a relation that it should refuse (review-Q-2-s21-2 R-1).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="the refusal view is not built: the masked lens reads the view that reaches code outside the allow-list"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/let sql = if refused_rel \{/let sql = if false \&\& refused_rel {/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
