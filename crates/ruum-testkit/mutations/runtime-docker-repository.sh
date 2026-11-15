# Images resolve only in the repository of the spec's tenant and app (card R-2a; STEP3 "How an app runs" rule 1 or 2, decision 0006 G). The mutation removes the
# flag from the create call and from the driver's own read-back, so the hostile sandbox suite (or
# the conformance suite) must notice the missing behaviour itself.
DESCRIPTION="images resolve in a repository per tenant, so another app's image starts"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/format!\\(\"ruum\\/\\{\\}\\/\\{\\}\", identity\\.tenant\\.0, identity\\.app\\.0\\)/format!(\"ruum\\/{}\", identity.tenant.0)/"
TESTS="-p ruum-runtime --test conformance -- --nocapture"
ENV=""
PLACEHOLDER=0
