# The names middleware takes a name from any tenant that knows the ID, not from the tenant of
# the object that names the principal.
# Security rule: a name is looked up and filled only in the tenant of the enclosing object (or
# of the request), so one tenant's names never show in another tenant's answer (decision 0003-5).
DESCRIPTION="the names middleware ignores the tenant of a name"
FILE="crates/ruum-api/src/names.rs"
PERL='s/principal_ref\(obj\)\.and_then\(\|id\| names\.get\(&\(t, id\)\)\)/principal_ref(obj).and_then(|id| names.iter().find(|((_, i), _)| *i == id).map(|(_, n)| n))/'
TESTS="-p ruum-api --lib names::tests"
PLACEHOLDER=0
