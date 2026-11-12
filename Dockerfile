# syntax=docker/dockerfile:1

ARG PLURX_MEDIA_RUNTIME_REPOSITORY=ghcr.io/pjunod/cinema-media-runtime

# Pinned by digest: `1-bookworm` floats, and what floats with it is this
# image's Debian system libraries and its own rustup. The compiler is not
# pinned here -- `COPY . .` below puts rust-toolchain.toml in the build
# context and rustup honours it -- so this digest is the system-library
# pin, nothing more. `scripts/image-base-drift` prints this digest beside
# the current upstream one; the weekly Rust dependency audit runs it.
FROM rust:1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
# `.git` is not in the build context (see .dockerignore), so the build script
# can't derive the commit itself — CI passes it in, e.g.
#   docker build --build-arg PLURX_BUILD_REF="$(git describe --tags --always --dirty)"
# Left empty, the binary reports its version with build "unknown", which is
# honest about a context that genuinely has no commit in it.
ARG PLURX_BUILD_REF=""
ENV PLURX_BUILD_REF=${PLURX_BUILD_REF}
ARG PLURX_BUILD_SHA=""
ENV PLURX_BUILD_SHA=${PLURX_BUILD_SHA}
# The commit's committer time, so two builds of one commit stamp the same
# `built_at` into plurxd (crates/plurxd/build_support/source_date.rs) and
# BuildKit writes the same image timestamps. Every caller derives it:
#   docker build --build-arg SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"
# Left unset, build.rs falls back to the compile clock, because this context
# has no `.git` to read the commit time from.
ARG SOURCE_DATE_EPOCH
ARG TARGETARCH
WORKDIR /src
COPY . .
# The target dirs below are cache mounts that outlive any one checkout, and
# cargo decides a path crate is fresh from file mtimes, not contents. A node
# that builds several commits can therefore end up linking plurxd against a
# stale workspace-crate rlib (2026-09-28: `normalize_sup_cancellable_into`
# "not found" in a tree that defines it). Touching every workspace and vendored
# file first (sources and the web assets build.rs watches) makes the mounts
# cache registry dependencies only.
RUN --mount=type=cache,id=plurx-cargo-registry,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=plurx-target-plurxd-${TARGETARCH},sharing=locked,target=/src/target-plurxd \
    --mount=type=cache,id=plurx-target-cluster-check-${TARGETARCH},sharing=locked,target=/src/target-cluster-check \
    find crates vendor -type f -exec touch {} + \
    && ! cargo tree --locked -p plurxd -e features \
        | grep -q 'cluster-read-cost-validation' \
    && CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/src/target-plurxd cargo build --locked --release -p plurxd \
    && cp target-plurxd/release/plurxd /plurxd \
    && cp target-plurxd/release/plurxd.dwp /plurxd.dwp \
    && CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/src/target-cluster-check cargo build --locked --release -p plurx-cluster-check \
    && cp target-cluster-check/release/plurx-cluster-check /plurx-cluster-check \
    && cp target-cluster-check/release/plurx-cluster-check.dwp /plurx-cluster-check.dwp

# Published media dependencies are independent of application source.
FROM ${PLURX_MEDIA_RUNTIME_REPOSITORY}@sha256:b171d3de4e9dc30fb544b3ca18945b5e14624e7d2a7fa005deaded460c9d3e67 AS runtime-assets

# M5's CI image uses the shipped runtime's media assets, not a second ffmpeg
# install on a persistent runner. These two tool images are pinned by index
# digest just like the release bases; neither builds the release binaries.
FROM rust:1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS ci-rust-toolchain
FROM node:22.23.4-bookworm-slim@sha256:de170b39439eb0e5a3444ad7d60b015614a381e10fecae2e30ec9e735122f38d AS ci-node-toolchain

FROM runtime-assets AS ci
ARG PLURX_CI_SOURCE_SHA=""
LABEL org.opencontainers.image.revision="${PLURX_CI_SOURCE_SHA}"
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PLAYWRIGHT_BROWSERS_PATH=/opt/playwright-browsers \
    PATH=/usr/local/cargo/bin:/opt/playwright-venv/bin:${PATH} \
    PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg \
    PLURX_FFPROBE=/usr/lib/jellyfin-ffmpeg/ffprobe
