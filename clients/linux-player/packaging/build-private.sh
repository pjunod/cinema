#!/usr/bin/env bash
# Build the private libplacebo and mpv for plurx-player. debian/rules calls
# this from the unpacked source package, whose libplacebo/ and mpv/
# components are the pinned upstream trees (packaging/fetch-sources.sh).
#
#   JOBS=8 packaging/build-private.sh <clients/linux-player>
#
# Each component is copied to build/src/<name>, made a git repository and
# patched with its series (packaging/apply-series.sh). Both are configured
# for prefix /usr/lib/plurx-player and staged under
# build/stage/usr/lib/plurx-player.
#
# libplacebo is built as a static library, its global symbols renamed
# pl_* -> plurx_pl_*, and linked into mpv with its symbols hidden
# (-Wl,--exclude-libs,ALL). The system FFmpeg's libavfilter loads the system
# libplacebo (another major version) into the same process, and libplacebo's
# symbols are unversioned: a private libplacebo.so would take over every
# pl_* call libavfilter makes, with the other version's ABI. Renamed and
# hidden, the private copy is reachable only from mpv and the system one
# only from libavfilter (the linker refuses to hide a symbol a linked
# library references, so hiding alone cannot work), and there is no private
# shared object for LD_LIBRARY_PATH or the loader to swap. mpv sees the new
# names through the staged libplacebo/config.h, which every libplacebo
# header includes.
#
# Options follow Ubuntu 26.10's own builds where they matter for this player:
# libplacebo with Vulkan, OpenGL, glslang (not shaderc), lcms2, xxhash and the
# built-in Dolby Vision RPU support; mpv with GPL, the Wayland, X11 and DRM
# outputs, Vulkan, VAAPI and CUDA hwdec, libavcodec from the system FFmpeg,
# Lua 5.2 (for mpv's built-in on-screen controller; the launcher itself only
# uses the JSON IPC), lcms2, zimg and PipeWire/PulseAudio/ALSA. Disc,
# capture, scripting-language and exotic outputs Ubuntu enables are left out.
# CFLAGS/LDFLAGS come from dpkg-buildflags (hardening=+all, as Ubuntu).
set -euo pipefail

player=$(cd "$1" && pwd)
out=$player/build
src=$out/src
stage=$out/stage
prefix=/usr/lib/plurx-player
jobs=${JOBS:-1}
lock=$player/packaging/upstream.lock
epoch=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Timestamp)
version=$(dpkg-parsechangelog -l "$player/debian/changelog" -S Version)
tests=true
case " ${DEB_BUILD_OPTIONS:-} " in *" nocheck "*) tests=false ;; esac
fail() { echo "build-private: $*" >&2; exit 1; }

declare -A pinned
while read -r name ver _; do
  case $name in ''|'#'*|apt-snapshot) continue ;; esac
  pinned[$name]=$ver
done < "$lock"
rm -rf "$out"
mkdir -p "$src"
for name in libplacebo mpv; do
  [ -f "$player/$name/meson.build" ] || fail "$player/$name is missing; run packaging/build-deb.sh, or packaging/fetch-sources.sh first"
  cp -a "$player/$name" "$src/$name"
  "$player/packaging/apply-series.sh" "$src/$name" "$player/packaging/patches/$name" "${pinned[$name]}" "$epoch"
done

# The mpv series compiles against plurx's closed-form display management
# (dm/, from tools/dv_processing) and the pinned libdovi C API header:
# Ubuntu's libdovi-dev ships an empty rpu_parser.h, so only its library is used.
headers=$src/mpv/video/out/placebo/plurx
mkdir -p "$headers"
for file in dm/dm_closed_form.h dm/dv_dm.h packaging/libdovi/rpu_parser.h; do
  [ -f "$player/$file" ] || fail "$player/$file is missing"
  cp "$player/$file" "$headers/"
done

# Without LTO: objcopy renames symbols in object code, not in LTO bytecode
# (Ubuntu's build flags turn LTO on).
CFLAGS="${CFLAGS:-} -fno-lto" CXXFLAGS="${CXXFLAGS:-} -fno-lto" LDFLAGS="${LDFLAGS:-} -fno-lto" \
meson setup "$out/libplacebo" "$src/libplacebo" \
  --prefix="$prefix" --libdir=lib --buildtype=plain --wrap-mode=nodownload \
  -Dvulkan=enabled -Dopengl=enabled -Dglslang=enabled -Dshaderc=disabled \
  -Dlcms=enabled -Dxxhash=enabled -Ddovi=enabled -Dlibdovi=disabled \
  -Dd3d11=disabled -Ddemos=false -Dtests="$tests" -Dbench=false -Dfuzz=false \
  -Ddefault_library=static
