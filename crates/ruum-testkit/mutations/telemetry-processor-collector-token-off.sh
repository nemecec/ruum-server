# The Collector's HTTP receiver does not require the processor's write token (card O-1b).
# Security rule: the Collector accepts writes only from the processor, so a sandbox that can
# reach it still cannot write past the processor. scripts/telemetry_check.sh fails on it too
# ("write without the Collector token refused"); the cargo test reads the config.
DESCRIPTION="the telemetry Collector takes HTTP writes without the processor's token"
FILE="deploy/dev/otel/collector.yaml"
PERL='s/(      http:\n        endpoint: 0\.0\.0\.0:4318\n        include_metadata: true\n)        auth:\n          authenticator: bearertokenauth\/processor\n/$1/'
TESTS="-p ruum-server telemetry"
PLACEHOLDER=0
