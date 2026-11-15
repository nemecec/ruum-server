# Every capability is dropped (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox keeps Docker's default capabilities"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/cap_drop: Some\\(vec!\\[\"ALL\"\\.to_owned\\(\\)\\]\\),/cap_drop: None,/; s/h\\.cap_drop\\.as_deref\\(\\) == Some\\(&\\[\"ALL\"\\.to_owned\\(\\)\\]\\[\\.\\.\\]\\)/true/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=privilege"
PLACEHOLDER=0
