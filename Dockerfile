# syntax=docker/dockerfile:1

FROM rust:1.96.1-slim-bookworm AS builder
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

# --- Optional build-time data provisioning ----------------------------------
#
# By default the image ships without data: mount ./datasets and import into
# the state volume at runtime (see docker-compose.yml / README). For a
# self-contained image, pass build args and the stages below fetch the data
# and bake the imported `.netweevil` state into the final image:
#
#   OSM extract:
#     docker build --build-arg DATA_SOURCE_URL=https://download.geofabrik.de/... .
#   Overture transportation segments (bbox = west,south,east,north):
#     docker build --build-arg OVERTURE_BBOX=6.4,53.1,6.7,53.3 .
#
# Raw downloads stay in intermediate stages; only the imported bundles land
# in the final image, and they seed the `netweevil-state` named volume on
# its first mount.

FROM debian:bookworm-slim AS data-fetch
ARG DATA_SOURCE_URL=""
ARG OVERTURE_BBOX=""
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /fetch/datasets
RUN if [ -n "$DATA_SOURCE_URL" ]; then \
        curl -fL -o /fetch/datasets/source.osm.pbf "$DATA_SOURCE_URL"; \
    fi
# The Overture download uses the official Python CLI (bbox extraction from
# the release GeoParquet on S3); the toolchain installs only when requested.
RUN if [ -n "$OVERTURE_BBOX" ]; then \
        apt-get update \
        && apt-get install -y --no-install-recommends python3 python3-pip \
        && rm -rf /var/lib/apt/lists/* \
        && python3 -m pip install --no-cache-dir --break-system-packages overturemaps \
        && overturemaps download --bbox="$OVERTURE_BBOX" -f geoparquet \
            --type=segment -o /fetch/datasets/overture-segments.parquet; \
    fi

FROM runtime AS importer
ARG DATASET_NAME=osm
COPY --from=data-fetch --chown=netweevil /fetch/datasets /fetch/datasets
RUN if [ -n "$(ls -A /fetch/datasets)" ]; then \
        netweevil dataset import "/fetch/datasets/$(ls /fetch/datasets | head -n 1)" \
            --name "$DATASET_NAME"; \
    fi

FROM runtime AS final
COPY --from=importer --chown=netweevil /data/.netweevil /data/.netweevil
