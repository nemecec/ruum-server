# The root file system is read-only (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox root file system is writable"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/readonly_rootfs: Some\\(true\\)/readonly_rootfs: Some(false)/; s/want\\(h\\.readonly_rootfs == Some\\(true\\), /want(true, /"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=privilege"
PLACEHOLDER=0
