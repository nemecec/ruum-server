# A build pushes to the repository of its own tenant and app only (card R-2b, "The push"). The
# mutation pushes to another repository of the tenant.
DESCRIPTION="the builder pushes to another repository"
FILE="crates/ruum-runtime/src/builder/mod.rs"
PERL="s/            repository\\(&input\\.identity\\),/            format!(\"ruum\\/{}\\/other\", input.identity.tenant.0),/"
TESTS="-p ruum-runtime --test build -- --nocapture the_builder_holds"
ENV="RUUM_BUILD_CASES=image"
PLACEHOLDER=0
