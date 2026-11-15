# App networks are created with inhibit_ipv4 (no host address on the bridge) (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="app networks give the host an address on their bridge"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/(INHIBIT_IPV4\\.to_owned\\(\\),\\s*)\"true\"/\$1\"false\"/; s/\\|\\| inhibit != Some\\(\"true\"\\)/|| false/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=network"
PLACEHOLDER=0
