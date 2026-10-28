#!/usr/bin/env bash
# Turn an unpacked upstream tree into a git repository and apply its patch
# series exactly.
#
#   packaging/apply-series.sh <tree> <patch-dir> <upstream-version> <epoch>
#
# The base commit carries the annotated tag v<version>-plurx, so `git
# describe` names the private build: libplacebo's pl_version() (which mpv
# prints as "libplacebo version") and mpv's own version show v<version>-plurx
# with no patches and v<version>-plurx-<n>-g<hash> with n. Commits use the
# given epoch and a fixed identity, so the same inputs give the same hashes.
#
# <patch-dir>/series lists the patches in order, one file name per line
# (# starts a comment). It must name every *.patch in the directory and
# nothing else. A git format-patch file (first line "From ") is applied
# with git am, anything else with git apply; neither accepts fuzz or a
# three-way merge, so a patch whose context does not match fails the build.
set -euo pipefail

tree=$1 dir=$2 version=$3 epoch=$4
fail() { echo "apply-series: $*" >&2; exit 1; }

export GIT_AUTHOR_NAME="plurx-player build" GIT_AUTHOR_EMAIL=noreply@plurx.invalid
export GIT_COMMITTER_NAME="plurx-player build" GIT_COMMITTER_EMAIL=noreply@plurx.invalid
export GIT_AUTHOR_DATE="@$epoch +0000" GIT_COMMITTER_DATE="@$epoch +0000"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null

[ -f "$dir/series" ] || fail "$dir/series is missing"
mapfile -t listed < <(sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e 's/^[[:space:]]*//' -e '/^$/d' "$dir/series")
for name in "${listed[@]}"; do
  case $name in */*|*[[:space:]]*) fail "series entry '$name' is not a plain file name" ;; *.patch) ;; *) fail "series entry '$name' is not a .patch" ;; esac
done
duplicates=$(printf '%s\n' "${listed[@]}" | sed '/^$/d' | LC_ALL=C sort | uniq -d)
[ -z "$duplicates" ] || fail "series lists twice: $duplicates"
want=$(printf '%s\n' "${listed[@]}" | sed '/^$/d' | LC_ALL=C sort)
have=$(find "$dir" -maxdepth 1 -type f -name '*.patch' -printf '%f\n' | LC_ALL=C sort)
if [ "$want" != "$have" ]; then
  echo "apply-series: $dir/series and the directory disagree" >&2
  diff <(echo "$want") <(echo "$have") | sed -n 's/^< /  listed, missing: /p; s/^> /  present, unlisted: /p' >&2
  exit 1
fi

git -C "$tree" init -q
git -C "$tree" add -A
git -C "$tree" commit -q -m "upstream $version"
git -C "$tree" tag -a -m "plurx-player private build of $version" "v$version-plurx"
for name in "${listed[@]}"; do
  patch=$dir/$name
  if head -n1 "$patch" | grep -q '^From '; then
    git -C "$tree" am -q "$patch" || fail "$name does not apply exactly"
  else
    git -C "$tree" apply --index "$patch" || fail "$name does not apply exactly"
    git -C "$tree" commit -q -m "$name"
  fi
  echo "apply-series: applied $name"
done
echo "apply-series: ${#listed[@]} patch(es), $(git -C "$tree" describe)"
