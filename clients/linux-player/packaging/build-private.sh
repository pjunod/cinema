#!/usr/bin/env bash
# Build the private libplacebo and mpv for plurx-player. debian/rules calls
# this after packaging/prepare-sources.sh has fetched and patched upstream/.
#
#   JOBS=8 packaging/build-private.sh <clients/linux-player>
#
# Both are configured for prefix /usr/lib/plurx-player and staged under
# build/stage/usr/lib/plurx-player. mpv is linked with the RUNPATH
# $ORIGIN/../lib, so /usr/lib/plurx-player/bin/mpv loads
# /usr/lib/plurx-player/lib/libplacebo.so.* and never the system libplacebo,
# while the system mpv (no RUNPATH, and the private directory is not in
# ld.so.conf) never sees the private one.
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
up=$player/upstream
out=$player/build
stage=$out/stage
prefix=/usr/lib/plurx-player
jobs=${JOBS:-1}
tests=true
case " ${DEB_BUILD_OPTIONS:-} " in *" nocheck "*) tests=false ;; esac

for name in libplacebo mpv; do
  [ -d "$up/$name/.git" ] || {
    echo "build-private: $up/$name is missing; run packaging/build-deb.sh, or packaging/prepare-sources.sh first" >&2
    exit 1
  }
done
rm -rf "$out"
mkdir -p "$out"

echo "build-private: libplacebo $(git -C "$up/libplacebo" describe)"
meson setup "$out/libplacebo" "$up/libplacebo" \
  --prefix="$prefix" --libdir=lib --buildtype=plain --wrap-mode=nodownload \
  -Dvulkan=enabled -Dopengl=enabled -Dglslang=enabled -Dshaderc=disabled \
  -Dlcms=enabled -Dxxhash=enabled -Ddovi=enabled -Dlibdovi=disabled \
  -Dd3d11=disabled -Ddemos=false -Dtests="$tests" -Dbench=false -Dfuzz=false
ninja -C "$out/libplacebo" -j "$jobs"
DESTDIR="$stage" meson install -C "$out/libplacebo" --no-rebuild --quiet

# mpv finds the staged libplacebo through a copy of its .pc with the prefix
# pointing into the stage. The system libplacebo-dev is not installed in the
# build container, and the check below fails the build if pkg-config found
# anything else.
mkdir -p "$out/pkgconfig"
sed "s|^prefix=.*|prefix=$stage$prefix|" "$stage$prefix/lib/pkgconfig/libplacebo.pc" > "$out/pkgconfig/libplacebo.pc"
export PKG_CONFIG_PATH="$out/pkgconfig"
case $(pkg-config --variable=libdir libplacebo) in
  "$stage$prefix/lib") ;;
  *) echo "build-private: pkg-config resolves libplacebo outside the stage" >&2; exit 1 ;;
esac

echo "build-private: mpv $(git -C "$up/mpv" describe --tags)"
LDFLAGS="${LDFLAGS:-} -Wl,-rpath,\$ORIGIN/../lib" meson setup "$out/mpv" "$up/mpv" \
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
runpath=$(readelf -d "$mpv" | sed -n 's/.*(RUNPATH).*\[\(.*\)\]$/\1/p')
[ "$runpath" = '$ORIGIN/../lib' ] || { echo "build-private: mpv RUNPATH is '$runpath', want \$ORIGIN/../lib" >&2; exit 1; }
soname=$(readelf -d "$stage$prefix/lib/libplacebo.so" | sed -n 's/.*(SONAME).*\[\(.*\)\]$/\1/p')
readelf -d "$mpv" | grep -q "(NEEDED).*\[$soname\]" || { echo "build-private: mpv does not need $soname" >&2; exit 1; }
if readelf -d "$stage$prefix/lib/$soname" | grep -q -E '\((RPATH|RUNPATH)\)'; then
  echo "build-private: the private libplacebo has an rpath" >&2; exit 1
fi
resolved=$(ldd "$mpv" | awk -v so="$soname" '$1 == so {print $3}')
[ "$(realpath "$resolved")" = "$(realpath "$stage$prefix/lib/$soname")" ] || {
  echo "build-private: staged mpv resolves $soname to $resolved" >&2; exit 1; }

{
  echo "plurx-player private build"
  echo
  while read -r name ver sha url; do
    case $name in ''|'#'*) continue ;; esac
    echo "$name $ver"
    echo "  source: $url"
    echo "  sha256: $sha"
    echo "  git describe: $(git -C "$up/$name" describe --tags)"
    git -C "$up/$name" log --reverse --format='  patch: %s' "v$ver-plurx..HEAD"
  done < "$player/packaging/upstream.lock"
  echo
  echo "Installed under $prefix: bin/mpv (RUNPATH $runpath) and lib/$soname."
} > "$out/private-build.txt"
cat "$out/private-build.txt"
"$mpv" --no-config --version
