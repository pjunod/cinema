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
# mpv is linked with DT_RPATH $ORIGIN/../lib (--disable-new-dtags), not
# DT_RUNPATH: the loader searches DT_RPATH before LD_LIBRARY_PATH, so even
# /usr/lib/plurx-player/bin/mpv started by hand with LD_LIBRARY_PATH pointing
# at another libplacebo loads the private one. The system mpv has no rpath,
# and the private directory is not in ld.so.conf, so it never sees the
# private libplacebo.
#
# mpv finds the staged libplacebo through -L in LDFLAGS rather than through
# the .pc file, so meson adds no build-directory rpath: no build path is left
# in mpv's dynamic string table, and the binaries do not depend on where the
# build ran.
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

meson setup "$out/libplacebo" "$src/libplacebo" \
  --prefix="$prefix" --libdir=lib --buildtype=plain --wrap-mode=nodownload \
  -Dvulkan=enabled -Dopengl=enabled -Dglslang=enabled -Dshaderc=disabled \
  -Dlcms=enabled -Dxxhash=enabled -Ddovi=enabled -Dlibdovi=disabled \
  -Dd3d11=disabled -Ddemos=false -Dtests="$tests" -Dbench=false -Dfuzz=false
ninja -C "$out/libplacebo" -j "$jobs"
DESTDIR="$stage" meson install -C "$out/libplacebo" --no-rebuild --quiet

# The staged .pc, with its prefix moved into the stage and without -L (see
# above). The system libplacebo-dev is not installed in the build container,
# and the check below fails the build if pkg-config found anything else.
mkdir -p "$out/pkgconfig"
sed -e "s|^prefix=.*|prefix=$stage$prefix|" -e 's|-L\${libdir} *||' \
  "$stage$prefix/lib/pkgconfig/libplacebo.pc" > "$out/pkgconfig/libplacebo.pc"
export PKG_CONFIG_PATH="$out/pkgconfig"
[ "$(pkg-config --variable=libdir libplacebo)" = "$stage$prefix/lib" ] \
  || fail "pkg-config resolves libplacebo outside the stage"

LDFLAGS="${LDFLAGS:-} -L$stage$prefix/lib -Wl,--disable-new-dtags -Wl,-rpath,\$ORIGIN/../lib" \
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
soname=$(readelf -d "$stage$prefix/lib/libplacebo.so" | sed -n 's/.*(SONAME).*\[\(.*\)\]$/\1/p')
lib=$stage$prefix/lib/$soname
mpv_dynamic=$(readelf -d "$mpv")
lib_dynamic=$(readelf -d "$lib")
rpath=$(sed -n 's/.*(RPATH).*\[\(.*\)\]$/\1/p' <<< "$mpv_dynamic")
[ "$rpath" = '$ORIGIN/../lib' ] || fail "mpv DT_RPATH is '$rpath', want \$ORIGIN/../lib"
! grep -q '(RUNPATH)' <<< "$mpv_dynamic" || fail "mpv has a DT_RUNPATH, which LD_LIBRARY_PATH would beat"
grep -q "(NEEDED).*\[$soname\]" <<< "$mpv_dynamic" || fail "mpv does not need $soname"
! grep -q -E '\((RPATH|RUNPATH)\)' <<< "$lib_dynamic" || fail "the private libplacebo has an rpath"
for binary in "$mpv" "$lib"; do
  ! grep -a -q -F "$player" "$binary" || fail "$binary contains the build path $player"
done
resolved=$(LD_LIBRARY_PATH=/nonexistent ldd "$mpv" | awk -v so="$soname" '$1 == so {print $3}')
[ "$(realpath "$resolved")" = "$(realpath "$lib")" ] || fail "staged mpv resolves $soname to $resolved"

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
  echo
  echo "Installed under $prefix: bin/mpv (DT_RPATH $rpath) and lib/$soname."
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
