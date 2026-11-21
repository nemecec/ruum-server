# getAppTrace shows another app's spans in full without edit on that app (card O-2). Security
# rule: another app's spans appear in full only when the caller has edit on that app too, else
# as one opaque span (docs/STEP3.md card O-2).
DESCRIPTION="getAppTrace shows other apps' spans to everyone"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/let full = seen\.is_some\(\)/let full = true || seen.is_some()/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
