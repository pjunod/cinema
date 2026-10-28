#!/usr/bin/env bash
# Fetch the sources pinned in packaging/upstream.lock, check their sha256, and
# lay out the Debian source package: the launcher as the main orig tarball,
# libplacebo and mpv as component orig tarballs (byte-for-byte upstream's),
# and their unpacked trees in <player>/libplacebo and <player>/mpv, which is
# where dpkg-source expects components.
#
#   packaging/fetch-sources.sh <clients/linux-player>
#
# The tarballs are written next to <player>. The main orig is made
# reproducibly (sorted names, the changelog's date, owner 0, normalised
# modes), so the same tree gives the same bytes. The patch series is applied
# later, offline, by build-private.sh during the package build.
set -euo pipefail

player=$(cd "$1" && pwd)
parent=$(dirname "$player")
base=$(basename "$player")
lock=$player/packaging/upstream.lock
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

declare -A pinned
mkdir -p "$parent/downloads"
while read -r name ver sha url; do
  case $name in ''|'#'*|apt-snapshot) continue ;; esac
  pinned[$name]=$ver
  file=$parent/downloads/$name-$ver.tar.gz
  if ! echo "$sha  $file" | sha256sum -c --status 2>/dev/null; then
    curl -fsSL --retry 3 -o "$file" "$url"
  fi
  echo "$sha  $file" | sha256sum -c --status \
    || fail "sha256 of $url is $(sha256sum < "$file" | cut -d' ' -f1), pinned $sha"
  cp "$file" "$parent/${source}_$upstream.orig-$name.tar.gz"
  mkdir -p "$player/$name"
  tar -xzf "$file" -C "$player/$name" --strip-components=1
  echo "fetch-sources: ${source}_$upstream.orig-$name.tar.gz ($name $ver, sha256 ok)"
done < "$lock"

for name in libplacebo mpv; do
  [ -n "${pinned[$name]:-}" ] || fail "$name is not pinned in $lock"
done
want="+lp${pinned[libplacebo]}.mpv${pinned[mpv]}"
case $upstream in
  *"$want") ;;
  *) fail "debian/changelog upstream version $upstream does not end in $want (packaging/upstream.lock)" ;;
esac
