# The core signs a static file link for an app in the trash (card I6-3D; Kimi review of I6-3D,
# 1). Security rule: decision 0008-4 and the runtime API's serving rule: a trashed app serves
# nothing, also to a gateway that calls the core's route directly and skips the host lookup.
DESCRIPTION="the core links static files of a trashed app"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/Ok\(true\) => return Ok\(true\),/Ok(true) => {}/'
TESTS="-p ruum-api --lib internal::tests::upstream_route"
PLACEHOLDER=0
