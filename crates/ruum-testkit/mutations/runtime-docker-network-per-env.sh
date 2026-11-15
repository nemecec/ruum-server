# One network per app and environment (live and preview never share one) (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="live and preview of one app share a network"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/(identity\\.app\\.0\\.simple\\(\\),\\s*)identity\\.environment\\.as_str\\(\\)/\$1\"shared\"/"
TESTS="-p ruum-runtime --test conformance -- --nocapture"
ENV=""
PLACEHOLDER=0
