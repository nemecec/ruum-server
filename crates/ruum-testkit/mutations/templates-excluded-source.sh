# createTemplate takes an excluded folder itself as the source (Kimi run 16, L-1 findings 1 and
# 2; S-95): an issue folder as the source would copy its report attachments, `.agents` its agent
# memory, although the copy rules never copy them below a source.
DESCRIPTION="createTemplate takes an issue, .agents, session, log or memory folder as the source"
FILE="crates/ruum-core/src/templates/create.rs"
PERL='s/if excluded\(&node\)\n/if false\n/'
TESTS="-p ruum-core --lib templates::tests::a_source_that_is_or_is_inside_an_excluded_folder_is_refused -- --exact"
PLACEHOLDER=0
