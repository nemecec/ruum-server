# A sandbox has no writable host folder but the token file: an image `VOLUME` is refused (card
# R-2a review, change 1; STEP3 "How an app runs" rule 1). The mutation removes the image check and
# the driver's read-back of the container's mounts, so the hostile sandbox suite must notice the
# anonymous volume itself. The read-back is matched across lines (`want(` and `mounts_ok,` are on
# two lines since I6-20; the old pattern no longer matched, so the mutation removed one check only).
DESCRIPTION="an image VOLUME gives the sandbox a writable volume with no size limit"
FILE="crates/ruum-runtime/src/driver.rs"
PERL="s/if !volumes\\.is_empty\\(\\) \\{/if volumes.is_empty() \\&\\& false {/; s/want\\(\\s*mounts_ok,/want(true,/"
TESTS="-p ruum-runtime --test sandbox -- --nocapture"
ENV="RUUM_SANDBOX_CASES=volumes"
PLACEHOLDER=0
