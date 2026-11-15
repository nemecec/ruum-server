# Output is redacted inside the driver, before a line reaches the ring (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="raw output goes into the log ring"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/r\\.out\\.push_bytes\\(&message, now\\)/vec![(now, String::from_utf8_lossy(&message).trim_end().to_owned())]/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=secrets"
PLACEHOLDER=0
