# syntax=docker/dockerfile:1
#
# Release image: assembled from the standalone Linux binaries already built by
# the build-linux matrix (release.yml) — no recompilation. The release job uses
# a dedicated build context (a staging dir) holding only `linux/<arch>/vibecast`
# plus this file, so buildx picks the right binary per platform via TARGETARCH.
#
# Runtime is distroless/cc (glibc + libgcc/libstdc++), which satisfies the
# glibc-linked, aws-lc-rs-static binary.

FROM gcr.io/distroless/cc-debian12:nonroot
ARG TARGETARCH
LABEL org.opencontainers.image.source="https://github.com/emilsvennesson/vibecast"
LABEL org.opencontainers.image.description="Native Google Cast receiver"
LABEL org.opencontainers.image.licenses="MIT"
COPY linux/${TARGETARCH}/vibecast /usr/local/bin/vibecast
# CastV2 TLS (8009), eureka HTTP (8008), player bridge (8010). mDNS discovery
# needs host networking at run time: `docker run --network host ...`.
EXPOSE 8008 8009 8010
ENTRYPOINT ["/usr/local/bin/vibecast"]
