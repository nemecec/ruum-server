# A guest (or an agent of a guest) creates a database while the tenant policy
# guests_create_resources is false (card D-1). Security rule: decision 0004-10 (closes
# 0002-2): resources that count against the host plan need the policy; else 403
# permission_denied with the hint guests_create_resources.
DESCRIPTION="createDatabase ignores guests_create_resources"
FILE="crates/ruum-core/src/databases/mod.rs"
PERL='s/if tenant\.policy\.guests_create_resources \{/if true {/'
TESTS="-p ruum-core --lib databases::tests::guests_need_the_policy_and_agents_need_edit -- --exact"
PLACEHOLDER=0
