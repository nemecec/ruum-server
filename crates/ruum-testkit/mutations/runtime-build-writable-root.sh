# The build job runs with a read-only root (card R-2b; STEP3 "How an app runs" rule 1: the same
# hardening as an app). The mutation drops the flag from the job's create call and from the
# builder's own read-back, so the comparison with the driver's container must notice.
DESCRIPTION="the build job gets a writable root"
FILE="crates/ruum-runtime/src/builder/job.rs"
PERL="s/readonly_rootfs: Some\\(true\\),/readonly_rootfs: Some(false),/; s/want\\(h\\.readonly_rootfs == Some\\(true\\), \"read-only root\"\\);//"
TESTS="-p ruum-runtime --test build -- --nocapture the_builder_holds"
ENV="RUUM_BUILD_CASES=image,hardening"
PLACEHOLDER=0
