#!/usr/bin/env bash
# Build plurx-player_<version>_amd64.deb (launcher + private mpv/libplacebo)
# and its Debian source package in a disposable Ubuntu 26.10 container,
# verify it in a second fresh one, then build it again and check the two
# builds are byte-identical. Nothing is installed on the host: every
# container runs with --rm from the stock image, the repository is mounted
# read-only, and only the output directory is written.
#
#   clients/linux-player/packaging/build-deb.sh [--out DIR] [--no-verify] [--no-rebuild-check]
#
# Output (default target/linux-player-deb/ at the repository root):
#   plurx-player_<version>_amd64.deb; the source package (.dsc, the launcher
#   .orig.tar.gz, .orig-libplacebo.tar.gz, .orig-mpv.tar.gz, .debian.tar.xz);
#   SHA256SUMS over all of those; the .buildinfo (the exact build
#   environment); build.log, verify.log and rebuild.log.
#
# Environment: PLURX_DEB_OUT (output directory), PLURX_DEB_JOBS (parallel
# jobs, default 8), PLURX_DEB_IMAGE (default the pinned ubuntu:26.10 below),
# PLURX_DEB_APT_CACHE (apt's indexes and packages for the pinned snapshot,
# shared by every container; default target/linux-player-apt-cache at the
# repository root; see apt-snapshot.sh), DOCKER (default: docker, or
# "sudo -n docker" when only that works).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
player=$(dirname "$here")
repo=$(cd "$player/../.." && pwd)
out=${PLURX_DEB_OUT:-$repo/target/linux-player-deb}
verify=1
rebuild=1
while [ $# -gt 0 ]; do
  case $1 in
    --out) out=$2; shift 2 ;;
    --no-verify) verify=0; shift ;;
    --no-rebuild-check) rebuild=0; shift ;;
    -h|--help) sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "build-deb.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
# The stock image's starting point; packaging/upstream.lock pins the apt
# snapshot everything is installed from.
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
apt_cache=${PLURX_DEB_APT_CACHE:-$repo/target/linux-player-apt-cache}
mkdir -p "$apt_cache"
apt_cache=$(cd "$apt_cache" && pwd)
# apt does not wait for its locks: a second build on the same cache would
# fail half way. Refuse it up front instead.
exec 9>"$apt_cache/.build.lock"
flock -n 9 || { echo "build-deb.sh: another build is using $apt_cache" >&2; exit 1; }
rm -rf "$out"/plurx-player_* "$out/SHA256SUMS" "$out"/*.log "$out/rebuild"

run() {
  local name=$1; shift
  $DOCKER run --rm --init --name "$name-$$" --cpus="$jobs" -v "$repo:/repo:ro" \
    -v "$apt_cache:/apt-cache" "$@"
}
# The containers run as root; hand the cache back so it can be removed.
trap '$DOCKER run --rm -v "$apt_cache:/apt-cache" "$image" chown -R "$(id -u):$(id -g)" /apt-cache >/dev/null 2>&1 || true' EXIT
build() {
  local dir=$1 log=$2
  run plurx-player-deb-build -v "$dir:/out" -e JOBS="$jobs" \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    "$image" bash /repo/clients/linux-player/packaging/build-in-container.sh > "$log" 2>&1 \
    || { tail -40 "$log"; echo "build-deb.sh: build failed; see $log" >&2; exit 1; }
}

start=$(date +%s)
echo "build-deb.sh: building in $image with $jobs jobs; log in $out/build.log"
build "$out" "$out/build.log"
built=$(date +%s)
echo "build-deb.sh: built in $((built - start)) s"
cat "$out/SHA256SUMS"

if [ "$verify" = 1 ]; then
  # Self-test first: a run whose checksum check is fed a wrong digest must
  # fail, or the verification below proves nothing.
  if run plurx-player-deb-selftest -v "$out:/out:ro" -e PLURX_VERIFY_SELFTEST=1 "$image" \
       bash /repo/clients/linux-player/packaging/verify-in-container.sh > "$out/selftest.log" 2>&1; then
    cat "$out/selftest.log"; echo "build-deb.sh: the verification self-test passed a wrong checksum" >&2; exit 1
  fi
  grep -q '^FAIL: checksums' "$out/selftest.log" && ! grep -q 'ALL CHECKS PASSED' "$out/selftest.log" \
    || { cat "$out/selftest.log"; echo "build-deb.sh: the verification self-test did not fail at its checksum check" >&2; exit 1; }
  echo "build-deb.sh: self-test: a forced checksum failure fails the verification"
  rm -f "$out/selftest.log"

  echo "build-deb.sh: verifying in a fresh container; log in $out/verify.log"
  run plurx-player-deb-verify -v "$out:/out:ro" "$image" \
    bash /repo/clients/linux-player/packaging/verify-in-container.sh > "$out/verify.log" 2>&1 \
    || { tail -40 "$out/verify.log"; echo "build-deb.sh: verification failed; see $out/verify.log" >&2; exit 1; }
  grep -E '^(PASS|FAIL):|^W:' "$out/verify.log"
  echo "build-deb.sh: verified in $(( $(date +%s) - built )) s"
fi

if [ "$rebuild" = 1 ]; then
  echo "build-deb.sh: building again to compare; log in $out/rebuild.log"
  mkdir -p "$out/rebuild"
  build "$out/rebuild" "$out/rebuild.log"
  if cmp -s "$out/SHA256SUMS" "$out/rebuild/SHA256SUMS"; then
    echo "build-deb.sh: the rebuild is byte-identical ($(wc -l < "$out/SHA256SUMS") files)"
    rm -rf "$out/rebuild"
  else
    diff "$out/SHA256SUMS" "$out/rebuild/SHA256SUMS" || true
    echo "build-deb.sh: the rebuild differs; both kept ($out and $out/rebuild)" >&2
    exit 1
  fi
fi
ls -l "$out"/plurx-player_*
