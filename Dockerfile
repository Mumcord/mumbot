# Multi-arch image of the Mumbot binary. The build stage runs on the build machine's own platform
# and cross-compiles a static musl binary with zig, so an arm64 image never needs a slow emulated
# Rust build. The `:-` defaults keep a plain `docker build` (without buildx) working.
FROM --platform=${BUILDPLATFORM:-linux/amd64} ghcr.io/rust-cross/cargo-zigbuild:0.24.0 AS build
ARG TARGETARCH
# Empty for the main bot, "tester" for the mumbot-tester image (spec 03, FR-3.12).
ARG FEATURES=""
# The Opus codec (C, behind mumcord-audio) is built with CMake.
RUN apt-get update -qq \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
# Cargo.toml names this example (tester only), so it must exist for any build.
COPY examples/loadtest.rs ./examples/
RUN case "${TARGETARCH:-amd64}" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && cargo zigbuild --release --locked --target "$target" ${FEATURES:+--features "$FEATURES"} \
    && mkdir -p /out/data \
    && cp "target/$target/release/mumbot" /out/mumbot

# Alpine instead of scratch only for busybox `wget`, which the health check needs.
FROM alpine:3.24
COPY --from=build /out/mumbot /usr/local/bin/mumbot
# The bot's client certificate (its identity on the server) lives here; keep it on a volume.
COPY --from=build --chown=10001:10001 /out/data /data
ENV MUMBOT_CONFIG=/config/bot.toml \
    MUMBOT_DATA=/data \
    RUST_LOG=info
USER 10001:10001
VOLUME /data
EXPOSE 8080
# Assumes the default container port 8080 (http.listen = "0.0.0.0:8080").
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD wget -q -O /dev/null http://127.0.0.1:8080/health || exit 1
ENTRYPOINT ["mumbot"]
CMD ["webhook"]
