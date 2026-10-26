#!/usr/bin/env bash
# Inside the disposable build container (packaging/build-deb.sh starts it):
# install the Build-Depends, fetch and patch the pinned sources, and run
# dpkg-buildpackage on a copy of the read-only /src. Results go to /out.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8
jobs=${JOBS:-8}

apt-get update -q
apt-get install -y -q --no-install-recommends ca-certificates curl dpkg-dev git patch

work=/build/plurx-player
mkdir -p /build
cp -a /src "$work"
rm -rf "$work/upstream" "$work/build" "$work/debian/plurx-player" "$work/debian/.debhelper"
find "$work" -name __pycache__ -prune -exec rm -rf {} +

apt-get build-dep -y -q --no-install-recommends "$work"
"$work/packaging/prepare-sources.sh" "$work"

cd "$work"
DEB_BUILD_OPTIONS="parallel=$jobs noddebs" dpkg-buildpackage -b -us -uc

cd /build
deb=$(ls plurx-player_*_amd64.deb)
cp "$deb" plurx-player_*_amd64.buildinfo /out/
cd /out
sha256sum "$deb" > SHA256SUMS
dpkg-deb --info "$deb"
dpkg-deb --contents "$deb"
chown "${HOST_UID:-0}:${HOST_GID:-0}" "$deb" plurx-player_*_amd64.buildinfo SHA256SUMS
