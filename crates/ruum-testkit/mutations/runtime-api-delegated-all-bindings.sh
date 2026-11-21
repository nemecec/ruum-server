# The delegated token names every binding, not only those with on_behalf_of_user (card R-3).
# Security rule: delegated-token.schema.json `bindings`: only the bindings that act for the user.
DESCRIPTION="the delegated token names every node binding"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/\.filter\(\|r\| r\.kind == BindingKind::Node && r\.on_behalf_of_user\)/.filter(|r| r.kind == BindingKind::Node)/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
