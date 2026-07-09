# syntax=docker/dockerfile:1
#
# From-source, reproducible image — the canonical recipe for `docker build`
# locally and the PR docker-build check. The RELEASE pipeline does NOT use this
# file; it assembles a multi-arch image from the already-compiled release
# binaries (docker/release.Dockerfile) to avoid recompiling the same artifact.
#
# aws-lc-rs (the pinned rustls crypto provider) compiles C, so the builder needs
# cmake + clang. The runtime is distroless/cc (glibc + libgcc/libstdc++).

FROM rust:1.87-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Release profile already strips + thin-LTOs (see workspace Cargo.toml).
RUN cargo build -p vibecast-cli --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.source="https://github.com/emilsvennesson/vibecast"
LABEL org.opencontainers.image.description="Native Google Cast receiver"
LABEL org.opencontainers.image.licenses="MIT"
COPY --from=builder /src/target/release/vibecast /usr/local/bin/vibecast
# CastV2 TLS (8009), eureka HTTP (8008), player bridge (8010). mDNS discovery
# needs host networking at run time: `docker run --network host ...`.
EXPOSE 8008 8009 8010
ENTRYPOINT ["/usr/local/bin/vibecast"]
