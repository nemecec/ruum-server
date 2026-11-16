# Any metric name is stored (card O-1a, review revo1a item 3). Security rule: a metric name is
# attacker text and must have the OTel instrument name syntax (design 9.6).
DESCRIPTION="the telemetry pipeline stores metric names of any syntax"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if !is_metric_name\(&rec\.name\) \{/if false {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
