# The core signs static file links for an app in the trash (decision 0008-4). Security rule: an
# app in the trash serves nothing (the host lookup leaves it out), and the core's static/file
# route, which a gateway that is not what it should be can call directly, checks it too.
DESCRIPTION="the core signs links for the files of an app in the trash"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/match app_in_trash\(&s, q\.tenant_id, q\.app_id\)\.await \{/match Ok::<bool, ruum_core::Error>(false) {/'
TESTS="-p ruum-gateway --test static_links the_core_signs_links_only_for_serving_static_deployments -- --exact"
PLACEHOLDER=0
