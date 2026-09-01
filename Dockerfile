# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS build
# `.git` is not in the build context (see .dockerignore), so the build script
# can't derive the commit itself — CI passes it in, e.g.
#   docker build --build-arg PLURX_BUILD_REF="$(git describe --tags --always --dirty)"
# Left empty, the binary reports its version with build "unknown", which is
# honest about a context that genuinely has no commit in it.
ARG PLURX_BUILD_REF=""
ENV PLURX_BUILD_REF=${PLURX_BUILD_REF}
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target-plurxd \
    --mount=type=cache,target=/src/target-cluster-check \
    ! cargo tree --locked -p plurxd -e features \
        | grep -q 'cluster-read-cost-validation' \
    && CARGO_TARGET_DIR=/src/target-plurxd cargo build --release -p plurxd \
    && cp target-plurxd/release/plurxd /plurxd \
    && CARGO_TARGET_DIR=/src/target-cluster-check cargo build --release -p plurx-cluster-check \
    && cp target-cluster-check/release/plurx-cluster-check /plurx-cluster-check

FROM debian:bookworm-slim AS runtime-assets
ARG TARGETARCH
ARG DOVI_TOOL_VERSION=2.3.3
ARG MKVTOOLNIX_VERSION=74.0.0-1
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
# The jellyfin-ffmpeg install below is deliberately unpinned — it should track
# the current build — but it is then ASSERTED to carry `dovi_rpu`, the
# bitstream filter (ffmpeg 7.1+) that removes a Dolby Vision configuration
# from a remux. That is a capability, not a nicety: without it every DV film
# is re-encoded for browsers that cannot decode Dolby Vision (Chrome cannot;
# Safari can), so a 4K disc remux quietly plays at the automatic rung. Because
# the install is unpinned, WHICH ffmpeg lands here depends on the day the image
# was built, and a stale build loses the capability with no visible symptom.
# The assertion turns that into a failed build instead of a mystery on
# somebody's television.
RUN sed -i 's/Components: main/Components: main non-free non-free-firmware/' \
        /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        ffmpeg ca-certificates mesa-va-drivers curl gnupg \
        "mkvtoolnix=${MKVTOOLNIX_VERSION}" \
    && if [ "$(dpkg --print-architecture)" = "amd64" ]; then \
        apt-get install -y --no-install-recommends \
            intel-media-va-driver-non-free i965-va-driver; \
    fi \
    && apt-get clean \
    && install -d /etc/apt/keyrings \
    && curl -fsSL https://repo.jellyfin.org/jellyfin_team.gpg.key \
        | gpg --dearmor -o /etc/apt/keyrings/jellyfin.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/jellyfin.gpg] https://repo.jellyfin.org/debian bookworm main" \
        > /etc/apt/sources.list.d/jellyfin.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends jellyfin-ffmpeg7 \
    && apt-get clean \
    && ( /usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -bsfs 2>&1 | grep -qx 'dovi_rpu' \
      || ( echo "FATAL: this jellyfin-ffmpeg7 has no dovi_rpu bitstream filter." >&2; \
           echo "Got: $(/usr/lib/jellyfin-ffmpeg/ffmpeg -version 2>&1 | head -1)" >&2; \
           echo "dovi_rpu needs ffmpeg 7.1+; see the note above this RUN." >&2; \
           echo "Rebuild fetching current packages: docker build --no-cache --pull" >&2; \
           exit 1 ) ) \
    && ( /usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -h filter=tonemapx 2>&1 | grep -q '^[[:space:]]*apply_dovi[[:space:]]' \
      || ( echo "FATAL: this jellyfin-ffmpeg7 has no tonemapx apply_dovi renderer." >&2; \
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
    && apt-get purge -y curl gnupg && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r plurx \
    && useradd -r -g plurx -d /var/lib/plurx plurx \
    && mkdir -p /var/lib/plurx \
    && chown plurx:plurx /var/lib/plurx

# Keep the expensive, architecture-specific runtime asset assertions available
# as their own CI target. The native release-build matrix already proves both
# Rust binaries for arm64; rebuilding the entire Rust graph under QEMU made a
# cold qualification exceed the Docker job's one-hour bound before these
# runtime assertions could report a result.
FROM runtime-assets AS runtime
COPY --from=build /plurxd /usr/local/bin/plurxd
# Stopped-node recovery and cluster validation tooling. The WAL inspector is
# read-only, refuses a live lock, and lets an operator diagnose the same image
# that produced the on-disk state without installing Rust on the host.
COPY --from=build /plurx-cluster-check /usr/local/bin/plurx-cluster-check

# Default to jellyfin-ffmpeg (recent GPUs need its driver stack); override
# either var to point elsewhere. It's a superset of system ffmpeg, so this is
# safe on hardware that the distro build would also handle.
ENV PLURX_BIND=0.0.0.0:32400 \
    PLURX_DATA_DIR=/var/lib/plurx \
    PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg \
    PLURX_FFPROBE=/usr/lib/jellyfin-ffmpeg/ffprobe \
    PLURX_DOVI_TOOL=/usr/local/bin/dovi_tool \
    PLURX_MKVMERGE=/usr/bin/mkvmerge

EXPOSE 32400
VOLUME ["/var/lib/plurx"]
USER plurx

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD ["plurxd", "healthcheck"]

ENTRYPOINT ["plurxd"]
CMD ["run"]
