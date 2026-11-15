# The sandbox runs as 10001:10001, never the image's user (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox runs as the image's user (root)"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/user: Some\\(SANDBOX_USER\\.to_owned\\(\\)\\),/user: None,/; s/cfg\\.user\\.as_deref\\(\\) == Some\\(SANDBOX_USER\\)/true/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=privilege"
PLACEHOLDER=0
