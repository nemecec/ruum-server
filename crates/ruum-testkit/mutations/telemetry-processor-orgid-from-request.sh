# The HTTP receiver takes the storage tenant from the request's X-Scope-OrgID header (card
# O-1b). Security rule: the storage tenant comes only from the identity of the authenticated
# source; a forged X-Scope-OrgID changes nothing.
DESCRIPTION="the telemetry receiver takes the storage tenant from the request's X-Scope-OrgID"
FILE="crates/ruum-server/src/telemetry.rs"
PERL='s/(    let identity = match r\.admit\(&facts\)\.await \{\n        Ok\(id\) => id,\n        Err\(e\) => return refused_response\(e\),\n    \};\n)/$1    let identity = SourceIdentity { tenant: headers.get("x-scope-orgid").and_then(|v| v.to_str().ok()).and_then(|t| t.parse().ok()).unwrap_or(identity.tenant), ..identity };\n/'
TESTS="-p ruum-server telemetry"
PLACEHOLDER=0
