# syntax=docker/dockerfile:1.7
# ruum-gateway: the app gateway (decision 0006-1, crates/ruum-gateway; card I-6). Built by
# .github/workflows/images.yml on GitHub (amd64), never on the build server. Build context: the
# repository root (its .dockerignore applies). Both bases are pinned by their multi-arch index
# digest (the same pins as deploy/images/ruum-egress/Dockerfile). Runtime: distroless, non-root,
# no shell; it writes nothing at run time (the user-token key is a mounted file), so a read-only
# root works.
FROM rust:1.99-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY contracts contracts
COPY migrations migrations
COPY third_party third_party
ENV CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=always
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ruum-gateway && \
    install -D -m 0755 target/release/ruum-gateway /out/ruum-gateway

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /out/ruum-gateway /app/ruum-gateway
ENV RUUM_GATEWAY_BIND=0.0.0.0:8494
USER 65532:65532
EXPOSE 8494
ENTRYPOINT ["/app/ruum-gateway"]
