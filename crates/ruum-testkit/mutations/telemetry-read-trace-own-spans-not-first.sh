# getAppTrace reads this app's spans only under the shared limit (review O-2, Kimi 4). Rule: a
# called app that writes many older spans into a trace must not crowd out the spans of the app
# whose trace is read (404 or a trace without them).
DESCRIPTION="the trace read of this app's spans has no app filter"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/app: Some\(app\),\n(\s*)trace_id: Some\(trace_id\.to_owned\(\)\),/app: None,\n$1trace_id: Some(trace_id.to_owned()),/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
