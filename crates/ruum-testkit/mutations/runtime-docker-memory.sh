# The memory limit (swap = memory) applies (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox has no memory limit (and reports the requested one)"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/memory: Some\\(spec\\.limits\\.memory_bytes as i64\\),/memory: None,/; s/memory_swap: Some\\(spec\\.limits\\.memory_bytes as i64\\),/memory_swap: None,/; s/memory_bytes: h\\.memory\\.map_or\\(0, \\|m\\| m as u64\\),/memory_bytes: spec.limits.memory_bytes,/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=memory"
PLACEHOLDER=0
