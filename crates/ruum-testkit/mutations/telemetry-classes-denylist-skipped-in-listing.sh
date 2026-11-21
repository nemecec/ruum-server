# listAppTelemetryFields shows the row's class instead of the effective one (card O-2). Security
# rule: deny-listed names are always secret, whatever a row, the privacy file or a person says.
DESCRIPTION="the field listing skips the deny-list"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/class: r\.effective_class\(\),/class: r.class,/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
