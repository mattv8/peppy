# syntax=docker/dockerfile:1.7

# The Emscripten SDK only publishes Linux x86_64 tools. Build browser assets on
# that fixed platform; the resulting JavaScript and WASM are portable and can
# be copied into both amd64 and arm64 server-image runtimes.
FROM --platform=linux/amd64 rust:1.98.1-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS web-build
ARG EMSDK_VERSION=6.0.11
ARG EMSDK_REVISION=dd8e25632640cfc1fb570c7fa4cc374e8a5e5a72
ARG NODE_VERSION=24.21.0
ARG PNPM_VERSION=12.8.1
WORKDIR /src
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl git python3 tar xz-utils build-essential cmake pkg-config libssl-dev libsodium-dev libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/* \
    && git init /opt/emsdk \
    && cd /opt/emsdk \
    && git remote add origin https://github.com/emscripten-core/emsdk.git \
    && git fetch --depth 1 origin "refs/tags/$EMSDK_VERSION" \
    && git checkout --detach FETCH_HEAD \
    && test "$(git rev-parse HEAD)" = "$EMSDK_REVISION" \
    && ./emsdk install "$EMSDK_VERSION" \
    && ./emsdk activate "$EMSDK_VERSION" \
    && case "$(dpkg --print-architecture)" in amd64) node_arch=x64 ;; arm64) node_arch=arm64 ;; *) echo "unsupported Node architecture" >&2; exit 1 ;; esac \
    && cd /tmp && node_archive="node-v${NODE_VERSION}-linux-${node_arch}.tar.xz" \
    && curl --fail --location --silent --show-error -O "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" \
    && curl --fail --location --silent --show-error -O "https://nodejs.org/dist/v${NODE_VERSION}/${node_archive}" \
    && grep "  ${node_archive}$" SHASUMS256.txt | sha256sum -c - \
    && tar -xJf "$node_archive" --strip-components=1 -C /usr/local \
    && rm -f SHASUMS256.txt "$node_archive" \
    && COREPACK_HOME=/root/.cache/corepack corepack enable \
    && COREPACK_HOME=/root/.cache/corepack corepack prepare "pnpm@${PNPM_VERSION}" --activate \
    && rustup target add wasm32-unknown-emscripten
ENV EMSDK=/opt/emsdk \
    EM_CONFIG=/opt/emsdk/.emscripten \
    EM_CACHE=/opt/emsdk/upstream/emscripten/cache \
    PATH=/opt/emsdk:/opt/emsdk/upstream/emscripten:/usr/local/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

COPY Cargo.toml Cargo.lock rust-toolchain.toml package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY vendor ./vendor
COPY crates ./crates
COPY services ./services
COPY infra/build/build-browser-core.sh infra/build/build-web-client.sh ./infra/build/
COPY apps/desktop ./apps/desktop
COPY apps/web ./apps/web
COPY packages/browser-runtime ./packages/browser-runtime
COPY packages/contracts ./packages/contracts
COPY packages/desktop-ui ./packages/desktop-ui
COPY packages/generated ./packages/generated
COPY packages/mobile-design ./packages/mobile-design
RUN --mount=type=cache,id=peppy-browser-core-linux-amd64,target=/src/target/browser-core \
    --mount=type=cache,id=peppy-pnpm-linux-amd64,target=/root/.cache/pnpm \
    CARGO_TARGET_DIR=/src/target/browser-core \
    bash infra/build/build-web-client.sh

FROM rust:1.98.1-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS build
ARG TARGETPLATFORM
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
COPY crates ./crates
COPY services ./services
RUN --mount=type=cache,id=peppy-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=peppy-server-target-${TARGETPLATFORM},target=/src/target \
    cargo build --release --locked -p peppy-server \
    && mkdir -p /out \
    && cp /src/target/release/peppy-server /out/peppy-server

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home peppy
COPY --from=build /out/peppy-server /usr/local/bin/peppy-server
COPY --from=web-build /src/apps/web/dist /usr/share/peppy/web
ENV PEPPY_WEB_CLIENT_DIR=/usr/share/peppy/web
USER peppy
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/peppy-server"]
