# The query escaping of the real telemetry store is off: values go into LogQL, PromQL and
# TraceQL as they are (card O-1b). Security rule: every value in a store query is escaped by
# the server, so a filter value cannot break out of the selector (reads never cross tenants).
DESCRIPTION="the telemetry store query escaping is off"
FILE="crates/ruum-core/src/telemetry/lgtm.rs"
PERL='s/    for c in value\.chars\(\) \{/    out.push_str(value);\n    for c in "".chars() {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
