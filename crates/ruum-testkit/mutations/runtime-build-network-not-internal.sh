# A build's network is internal with inhibit_ipv4 (card R-2b; decision 0006 G point 1). The
# mutation creates it without `internal` and drops the builder's own read-back of the flag.
DESCRIPTION="the build network is not internal"
FILE="crates/ruum-runtime/src/builder/job.rs"
PERL="s/internal: Some\\(true\\),/internal: Some(false),/; s/if n\\.internal != Some\\(true\\) \\|\\| /if /"
TESTS="-p ruum-runtime --test build -- --nocapture the_builder_holds"
ENV="RUUM_BUILD_CASES=image,hardening"
PLACEHOLDER=0