ninja -C "$out/libplacebo" -j "$jobs"
DESTDIR="$stage" meson install -C "$out/libplacebo" --no-rebuild --quiet

archive=$stage$prefix/lib/libplacebo.a
config_h=$stage$prefix/include/libplacebo/config.h
[ -f "$archive" ] && ! ls "$stage$prefix"/lib/libplacebo.so* 2> /dev/null \
  || fail "libplacebo was not built as a static library only"
nm -g --defined-only "$archive" | awk 'NF == 3 && $3 ~ /^pl_/ { print $3 }' | LC_ALL=C sort -u > "$out/libplacebo.symbols"
[ -s "$out/libplacebo.symbols" ] || fail "libplacebo.a defines no pl_ symbol"
macros=$(grep -rhE '^[[:space:]]*#[[:space:]]*define[[:space:]]+pl_' "$stage$prefix/include/libplacebo" \
  | sed -E 's/^[[:space:]]*#[[:space:]]*define[[:space:]]+(pl_[A-Za-z0-9_]+).*/\1/' | LC_ALL=C sort -u)
clash=$(LC_ALL=C comm -12 <(echo "$macros") "$out/libplacebo.symbols")
[ -z "$clash" ] || fail "libplacebo headers define macros named like its symbols: $clash"
awk '{ print $1, "plurx_" $1 }' "$out/libplacebo.symbols" > "$out/libplacebo.rename"
objcopy --redefine-syms="$out/libplacebo.rename" "$archive"
left=$(nm -g --defined-only "$archive" | awk 'NF == 3 && $3 ~ /^pl_/ { print $3 }' | head -n 3)
[ -z "$left" ] || fail "libplacebo.a still defines pl_ symbols: $left"
lto=$(nm "$archive" 2> /dev/null | grep -c __gnu_lto || true)
[ "$lto" = 0 ] || fail "libplacebo.a carries LTO bytecode, which objcopy cannot rename"
# First in config.h, before its own declarations (pl_version).
{
  echo "/* plurx-player: this private libplacebo's global symbols are renamed"
  echo "   pl_* -> plurx_pl_* (packaging/build-private.sh). */"
  echo "#ifndef PLURX_LIBPLACEBO_RENAMED"
  echo "#define PLURX_LIBPLACEBO_RENAMED"
  awk '{ print "#define", $1, "plurx_" $1 }' "$out/libplacebo.symbols"
  echo "#endif"
  echo
  cat "$config_h"
} > "$out/config.h"
mv "$out/config.h" "$config_h"

# The staged .pc, with its prefix moved into the stage and the archive named
# by path instead of -L/-l (so meson adds no build-directory rpath). A
# static-only meson build already lists glslang, SPIRV-Tools and the rest in
# Libs and Requires (the build fails if that ever changes), but not the C++
# runtime glslang needs, which is added. The system
# libplacebo-dev is not installed in the build container, and the check
# below fails the build if pkg-config found anything else.
mkdir -p "$out/pkgconfig"
pc=$stage$prefix/lib/pkgconfig/libplacebo.pc
! grep -q -E '^(Libs|Requires)\.private:' "$pc" || fail "libplacebo.pc has private dependencies a static link would miss"
grep -q '^Libs: -L\${libdir} -lplacebo ' "$pc" || fail "libplacebo.pc Libs does not start with -L\${libdir} -lplacebo"
sed -e "s|^prefix=.*|prefix=$stage$prefix|" -e 's|^Libs: -L\${libdir} -lplacebo |Libs: ${libdir}/libplacebo.a |' \
  -e '/^Libs:/{/-lstdc++/!s/$/ -lstdc++/}' "$pc" > "$out/pkgconfig/libplacebo.pc"
export PKG_CONFIG_PATH="$out/pkgconfig"
[ "$(pkg-config --variable=libdir libplacebo)" = "$stage$prefix/lib" ] \
  || fail "pkg-config resolves libplacebo outside the stage"

