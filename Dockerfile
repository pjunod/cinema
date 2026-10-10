# syntax=docker/dockerfile:1

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
    && CARGO_TARGET_DIR=/src/target-plurxd cargo build --locked --release -p plurxd \
    && cp target-plurxd/release/plurxd /plurxd \
    && cp target-plurxd/release/plurxd.dwp /plurxd.dwp \
    && CARGO_TARGET_DIR=/src/target-cluster-check cargo build --locked --release -p plurx-cluster-check \
    && cp target-cluster-check/release/plurx-cluster-check /plurx-cluster-check \
    && cp target-cluster-check/release/plurx-cluster-check.dwp /plurx-cluster-check.dwp

# Cached source-only DV dependencies are independent of daemon and helper edits.
FROM rust:1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS dv-processing-dependencies
ARG DEBIAN_SNAPSHOT=20260928T000000Z
ARG TARGETARCH
ARG PLURX_DV_BUILD_JOBS=2
ENV PLURX_DV_BUILD_JOBS=${PLURX_DV_BUILD_JOBS} PLURX_DV_BUILD_INPUTS=/build-inputs
RUN sed -i \
      -e 's|http://deb.debian.org/debian-security|http://snapshot.debian.org/archive/debian-security/'"${DEBIAN_SNAPSHOT}"'/|' \
      -e 's|http://deb.debian.org/debian|http://snapshot.debian.org/archive/debian/'"${DEBIAN_SNAPSHOT}"'/|' \
      /etc/apt/sources.list.d/debian.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99plurx-snapshot \
    && apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl build-essential pkg-config ninja-build nasm python3-venv \
      libvulkan-dev glslang-dev libxxhash-dev \
    && rm -rf /var/lib/apt/lists/*
COPY tools/dv_processing/build-requirements.txt /build-inputs/build-requirements.txt
RUN python3 -m venv /opt/dv-build-python \
    && /opt/dv-build-python/bin/pip install --require-hashes --only-binary=:all: -r /build-inputs/build-requirements.txt
ENV PATH=/opt/dv-build-python/bin:${PATH}
WORKDIR /build-inputs
COPY rust-toolchain.toml .
COPY tools/dv_quality/backends/parsed-rpu/fetch_parser.py tools/dv_quality/backends/parsed-rpu/resolved-Cargo.lock parsed-rpu/
COPY tools/dv_quality/backends/libplacebo/fetch_sources.py libplacebo/
COPY tools/dv_processing/build-dependencies.sh .
RUN --mount=type=cache,id=plurx-dv-cargo-registry,sharing=locked,target=/usr/local/cargo/registry \
    sh /build-inputs/build-dependencies.sh /opt/dv-dependencies

# Only helper source changes invalidate this small compile/identity layer.
FROM dv-processing-dependencies AS dv-processing-helpers
COPY tools/dv_processing /build-inputs/dv_processing
COPY LICENSE NOTICE /build-inputs/project-notices/
ENV PLURX_DV_PROJECT_NOTICES=/build-inputs/project-notices
RUN sh /build-inputs/dv_processing/build-bundle.sh /opt/dv-dependencies /opt/dv-bundle

# Headless bundle/loader qualification without the daemon or production encoder.
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS dv-processing-helper-check
ARG DEBIAN_SNAPSHOT=20260928T000000Z
RUN sed -i \
      -e 's|http://deb.debian.org/debian-security|http://snapshot.debian.org/archive/debian-security/'"${DEBIAN_SNAPSHOT}"'/|' \
      -e 's|http://deb.debian.org/debian|http://snapshot.debian.org/archive/debian/'"${DEBIAN_SNAPSHOT}"'/|' \
      /etc/apt/sources.list.d/debian.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99plurx-snapshot \
    && apt-get update && apt-get install -y --no-install-recommends \
      python3 libvulkan1 mesa-vulkan-drivers liblcms2-2 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=dv-processing-helpers /opt/dv-bundle /usr/lib/plurx/dv-processing
RUN python3 /usr/lib/plurx/dv-processing/sources/bundle-manifest.py \
      /usr/lib/plurx/dv-processing/sources /usr/lib/plurx/dv-processing --verify \
    && test -n "$(find /usr/share/vulkan/icd.d -name '*.json' -print -quit)" \
    && for helper in segment_decode_render mux_rgb author_p81; do \
      status=0; LD_LIBRARY_PATH=/usr/lib/plurx/dv-processing/lib \
        /usr/lib/plurx/dv-processing/bin/$helper >/tmp/helper.out 2>/tmp/helper.err || status=$?; \
      test "$status" -eq 1 && grep -q 'usage:' /tmp/helper.err || exit 1; \
    done && rm /tmp/helper.out /tmp/helper.err

# Pinned by digest for the same reason, and with more at stake: this layer
# is the shipped image's entire userland, and `bookworm-slim` moves under
# the same tag on every Debian point release.
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS runtime-assets
ARG TARGETARCH
ARG DOVI_TOOL_VERSION=2.3.3
ARG MKVTOOLNIX_VERSION=74.0.0-1
ARG DEBIAN_SNAPSHOT=20260928T000000Z
ARG JELLYFIN_FFMPEG_VERSION=8.1.3-1-bookworm
# The facts collector seals one executable, so Jellyfin's dynamically linked
# ffprobe cannot be its trusted input. Build a separate static local-file
# probe; keep the ordinary Jellyfin probe and hardware encoder intact.
COPY scripts/build-static-ffprobe /usr/local/libexec/build-static-ffprobe
COPY scripts/sealed-parser-dependency-offers.py scripts/sealed-parser-dependency-sources.json scripts/prepare-linux-dolby-sdk /usr/local/libexec/
COPY scripts/prepare-linux-dolby-ffmpeg /usr/local/libexec/prepare-linux-dolby-ffmpeg
COPY scripts/linux-video-ffmpeg-patches/0001-require-current-dolby-state.patch /usr/local/libexec/linux-video-ffmpeg-patches/0001-require-current-dolby-state.patch
COPY scripts/build-static-vmaf-scorer /usr/local/libexec/build-static-vmaf-scorer
# plurxd shells out to ffmpeg/ffprobe for scanning, remux, and transcode; TLS
# roots are for TMDB/AniList.
#
# Hardware transcode ships two ways in one image:
#   * jellyfin-ffmpeg (the DEFAULT engine, via PLURX_FFMPEG below) — bundles a
#     CURRENT Intel media driver + libva + oneVPL, so recent GPUs (Arc,
#     Meteor/Arrow Lake on the `xe` driver) that Debian's own driver is years
#     too old for can still do QSV/VAAPI. It's a full ffmpeg, so it also
#     handles scanning and software encode.
#   * the distro ffmpeg + Mesa/Intel VA drivers — the fallback if you override
#     PLURX_FFMPEG back to plain `ffmpeg` (older, widely-tested GPUs).
# Startup validation test-encodes each path, so only what actually works is used.
#
# `apt-get clean` runs between the two ffmpeg installs, not just at the end.
# This layer downloads two independent stacks — the distro ffmpeg and its
# driver set (~150 MB of archives), then jellyfin-ffmpeg — and apt keeps every
# .deb under /var/cache/apt/archives until told otherwise, so the peak is both
# stacks' archives PLUS both unpacked. On a builder with a small disk that
# overflows partway through with an error naming the apt cache rather than the
# real cause. Cleaning between stages keeps the peak to one stack at a time.
#
# The jellyfin-ffmpeg install below pins a reviewed build in major 8, which
# adds the AC-4 decoder required by clear ATSC 3.0 broadcasts. It is then
# ASSERTED to carry both that decoder and `dovi_rpu`, the
# bitstream filter (ffmpeg 7.1+) that removes a Dolby Vision configuration
# from a remux. That is a capability, not a nicety: without it every DV film
# is re-encoded for browsers that cannot decode Dolby Vision (Chrome cannot;
# Safari can), so a 4K disc remux quietly plays at the automatic rung.
# The assertion turns that into a failed build instead of a mystery on
# somebody's television. Debian packages come from one immutable snapshot;
# Jellyfin's separately published deb is verified against its repository's
# SHA-256 metadata for each architecture before apt resolves its dependencies.
#
# The layer is reproducible: two cold builds of one commit on 2026-10-02
# differed only in build-time state, so the end of this RUN removes it. Apt,
# dpkg and alternatives logs are timestamped; ldconfig's aux-cache records
# inode times; fontconfig's caches embed the font directories' build-time
# mtimes. Removing those caches costs nothing measurable: the image carries six
# fonts, which rescan instantly, and each child regenerates its cache under its
# XDG_CACHE_HOME on the writable data volume. (Only an export that rewrites
# file timestamps also makes the baked caches stale; a plain `docker build`
# would have kept them valid.) `useradd` stamps the account's last-change day from
# SOURCE_DATE_EPOCH or the clock, so it is given the Debian snapshot's date,
# which is already this layer's input; the commit's own time is not, because
# declaring it here would rebuild this whole layer on every commit.
# docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md §5.6 has the
# comparison.
RUN sed -i \
        -e 's|http://deb.debian.org/debian-security|http://snapshot.debian.org/archive/debian-security/'"${DEBIAN_SNAPSHOT}"'/|' \
        -e 's|http://deb.debian.org/debian|http://snapshot.debian.org/archive/debian/'"${DEBIAN_SNAPSHOT}"'/|' \
        -e 's/Components: main/Components: main non-free non-free-firmware/' \
        /etc/apt/sources.list.d/debian.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99plurx-snapshot \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential pkg-config nasm curl ca-certificates xz-utils meson ninja-build xxd python3 patch \
        zlib1g-dev libbz2-dev liblzma-dev \
    && static_provider_inputs="" \
    && if [ "$TARGETARCH" = amd64 ]; then static_provider_inputs=/tmp/sealed-parser-provider-inputs; fi \
    && sh /usr/local/libexec/build-static-ffprobe \
        /usr/local/lib/plurx/ffprobe /usr/share/doc/plurx/ffprobe $static_provider_inputs \
    && sh /usr/local/libexec/build-static-vmaf-scorer \
        /usr/local/lib/plurx/vmaf-ffmpeg /usr/share/doc/plurx/vmaf-scorer \
    && apt-get purge -y build-essential pkg-config nasm xz-utils meson ninja-build xxd python3 patch \
        zlib1g-dev libbz2-dev liblzma-dev \
    && apt-get autoremove -y \
    && rm /usr/local/libexec/build-static-ffprobe /usr/local/libexec/build-static-vmaf-scorer /usr/local/libexec/prepare-linux-dolby-ffmpeg \
    && rm /usr/local/libexec/sealed-parser-dependency-offers.py /usr/local/libexec/sealed-parser-dependency-sources.json /usr/local/libexec/prepare-linux-dolby-sdk \
    && rm -rf /usr/local/libexec/linux-video-ffmpeg-patches /tmp/sealed-parser-provider-inputs \
    && apt-get install -y --no-install-recommends \
        ffmpeg ca-certificates mesa-va-drivers libvulkan1 mesa-vulkan-drivers liblcms2-2 libstdc++6 curl \
        "mkvtoolnix=${MKVTOOLNIX_VERSION}" \
    && if [ "$(dpkg --print-architecture)" = "amd64" ]; then \
        apt-get install -y --no-install-recommends \
            intel-media-va-driver-non-free i965-va-driver; \
    fi \
    && apt-get clean \
    && case "$(dpkg --print-architecture)" in \
        amd64) jellyfin_sha=4829b34df16843ecf1aca8b1a600874b5c767396e1d1990c889c761fd143d928 ;; \
        arm64) jellyfin_sha=3497002f1b7a7664875dd0aa71d1916a74cbee6598d8b3b413b83732cd1d8794 ;; \
        *) echo 'FATAL: jellyfin-ffmpeg8 has no pinned artifact for this architecture' >&2; exit 1 ;; \
       esac \
    && jellyfin_deb="jellyfin-ffmpeg8_${JELLYFIN_FFMPEG_VERSION}_$(dpkg --print-architecture).deb" \
    && curl -fsSL "https://repo.jellyfin.org/debian/pool/main/j/jellyfin-ffmpeg/${jellyfin_deb}" \
        -o "/tmp/${jellyfin_deb}" \
    && echo "${jellyfin_sha}  /tmp/${jellyfin_deb}" | sha256sum -c - \
    && apt-get install -y --no-install-recommends "/tmp/${jellyfin_deb}" \
    && rm -f "/tmp/${jellyfin_deb}" \
    && apt-get clean \
    && ( /usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -decoders 2>&1 \
        | grep -Eq '^[[:space:]]*A[^[:space:]]*[[:space:]]+ac4[[:space:]]' \
      || ( echo "FATAL: this jellyfin-ffmpeg8 has no AC-4 decoder." >&2; \
           echo "Clear ATSC 3.0 channels require AC-4 audio decoding." >&2; \
           exit 1 ) ) \
    && ( /usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -bsfs 2>&1 | grep -qx 'dovi_rpu' \
      || ( echo "FATAL: this jellyfin-ffmpeg8 has no dovi_rpu bitstream filter." >&2; \
           echo "Got: $(/usr/lib/jellyfin-ffmpeg/ffmpeg -version 2>&1 | head -1)" >&2; \
           echo "dovi_rpu needs ffmpeg 7.1+; see the note above this RUN." >&2; \
           echo "Review and pin a new media runtime artifact before rebuilding." >&2; \
           exit 1 ) ) \
    && ( /usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -h filter=tonemapx 2>&1 | grep -q '^[[:space:]]*apply_dovi[[:space:]]' \
      || ( echo "FATAL: this jellyfin-ffmpeg8 has no tonemapx apply_dovi renderer." >&2; \
           echo "Profile 5 fallback requires tonemapx with Dolby Vision RPU reshaping." >&2; \
           exit 1 ) ) \
    && dovi_arch="${TARGETARCH:-$(dpkg --print-architecture)}" \
    && case "$dovi_arch" in \
        amd64|x86_64) \
            dovi_target=x86_64-unknown-linux-musl; \
            dovi_sha=5dae82cb2becd3b9fd726127f936a8d32635e60746d16238fdfded12aa05988c ;; \
        arm64|aarch64) \
            dovi_target=aarch64-unknown-linux-musl; \
            dovi_sha=daf538c275f4e702219ce8eb61db28382193ac9d0126e1ef4185a88303af4485 ;; \
        *) echo "FATAL: dovi_tool has no pinned asset for architecture $dovi_arch" >&2; exit 1 ;; \
       esac \
    && dovi_archive="dovi_tool-${DOVI_TOOL_VERSION}-${dovi_target}.tar.gz" \
    && curl -fsSL \
        "https://github.com/quietvoid/dovi_tool/releases/download/${DOVI_TOOL_VERSION}/${dovi_archive}" \
        -o "/tmp/${dovi_archive}" \
    && echo "${dovi_sha}  /tmp/${dovi_archive}" | sha256sum -c - \
    && tar -xzf "/tmp/${dovi_archive}" -C /usr/local/bin ./dovi_tool \
    && chmod 0755 /usr/local/bin/dovi_tool \
    && rm -f "/tmp/${dovi_archive}" \
    && dovi_tool --version | grep -F "${DOVI_TOOL_VERSION}" \
    && mkvmerge --version | grep -F "mkvmerge v74.0.0" \
    && apt-get purge -y curl && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /usr/share/doc/plurx \
    && dpkg-query -W -f='${Package}=${Version}\n' | LC_ALL=C sort \
        > /usr/share/doc/plurx/media-runtime-packages.txt \
    && rm -rf /var/log/apt/* /var/log/*.log /var/cache/ldconfig/aux-cache \
        /var/cache/fontconfig/*.cache-* \
    && groupadd -r plurx \
    && snapshot_day=$(printf '%s' "$DEBIAN_SNAPSHOT" | cut -c1-8) \
    && SOURCE_DATE_EPOCH=$(date -u -d "$snapshot_day" +%s) \
        useradd -r -g plurx -d /var/lib/plurx plurx \
    && mkdir -p /var/lib/plurx \
    && chown plurx:plurx /var/lib/plurx

# Private libplacebo affects only renderer subprocesses through their manifest.
# No Vulkan ICD override: the worker retains normal host/device discovery.
COPY --from=dv-processing-helpers /opt/dv-bundle /usr/lib/plurx/dv-processing

# M5's CI image uses the shipped runtime's media assets, not a second ffmpeg
# install on a persistent runner. These two tool images are pinned by index
# digest just like the release bases; neither builds the release binaries.
FROM rust:1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS ci-rust-toolchain
FROM node:22-bookworm-slim@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c AS ci-node-toolchain

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
        build-essential clang cmake curl git jq lld llvm make nasm ninja-build \
        pkg-config python3 python3-venv \
    && python3 -m venv /opt/playwright-venv \
    && /opt/playwright-venv/bin/pip install --no-cache-dir 'playwright==1.62.0' \
    && /opt/playwright-venv/bin/python3 -m playwright install --with-deps chromium \
    && rustup toolchain install 1.97.1 --profile minimal \
        --component rustfmt --component clippy --component llvm-tools-preview \
    && rustc +1.97.1 --version | grep -F 'rustc 1.97.1' \
    && node --version | grep -E '^v22\.' \
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
ARG PLURX_MEDIA_RUNTIME_DIGEST=""
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

# Coherent amd64 shipping entrypoint: scripts/build-linux-dolby-ffmpeg
# --step docker-build --root CHECKOUT --output AUDITED_PACKAGE --image IMAGE.
# It rechecks actual bytes before passing this context; ARM uses runtime unchanged.
FROM runtime-assets AS linux-dolby-package-build
USER root
ARG TARGETARCH
RUN test "$TARGETARCH" = amd64 \
    && apt-get update && apt-get install -y --no-install-recommends \
        build-essential python3 pkg-config cmake ninja-build nasm patch curl \
        autoconf automake libtool libtool-bin gettext texinfo git bison flex clang \
        zlib1g-dev libbz2-dev liblzma-dev libpng-dev=1.6.39-2+deb12u5 \
    && rm -rf /var/lib/apt/lists/*
COPY scripts/build-linux-dolby-ffmpeg scripts/prepare-linux-dolby-sdk scripts/prepare-linux-dolby-ffmpeg scripts/build-static-ffprobe scripts/linux-video-ffmpeg-sdk-sources.json /opt/plurx-media/scripts/
COPY scripts/linux-video-ffmpeg-patches /opt/plurx-media/scripts/linux-video-ffmpeg-patches
RUN python3 /opt/plurx-media/scripts/build-linux-dolby-ffmpeg --step pipeline \
    --root /work/linux-dolby --jobs 2 --deadline-seconds 600
FROM scratch AS linux-dolby-package-export
COPY --from=linux-dolby-package-build /work/linux-dolby/package /package

FROM runtime-assets AS linux-dolby-install
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY scripts/build-linux-dolby-ffmpeg /usr/local/libexec/build-linux-dolby-ffmpeg
RUN --mount=from=linux-dolby-package,target=/tmp/linux-dolby-package,ro \
    python3 /usr/local/libexec/build-linux-dolby-ffmpeg --step install-shipping \
        --root /usr/lib/jellyfin-ffmpeg --output /tmp/linux-dolby-package \
    && rm /usr/local/libexec/build-linux-dolby-ffmpeg

FROM runtime-assets AS runtime-assets-dolby-amd64
USER root
RUN rm -rf /usr/lib/jellyfin-ffmpeg/lib
COPY --from=linux-dolby-install /usr/lib/jellyfin-ffmpeg /usr/lib/jellyfin-ffmpeg
COPY --from=linux-dolby-install /usr/local/lib/plurx/ffprobe /usr/local/lib/plurx/ffprobe

FROM runtime AS runtime-dolby-amd64
USER root
RUN rm -rf /usr/lib/jellyfin-ffmpeg/lib
COPY --from=linux-dolby-install /usr/lib/jellyfin-ffmpeg /usr/lib/jellyfin-ffmpeg
COPY --from=linux-dolby-install /usr/local/lib/plurx/ffprobe /usr/local/lib/plurx/ffprobe
USER plurx

# Preserve the incumbent default Docker target; make docker explicitly selects amd64 shipping.
FROM runtime AS default-runtime
