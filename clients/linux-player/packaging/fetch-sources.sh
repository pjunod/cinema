#!/usr/bin/env bash
# Make the reproducible launcher orig tarball for a source-only Cinema package.
# Historical renderer patches and their CPU controls remain in the source;
# no mpv/libplacebo source components are downloaded, built or installed.
set -euo pipefail

player=$(cd "$1" && pwd)
parent=$(dirname "$player")
base=$(basename "$player")
fail() { echo "fetch-sources: $*" >&2; exit 1; }

version=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Version)
source=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Source)
epoch=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Timestamp)
upstream=${version%-*}
[ "$upstream" != "$version" ] || fail "debian/changelog version $version has no Debian revision"

for header in dm_closed_form.h dv_dm.h; do
  [ -f "$player/dm/$header" ] || fail "$player/dm/$header is missing (build-in-container.sh copies it from tools/dv_processing)"
done
rm -rf "$player/libplacebo" "$player/mpv"
tar -C "$parent" --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
  --mode=u+rwX,go+rX,go-w --format=gnu \
  --exclude="$base/debian" --exclude=__pycache__ \
  --transform="s|^$base|$source-$upstream|" -cf - "$base" \
  | gzip -n -9 > "$parent/${source}_$upstream.orig.tar.gz"
echo "fetch-sources: ${source}_$upstream.orig.tar.gz (the launcher)"
