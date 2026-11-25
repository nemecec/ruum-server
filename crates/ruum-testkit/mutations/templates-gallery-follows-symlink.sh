# The gallery loader follows a symlink out of a template directory (card L-1: symlinks are
# refused, never followed).
DESCRIPTION="the gallery loader follows symlinks"
FILE="crates/ruum-core/src/templates/source.rs"
PERL='s/let meta = std::fs::symlink_metadata\(e\.path\(\)\)/let meta = std::fs::metadata(e.path())/'
TESTS="-p ruum-core --lib templates::tests::hostile_gallery_directories_are_refused -- --exact"
PLACEHOLDER=0
