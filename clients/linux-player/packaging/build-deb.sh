#!/usr/bin/env bash
# Build plurx-player_<version>_amd64.deb (launcher + private mpv/libplacebo)
# in a disposable Ubuntu 26.10 container, then install and check it in a
# second fresh one. Nothing is installed on the host: both containers run
# with --rm from the stock image, the source is mounted read-only, and only
# the output directory is written.
#
#   clients/linux-player/packaging/build-deb.sh [--out DIR] [--no-verify]
#
# Output (default target/linux-player-deb/ at the repository root):
#   plurx-player_<version>_amd64.deb, its .buildinfo (the exact build
#   environment), SHA256SUMS, build.log and verify.log.
#
# Environment: PLURX_DEB_OUT (output directory), PLURX_DEB_JOBS (parallel
# jobs, default 8), PLURX_DEB_IMAGE (default the pinned ubuntu:26.10 below),
# DOCKER (default: docker, or "sudo -n docker" when only that works).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
player=$(dirname "$here")
repo=$(cd "$player/../.." && pwd)
out=${PLURX_DEB_OUT:-$repo/target/linux-player-deb}
verify=1
while [ $# -gt 0 ]; do
  case $1 in
    --out) out=$2; shift 2 ;;
    --no-verify) verify=0; shift ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "build-deb.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
# ubuntu:26.10 as of 2026-10-25. Bump deliberately; the .buildinfo records
# the package versions the build actually used.
image=${PLURX_DEB_IMAGE:-ubuntu:26.10@sha256:93e29beff5257b2cdf1c024d831f6d2997e53655a653ff76a811fc9e6564a719}
jobs=${PLURX_DEB_JOBS:-8}
if [ -z "${DOCKER:-}" ]; then
  if docker info >/dev/null 2>&1; then DOCKER=docker
  elif sudo -n docker info >/dev/null 2>&1; then DOCKER="sudo -n docker"
  else echo "build-deb.sh: docker is not usable (tried docker and sudo -n docker)" >&2; exit 1
  fi
fi

mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -f "$out"/plurx-player_*_amd64.deb "$out"/plurx-player_*_amd64.buildinfo \
      "$out/SHA256SUMS" "$out/build.log" "$out/verify.log"

run() {
  local name=$1; shift
  $DOCKER run --rm --init --name "$name-$$" --cpus="$jobs" \
    -v "$player:/src:ro" "$@"
}

start=$(date +%s)
echo "build-deb.sh: building in $image with $jobs jobs; log in $out/build.log"
run plurx-player-deb-build -v "$out:/out" -e JOBS="$jobs" \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  "$image" bash /src/packaging/build-in-container.sh > "$out/build.log" 2>&1 \
  || { tail -40 "$out/build.log"; echo "build-deb.sh: build failed; see $out/build.log" >&2; exit 1; }
built=$(date +%s)
echo "build-deb.sh: built in $((built - start)) s"
cat "$out/SHA256SUMS"

if [ "$verify" = 1 ]; then
  echo "build-deb.sh: verifying in a fresh container; log in $out/verify.log"
  run plurx-player-deb-verify -v "$out:/out:ro" "$image" \
    bash /src/packaging/verify-in-container.sh > "$out/verify.log" 2>&1 \
    || { tail -40 "$out/verify.log"; echo "build-deb.sh: verification failed; see $out/verify.log" >&2; exit 1; }
  grep -E '^(PASS|FAIL):|^W:' "$out/verify.log"
  echo "build-deb.sh: verified in $(( $(date +%s) - built )) s"
fi
ls -l "$out"/plurx-player_*_amd64.deb
