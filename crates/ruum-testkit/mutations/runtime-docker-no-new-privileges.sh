# no-new-privileges is set (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox may gain privileges (no no-new-privileges)"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/security_opt: Some\\(vec!\\[\"no-new-privileges\"\\.to_owned\\(\\)\\]\\),/security_opt: None,/; s/\\.any\\(\\|s\\| s == \"no-new-privileges\" \\|\\| s == \"no-new-privileges:true\"\\)/.all(|_| true)/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=privilege"
PLACEHOLDER=0
