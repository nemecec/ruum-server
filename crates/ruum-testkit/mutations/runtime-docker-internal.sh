# App networks are internal (no route out) (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="app networks are not internal"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/internal: Some\\(true\\),/internal: Some(false),/; s/if inspect\\.internal != Some\\(true\\)/if false/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=network"
PLACEHOLDER=0
