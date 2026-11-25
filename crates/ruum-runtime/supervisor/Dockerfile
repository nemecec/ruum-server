# The sandbox supervisor's image (backlog I6-20, S-89; docs/STEP3.md "The sandbox supervisor").
# Only the static binary, at /ruum-supervisor: the init container of each Kubernetes sandbox runs
# `/ruum-supervisor install /ruum/bin/ruum-supervisor`, which copies it into a memory volume that
# the app container mounts read-only (crates/ruum-runtime src/kube pod.rs).
#
# Build from the repository root:
#   docker build -f crates/ruum-runtime/supervisor/Dockerfile -t ruum-supervisor .
# On Alpine the Rust target is musl and links statically. The binary needs std, nix and serde only.
# Publishing it (a pinned digest for runtime.supervisorImage) is a card of its own for
# .github/workflows/images.yml (decision 0008-3).
FROM rust:1.99-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY third_party third_party
RUN cargo build --release --locked -p ruum-supervisor && strip target/release/ruum-supervisor

FROM scratch
COPY --from=build /src/target/release/ruum-supervisor /ruum-supervisor
USER 10001:10001
ENTRYPOINT ["/ruum-supervisor"]
