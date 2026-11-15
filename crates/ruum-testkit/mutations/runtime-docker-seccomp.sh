# Docker's default seccomp profile applies, never unconfined (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="the sandbox runs with seccomp=unconfined"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/security_opt: Some\\(vec!\\[\"no-new-privileges\"\\.to_owned\\(\\)\\]\\)/security_opt: Some(vec![\"no-new-privileges\".to_owned(), \"seccomp=unconfined\".to_owned()])/; s/!sec\\.iter\\(\\)\\.any\\(\\|s\\| s\\.contains\\(\"unconfined\"\\)\\)/true/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=privilege"
PLACEHOLDER=0
