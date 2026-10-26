#!/usr/bin/env bash
# Fetch the sources pinned in packaging/upstream.lock, check their sha256, and
# apply the ordered patch series in packaging/patches/<name>/*.patch.
#
#   packaging/prepare-sources.sh <clients/linux-player>
#
# Each source becomes a git repository in upstream/<name>. Its base commit
# carries the annotated tag v<version>-plurx, so `git describe` names this
# private build: libplacebo's pl_version() (which mpv prints as "libplacebo
# version") and mpv's own version both show v<version>-plurx with no patches
# and v<version>-plurx-<n>-g<hash> with n patches. The commits use the
# changelog's date, so the same inputs give the same hashes.
#
# A patch that starts with "From " (git format-patch) is applied with git am;
# anything else with patch -p1 and committed under its file name. Files apply
# in C-locale name order, so name them 0001-..., 0002-...
set -euo pipefail

player=$(cd "$1" && pwd)
lock=$player/packaging/upstream.lock
dest=$player/upstream

epoch=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Timestamp)
version=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Version)
export GIT_AUTHOR_NAME="plurx-player build" GIT_AUTHOR_EMAIL=noreply@plurx.invalid
export GIT_COMMITTER_NAME="plurx-player build" GIT_COMMITTER_EMAIL=noreply@plurx.invalid
export GIT_AUTHOR_DATE="@$epoch +0000" GIT_COMMITTER_DATE="@$epoch +0000"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null

declare -A pinned
mkdir -p "$dest/downloads"
while read -r name ver sha url; do
  case $name in ''|'#'*) continue ;; esac
  pinned[$name]=$ver
  file=$dest/downloads/$name-$ver.tar.gz
  if ! echo "$sha  $file" | sha256sum -c --status 2>/dev/null; then
    curl -fsSL --retry 3 -o "$file" "$url"
  fi
  if ! echo "$sha  $file" | sha256sum -c --status; then
    echo "prepare-sources: sha256 of $url is $(sha256sum < "$file" | cut -d' ' -f1), pinned $sha" >&2
    exit 1
  fi
  src=$dest/$name
  rm -rf "$src"
  mkdir -p "$src"
  tar -xzf "$file" -C "$src" --strip-components=1
  git -C "$src" init -q
  git -C "$src" add -A
  git -C "$src" commit -q -m "$name $ver from $url (sha256 $sha)"
  git -C "$src" tag -a -m "plurx-player private build of $name $ver" "v$ver-plurx"
  count=0
  while IFS= read -r patch; do
    [ -n "$patch" ] || continue
    if head -n1 "$patch" | grep -q '^From '; then
      git -C "$src" am -q --3way "$patch"
    else
      patch -d "$src" -p1 --forward --no-backup-if-mismatch -s -i "$patch"
      git -C "$src" add -A
      git -C "$src" commit -q -m "$(basename "$patch")"
    fi
    echo "prepare-sources: applied $name/$(basename "$patch")"
    count=$((count + 1))
  done < <(find "$player/packaging/patches/$name" -maxdepth 1 -type f -name '*.patch' | LC_ALL=C sort)
  echo "prepare-sources: $name $ver, sha256 ok, $count patch(es), $(git -C "$src" describe)"
done < "$lock"

for name in libplacebo mpv; do
  [ -n "${pinned[$name]:-}" ] || { echo "prepare-sources: $name is not pinned in $lock" >&2; exit 1; }
done
want="+lp${pinned[libplacebo]}.mpv${pinned[mpv]}"
case $version in
  *"$want") ;;
  *) echo "prepare-sources: debian/changelog version $version does not end in $want (packaging/upstream.lock)" >&2; exit 1 ;;
esac
