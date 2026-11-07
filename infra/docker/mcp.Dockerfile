# syntax=docker/dockerfile:1.7

FROM rust:1.98.1-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS build
ARG TARGETPLATFORM
WORKDIR /src
RUN apt-get update \
    && apt-get install --yes --no-install-recommends build-essential pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
COPY crates ./crates
COPY services ./services
RUN --mount=type=cache,id=peppy-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=peppy-mcp-target-${TARGETPLATFORM},target=/src/target \
    cargo build --release --locked -p peppy-mcp \
    && mkdir -p /out \
    && cp /src/target/release/peppy-mcp /out/peppy-mcp

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home peppy
COPY --from=build /out/peppy-mcp /usr/local/bin/peppy-mcp
USER peppy
ENTRYPOINT ["/usr/local/bin/peppy-mcp"]
