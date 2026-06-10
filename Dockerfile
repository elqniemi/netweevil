# syntax=docker/dockerfile:1

FROM rust:1.88-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p netweevil-cli \
    && cp target/release/netweevil /usr/local/bin/netweevil

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /data netweevil \
    && mkdir -p /data/.netweevil \
    && chown -R netweevil /data/.netweevil

COPY --from=builder /usr/local/bin/netweevil /usr/local/bin/netweevil

USER netweevil
# The CLI keeps all state under <workdir>/.netweevil; mount a volume there.
WORKDIR /data

EXPOSE 8080

ENTRYPOINT ["netweevil"]
CMD ["--help"]
