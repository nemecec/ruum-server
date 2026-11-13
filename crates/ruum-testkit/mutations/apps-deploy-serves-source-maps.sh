# The static site serves the files of telemetry.source_maps (card W-2s, review Kimi 3). Security
# rule: contracts/manifests/app.schema.json `telemetry`, source maps are kept private and never
# served (they can hold the original source).
DESCRIPTION="the site keeps the files of telemetry.source_maps"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/\.filter\(\|e\| !maps\.as_ref\(\)\.is_some_and\(\|g\| g\.matches\(&e\.name\)\)\)/.filter(|_| maps.is_some() || true)/'
TESTS="-p ruum-core --lib apps::deployments::tests::source_maps_and_the_privacy_file_are_not_served -- --exact"
PLACEHOLDER=0
