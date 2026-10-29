# Formless daemon for servers (docs/docker.md): the daemon runs headless and
# stays up (`--stay`), so scheduled jobs fire; reach it through the relay
# or `docker exec … formless`.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin formless

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 formless \
    && mkdir -p /data /config && chown formless:formless /data /config
COPY --from=build /src/target/release/formless /usr/local/bin/formless
USER formless
ENV FORMLESS_DATA_DIR=/data \
    FORMLESS_CONFIG_DIR=/config
VOLUME ["/data", "/config"]
ENTRYPOINT ["/usr/bin/tini", "--", "formless"]
CMD ["daemon", "serve", "--stay"]
