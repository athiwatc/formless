# Formless daemon for servers (docs/docker.md): the daemon runs headless and
# stays up (`--stay`), so scheduled jobs fire; reach it through the relay
# or `docker exec … formless`.
# Bases are pinned by digest (update both together: docs/docker.md).
FROM rust:1-bookworm@sha256:9181b56c2a21ab1d5e73cffe04fc762426d30aa7fac3c91141dd033f49a7fc8b AS build
# The keyring's Secret Service backend links libdbus.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin formless

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
LABEL org.opencontainers.image.source="https://github.com/athiwatc/formless" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.title="formless" \
      org.opencontainers.image.description="A tool-building AI agent harness"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git tini libdbus-1-3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 formless \
    && mkdir -p /data /config && chown formless:formless /data /config
COPY --from=build /src/target/release/formless /usr/local/bin/formless
COPY LICENSE /usr/share/doc/formless/LICENSE
USER formless
ENV FORMLESS_DATA_DIR=/data \
    FORMLESS_CONFIG_DIR=/config
VOLUME ["/data", "/config"]
# Healthy while the daemon answers on its socket.
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s \
    CMD ["sh", "-c", "! formless daemon status | grep -q 'not running'"]
ENTRYPOINT ["/usr/bin/tini", "--", "formless"]
CMD ["daemon", "serve", "--stay"]
