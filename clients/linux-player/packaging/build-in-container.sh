#!/usr/bin/env bash
# Inside the disposable build container (packaging/build-deb.sh starts it,
# with the repository read-only at /repo and the output directory at /out):
#
# 1. point apt at the pinned snapshot (packaging/apt-snapshot.sh);
# 2. copy clients/linux-player to the fixed path /build/plurx-player and make
#    the Debian source package from it (packaging/fetch-sources.sh, then
#    dpkg-source -b): .dsc, the launcher orig, the libplacebo and mpv
#    component origs and the debian tarball;
# 3. unpack that source package afresh in the same path and build the .deb
#    from it, so the published source is exactly what the binary was built
#    from.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8
jobs=${JOBS:-8}
player=/build/plurx-player

bash /repo/clients/linux-player/packaging/apt-snapshot.sh /repo/clients/linux-player/packaging/upstream.lock
apt-get install -y -q --no-install-recommends curl dpkg-dev git

rm -rf /build
mkdir -p /build
cp -a /repo/clients/linux-player "$player"
rm -rf "$player/upstream" "$player/build" "$player/libplacebo" "$player/mpv" "$player/dm" \
       "$player/debian/plurx-player" "$player/debian/.debhelper" "$player/debian/files"
find "$player" -name __pycache__ -prune -exec rm -rf {} +
chmod -R u+rwX,go+rX,go-w "$player"

# plurx's closed-form display management, the same files the server's
# processing helper compiles; they travel in the launcher orig tarball, so
# the source package rebuilds without the plurx tree.
mkdir -p "$player/dm"
cp /repo/tools/dv_processing/dm_closed_form.h /repo/tools/dv_processing/dv_dm.h "$player/dm/"

"$player/packaging/fetch-sources.sh" "$player"
apt-get build-dep -y -q --no-install-recommends "$player"
version=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Version)
upstream=${version%-*}
cd /build
dpkg-source -b plurx-player

rm -rf "$player"
dpkg-source -x "plurx-player_$version.dsc" "$player"
cd "$player"
DEB_BUILD_OPTIONS="parallel=$jobs noddebs" dpkg-buildpackage -b -us -uc

cd /build
artifacts=("plurx-player_${version}_amd64.deb" "plurx-player_$version.dsc"
           "plurx-player_$upstream.orig.tar.gz" "plurx-player_$upstream.orig-libplacebo.tar.gz"
           "plurx-player_$upstream.orig-mpv.tar.gz" "plurx-player_$version.debian.tar.xz")
cp "${artifacts[@]}" "plurx-player_${version}_amd64.buildinfo" /out/
cd /out
sha256sum "${artifacts[@]}" > SHA256SUMS
cat SHA256SUMS
dpkg-deb --info "plurx-player_${version}_amd64.deb"
dpkg-deb --contents "plurx-player_${version}_amd64.deb"
chown "${HOST_UID:-0}:${HOST_GID:-0}" "${artifacts[@]}" "plurx-player_${version}_amd64.buildinfo" SHA256SUMS
