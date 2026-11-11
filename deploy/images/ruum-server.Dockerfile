# syntax=docker/dockerfile:1.7
# ruum-server for staging and production (plan 6.1). Built by .github/workflows/images.yml on
# GitHub (amd64), never on the build server. Runtime: distroless, non-root, no shell.
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY contracts contracts
COPY migrations migrations
COPY third_party third_party
ENV CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=always
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ruum-server && \
    install -D -m 0755 target/release/ruum-server /out/ruum-server

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/ruum-server /app/ruum-server
COPY migrations /app/migrations
ENV RUUM_MIGRATIONS_DIR=/app/migrations RUUM_BIND=0.0.0.0:8491 RUUM_DAV_BIND=0.0.0.0:8492
USER 65532:65532
EXPOSE 8491 8492 8493
ENTRYPOINT ["/app/ruum-server"]
