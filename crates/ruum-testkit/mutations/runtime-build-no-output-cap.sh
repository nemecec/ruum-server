# The build's output leaves the sandbox under a byte cap (card R-2b, "Output out of the
# sandbox"): the job is killed when it writes more. The mutation removes the cap of the copy.
DESCRIPTION="the build's output is copied without a byte cap"
FILE="crates/ruum-runtime/src/builder/job.rs"
PERL="s/let cap = cfg\\.output_max;/let cap = u64::MAX;/"
TESTS="-p ruum-runtime --test build -- --nocapture the_builder_holds"
ENV="RUUM_BUILD_CASES=limits"
PLACEHOLDER=0
