# syntax=docker/dockerfile:1.7
# ruum-egress: the egress proxy (card R-2e, crates/ruum-egress/README.md). Build context: the
# repository root (its .dockerignore applies):
#   docker build -f deploy/images/ruum-egress/Dockerfile -t ruum-egress .
# Both bases are pinned by their multi-arch index digest (linux/arm64 for the build server,
# linux/amd64 for the runners and staging). Runtime: distroless, non-root, no shell; nothing is
# written at run time except the control socket (put it on a volume), so `--read-only` works.
# Publishing through .github/workflows/images.yml is a later card.
FROM rust:1.99-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY third_party third_party
ENV CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=always
# A release build without dev-dependencies: the test-only `test-hooks` feature is not in it.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ruum-egress && \
    install -D -m 0755 target/release/ruum-egress /out/ruum-egress

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /out/ruum-egress /app/ruum-egress
ENV RUUM_EGRESS_LISTEN=0.0.0.0:3128
USER 65532:65532
EXPOSE 3128
ENTRYPOINT ["/app/ruum-egress"]
