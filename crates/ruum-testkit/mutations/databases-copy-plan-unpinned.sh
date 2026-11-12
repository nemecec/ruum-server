# The platform's mask plan does not name the catalog it was made from (review FU-1-3 of Kimi),
# so the engine cannot refuse a copy whose source got a column in between. Rule: a column that
# a migration added after the plan was made is never copied in clear into a masked copy.
DESCRIPTION="the mask plan carries no catalog, so the engine skips the check"
FILE="crates/ruum-core/src/databases/provider.rs"
PERL='s/planned_from: Some\(planned_from\),/planned_from: { let _ = planned_from; None },/'
TESTS="-p ruum-core --lib databases::provider::tests::a_mask_plan_follows_the_classes"
PLACEHOLDER=0