# Copy the pinned rustup launcher, not the source image's default compiler;
# only the repository-pinned 1.97.1 toolchain belongs in this CI layer.
COPY --from=ci-rust-toolchain /usr/local/cargo/bin /usr/local/cargo/bin
COPY --from=ci-node-toolchain /usr/local/bin/node /usr/local/bin/node
COPY LICENSE NOTICE THIRD-PARTY-NOTICES.md /usr/share/doc/plurx/
COPY licenses/ /usr/share/doc/plurx/licenses/
# Keep both the daemon's explicit path and shell-invoked fixture generation on
# jellyfin-ffmpeg 8. Debian bookworm's /usr/bin/ffmpeg remains the release
# fallback, but must never answer an M5 CI job's bare `ffmpeg` invocation.
RUN ln -s /usr/lib/jellyfin-ffmpeg/ffmpeg /usr/local/bin/ffmpeg \
    && ln -s /usr/lib/jellyfin-ffmpeg/ffprobe /usr/local/bin/ffprobe \
    && apt-get -o Acquire::Retries=3 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        build-essential clang cmake curl git jq libssl-dev lld llvm make nasm ninja-build \
        libgtk-4-dev libdbus-1-dev pkg-config python3 python3-venv \
    && python3 -m venv /opt/playwright-venv \
    && /opt/playwright-venv/bin/pip install --no-cache-dir 'playwright==1.62.0' \
    && /opt/playwright-venv/bin/python3 -m playwright install --with-deps chromium \
    && rustup toolchain install 1.97.1 --profile minimal \
        --component rustfmt --component clippy --component llvm-tools-preview \
    && rustc +1.97.1 --version | grep -F 'rustc 1.97.1' \
    && node --version | grep -Fx 'v22.23.4' \
    && ffmpeg -version | grep -E '^ffmpeg version n?8' \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Keep the expensive, architecture-specific runtime asset assertions available
# as their own CI target. The native release-build matrix already proves both
# Rust binaries for arm64; rebuilding the entire Rust graph under QEMU made a
# cold qualification exceed the Docker job's one-hour bound before these
# runtime assertions could report a result.
FROM runtime-assets AS runtime
ARG PLURX_BUILD_SHA=""
ARG PLURX_MEDIA_RUNTIME_DIGEST="sha256:b171d3de4e9dc30fb544b3ca18945b5e14624e7d2a7fa005deaded460c9d3e67"
ENV PLURX_MEDIA_RUNTIME_DIGEST=${PLURX_MEDIA_RUNTIME_DIGEST}
# The fleet rollout inspects this label on the pulled image ID before it trusts
# checkout-owned deployment policy. Redeclare the build arg in this final stage:
# Docker build args are stage-scoped, and a label inherited only by the build
# stage would leave the shipped runtime unverifiable.
LABEL org.opencontainers.image.revision="${PLURX_BUILD_SHA}"
# Apache-2.0 sec. 4(a)/4(d) and the OFL both condition redistribution on the
# license text and notices travelling with the work. The web UI is compiled
# into the binary, fonts and all, so an image carrying only /plurxd
# distributes those components with no notices attached.
#
# These copy from the BUILD CONTEXT rather than from the build stage, and
# that is load bearing: validation/release_dockerfile.py rewrites the runtime
# stage for release and rejects any build-stage copy it does not recognise as
# a known binary artifact -- it matches on the literal token, so even naming
# that form in a comment here fails the rewrite. A context copy passes
# through untouched, and the release build's context (release-source) is a
# full repo checkout, so these paths resolve there too.
# tests/operations/test_release_publication.py holds both halves of that.
COPY LICENSE NOTICE THIRD-PARTY-NOTICES.md /usr/share/doc/plurx/
COPY licenses/ /usr/share/doc/plurx/licenses/
COPY --from=build /plurxd /usr/local/bin/plurxd
COPY --from=build /plurxd.dwp /usr/local/bin/plurxd.dwp
# Stopped-node recovery and cluster validation tooling. The WAL inspector is
# read-only, refuses a live lock, and lets an operator diagnose the same image
# that produced the on-disk state without installing Rust on the host.
COPY --from=build /plurx-cluster-check /usr/local/bin/plurx-cluster-check
COPY --from=build /plurx-cluster-check.dwp /usr/local/bin/plurx-cluster-check.dwp

# NVIDIA Container Toolkit must also mount the video encode/decode libraries.
# This default covers direct Docker users; GPU device access is still runtime
# configuration, supplied automatically by make docker-up on local Linux.
# Default to jellyfin-ffmpeg (recent GPUs need its driver stack); override
# either var to point elsewhere. It's a superset of system ffmpeg, so this is
# safe on hardware that the distro build would also handle.
ENV PLURX_BIND=0.0.0.0:32400 \
    PLURX_DATA_DIR=/var/lib/plurx \
    PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg \
    PLURX_FFPROBE=/usr/lib/jellyfin-ffmpeg/ffprobe \
    PLURX_BOUND_FFPROBE=/usr/local/lib/plurx/ffprobe \
    PLURX_DOVI_TOOL=/usr/local/bin/dovi_tool \
    PLURX_MKVMERGE=/usr/bin/mkvmerge \
    NVIDIA_DRIVER_CAPABILITIES=compute,video,utility,graphics

EXPOSE 32400
VOLUME ["/var/lib/plurx"]
USER plurx

# The default replicated startup may spend 45s reaching Hiqlite health, 45s
# awaiting admission, then a 1,200s snapshot transfer plus a 120s final install
# and 45s reaching its quorum watermark. Twenty-five minutes covers that 1,455s
# budget with margin while still exposing a broken build promptly. Compose can
# lengthen the grace when an operator lengthens either snapshot stage; a
# successful probe ends startup grace immediately and later failures use the
# normal retry cadence.
HEALTHCHECK --interval=30s --timeout=5s --start-period=25m \
    CMD ["plurxd", "healthcheck"]

ENTRYPOINT ["plurxd"]
CMD ["run"]

# Default application image.
FROM runtime AS default-runtime
