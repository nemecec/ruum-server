# The tar reader takes link entries as files (card W-2s). Security rule: STEP3 card W-2, decision
# 0006 H-2: symbolic and hard links in an archive are refused, never followed or stored.
DESCRIPTION="unpack takes symbolic and hard links as files"
FILE="crates/ruum-core/src/apps/archive.rs"
PERL="s/b'0' \\| 0 \\| b'7' => \\{/b'0' | 0 | b'7' | b'1' | b'2' => {/"
TESTS="-p ruum-core --lib apps::archive::tests::links_devices_and_fifos_are_refused -- --exact"
PLACEHOLDER=0
