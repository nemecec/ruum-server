# An issue template's schema may refer to another document (card L-1: no remote $ref; the
# server never fetches a schema).
DESCRIPTION="the issue schema check accepts a remote \$ref"
FILE="crates/ruum-core/src/templates/manifest.rs"
PERL='s/if let Some\(at\) = remote_reference\(schema, &sp\) \{/if let Some(at) = None::<String> {/'
TESTS="-p ruum-core --lib templates::tests::issue_templates_check_their_schema -- --exact"
PLACEHOLDER=0
