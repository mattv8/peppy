FROM rust:1.98.1-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
COPY crates ./crates
COPY services ./services
RUN cargo build --release --locked -p peppy-push-relay

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home peppy
COPY --from=build /src/target/release/peppy-push-relay /usr/local/bin/peppy-push-relay
USER peppy
ENV BIND_ADDR=0.0.0.0:8090
EXPOSE 8090
ENTRYPOINT ["/usr/local/bin/peppy-push-relay"]
