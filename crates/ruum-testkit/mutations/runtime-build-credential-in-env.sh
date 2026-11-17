# The registry push credential never enters the build job (card R-2b; STEP3 rule 7; backlog
# S-37). The mutation puts the password into the job's environment; the conformance case
# build_sees_no_platform_secrets (hex-encoded env dump) must notice.
DESCRIPTION="the push credential is in the build job's environment"
FILE="crates/ruum-runtime/src/builder/job.rs"
PERL="s/format!\\(\"RUUM_SOURCE_BYTES=\\{source_bytes\\}\"\\),/format!(\"RUUM_SOURCE_BYTES={source_bytes}\"), format!(\"REGISTRY_AUTH={}\", cfg.credential.as_ref().map(|c| c.password.expose().to_owned()).unwrap_or_default()),/"
TESTS="-p ruum-runtime --test build -- --nocapture the_builder_passes_the_conformance_suite"
ENV=""
PLACEHOLDER=0
