# The unpacker lets a `..` segment through (card W-2s). Security rule: decision 0006 H-2, STEP3
# card W-2: an archive entry never leads out of the archive (zip slip, tar slip).
DESCRIPTION="unpack accepts an entry with a .. segment"
FILE="crates/ruum-core/src/apps/archive.rs"
PERL='s/if seg == "\.\." \{/if seg == "..." {/'
TESTS="-p ruum-core --lib apps::archive::tests::hostile_names_are_refused -- --exact"
PLACEHOLDER=0
