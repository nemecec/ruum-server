# createTemplate takes a source inside an excluded folder (Kimi run 16, L-1 findings 1 and 2;
# S-95): a folder inside an issue folder, `.agents/memory` or a folder inside Sessions or Logs
# would be copied, although the copy rules never copy those trees.
DESCRIPTION="createTemplate takes a source inside an issue, .agents, session, log or memory folder"
FILE="crates/ruum-core/src/templates/create.rs"
PERL='s/\.any\(excluded\)/.any(|_| false)/'
TESTS="-p ruum-core --lib templates::tests::a_source_that_is_or_is_inside_an_excluded_folder_is_refused -- --exact"
PLACEHOLDER=0
