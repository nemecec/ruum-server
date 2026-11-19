# The network authenticator gives an unknown peer address a default identity instead of
# refusing it (card O-1b). Security rule: no identity, no write; an address that names no
# running instance is refused (401 / UNAUTHENTICATED).
DESCRIPTION="the telemetry source authenticator gives an unknown address a default identity"
FILE="crates/ruum-core/src/telemetry/source.rs"
PERL='s/\(self\.lookup\)\(facts\)\n            \.map\(/(self.lookup)(facts).or(Some(WorkloadIdentity { tenant: TenantId::from(Uuid::nil()), app: NodeId::from(Uuid::nil()), deployment: Uuid::nil(), environment: Environment::Live }))\n            .map(/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
