# A built image declares no VOLUME (card R-2b; the driver refuses such images, review of R-2a).
# The mutation adds one to the image config.
DESCRIPTION="the built image declares a volume"
FILE="crates/ruum-runtime/src/builder/oci.rs"
PERL="s/\"WorkingDir\": \"\\/app\",/\"WorkingDir\": \"\\/app\", \"Volumes\": {\"\\/data\": {}},/"
TESTS="-p ruum-runtime --test build -- --nocapture the_builder_holds"
ENV="RUUM_BUILD_CASES=image"
PLACEHOLDER=0