LDFLAGS="${LDFLAGS:-} -Wl,--exclude-libs,ALL" \
meson setup "$out/mpv" "$src/mpv" \
  --prefix="$prefix" --libdir=lib --buildtype=plain --wrap-mode=nodownload \
  -Dgpl=true -Dcplayer=true -Dlibmpv=false -Dbuild-date=false -Dtests=false -Dfuzzers=false \
  -Dvulkan=enabled -Dwayland=enabled -Degl-wayland=enabled -Ddmabuf-wayland=enabled \
  -Dx11=enabled -Degl-x11=enabled -Ddrm=enabled -Dgbm=enabled -Degl-drm=enabled \
  -Dvaapi=enabled -Dvaapi-wayland=enabled -Dvaapi-x11=enabled -Dvaapi-drm=enabled \
  -Dcuda-hwaccel=enabled -Dcuda-interop=enabled \
  -Dlua=lua5.2 -Dlcms2=enabled -Dzimg=enabled -Djpeg=enabled -Duchardet=enabled \
  -Diconv=enabled -Dzlib=enabled -Dpipewire=enabled -Dpulse=enabled -Dalsa=enabled \
  -Djavascript=disabled -Dlibarchive=disabled -Dlibbluray=disabled -Ddvdnav=disabled \
  -Dcdda=disabled -Ddvbin=disabled -Dlibavdevice=disabled -Drubberband=disabled \
  -Dvapoursynth=disabled -Dsdl2-audio=disabled -Dsdl2-video=disabled \
  -Dsdl2-gamepad=disabled -Djack=disabled -Dopenal=disabled -Dsndio=disabled \
  -Dcaca=disabled -Dsixel=disabled -Dvdpau=disabled -Dxv=disabled \
  -Dshaderc=disabled -Dspirv-cross=disabled \
  -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled
ninja -C "$out/mpv" -j "$jobs"
DESTDIR="$stage" meson install -C "$out/mpv" --no-rebuild --quiet

mpv=$stage$prefix/bin/mpv
mpv_dynamic=$(readelf -d "$mpv")
! grep -q '(NEEDED).*\[libplacebo' <<< "$mpv_dynamic" || fail "mpv needs a shared libplacebo"
! grep -q -E '\((RPATH|RUNPATH)\)' <<< "$mpv_dynamic" || fail "mpv has an rpath"
exported=$(nm -D --defined-only "$mpv" | awk '$3 ~ /^(plurx_)?pl_/' | head -n 3)
[ -z "$exported" ] || fail "mpv exports libplacebo symbols: $exported"
unrenamed=$(nm -D --undefined-only "$mpv" | awk '$2 ~ /^pl_/' | head -n 3)
[ -z "$unrenamed" ] || fail "mpv would call the system libplacebo: $unrenamed"
! grep -a -q -F "$player" "$mpv" || fail "$mpv contains the build path $player"

builtusing=$(dpkg-query -W -f '${source:Package} ${source:Version}\n' glslang-dev spirv-tools-dev)
{
  echo "plurx-player $version: private build"
  echo
  while read -r name ver sha url; do
    case $name in ''|'#'*|apt-snapshot) continue ;; esac
    echo "$name $ver"
    echo "  source: $url"
    echo "  sha256: $sha"
    echo "  git describe: $(git -C "$src/$name" describe --tags)"
    git -C "$src/$name" log --reverse --format='  patch: %s' "v$ver-plurx..HEAD"
  done < "$lock"
  echo "closed-form display management (compiled into mpv):"
  for file in dm/dm_closed_form.h dm/dv_dm.h packaging/libdovi/rpu_parser.h; do
    echo "  $(basename "$file") sha256 $(sha256sum < "$player/$file" | cut -d' ' -f1)"
  done
  echo
  echo "Installed under $prefix: bin/mpv, with libplacebo linked in, its symbols renamed plurx_pl_* and hidden."
  echo "libplacebo statically embeds glslang and SPIRV-Tools from the build environment:"
  echo "$builtusing" | sed 's/^/  /'
  echo "(licences in copyright.glslang and copyright.spirv-tools beside this file)."
  echo "Build environment: snapshot.ubuntu.com $(awk '$1 == "apt-snapshot" {print $2}' "$lock")."
  echo
  echo "Corresponding source: the Debian source package plurx-player_$version.dsc,"
  echo "published beside the .deb with plurx-player_${version%-*}.orig.tar.gz (the launcher and"
  echo "the patch series), .orig-libplacebo.tar.gz, .orig-mpv.tar.gz and"
  echo "plurx-player_$version.debian.tar.xz. dpkg-source -x unpacks it and"
  echo "dpkg-buildpackage rebuilds this package from it."
} > "$out/private-build.txt"
cat "$out/private-build.txt"
"$mpv" --no-config --version
