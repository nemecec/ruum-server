# The log search does not build its selector from the path's app (card O-2): no app in the store
# filter and no check of the records' app. Security rule: the tenant and the app come from the
# path and the caller's context, the selector is built by the server; a search returns only this
# app's records (T-4 injection case, docs/STEP3.md "Telemetry and privacy").
DESCRIPTION="searchAppLogs reads without the path's app"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/signal: Some\(Signal::Logs\),\n(\s*)app: Some\(app\),/signal: Some(Signal::Logs),\n$1app: None,/; s/\.filter\(\|r\| r\.source\.tenant == ctx\.tenant && r\.source\.app == app\)/.filter(|r| r.source.tenant == ctx.tenant)/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
