# The process limit (pids-limit) applies (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox has no process limit"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/pids_limit: Some\\(self\\.cfg\\.pids_limit\\),/pids_limit: None,/; s/want\\(h\\.pids_limit == Some\\(self\\.cfg\\.pids_limit\\), /want(true, /"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=forkbomb"
PLACEHOLDER=0
