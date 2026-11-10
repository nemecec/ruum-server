# The query layer leaves the lens constant of a secret value in the result instead of the hidden-value marker (card Q-2).
# Contract rule: a secret value reads as the hidden-value marker {"$ruum": "hidden", "class": "secret"}, never a look-alike string (decision 0004-37 a).
DESCRIPTION="a secret value comes back as the lens constant, not the hidden-value marker"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/(let m = hidden\(DataClass::Secret\);\n(\s*)for r in rows\.iter_mut\(\) \{)/${1}\n${2}    break;/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
