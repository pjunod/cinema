#!/usr/bin/env bash
# Inside a fresh Ubuntu 26.10 container (packaging/build-deb.sh starts it):
# install the .deb from /out the way a user would, check that the private
# mpv uses the private libplacebo while the system mpv beside it keeps the
# system one, render a 10-bit PQ clip through vo=gpu-next on Vulkan (Mesa's
# software device under Xvfb) with both, validate the desktop entry and the
# scheme registration, run lintian and the launcher's tests, and remove it.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8 PYTHONDONTWRITEBYTECODE=1

private=/usr/lib/plurx-player
cache=/usr/share/applications/mimeinfo.cache
step() { printf '\n== %s\n' "$*"; }
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

cd /out
deb=$(ls plurx-player_*_amd64.deb)
step "SHA256SUMS"
sha256sum -c SHA256SUMS && pass "checksum"

apt-get update -q >/dev/null
# desktop-file-utils owns the update-desktop-database trigger, as on a desktop.
apt-get install -y -q --no-install-recommends desktop-file-utils lintian >/dev/null

step "lintian $deb"
lintian --allow-root --info "$deb" | tee /tmp/lintian.txt || true
errors=$(grep -c '^E:' /tmp/lintian.txt || true)
warnings=$(grep -c '^W:' /tmp/lintian.txt || true)
[ "$errors" -eq 0 ] || fail "lintian: $errors error(s)"
pass "lintian: 0 errors, $warnings warning(s)"

step "dpkg -i (then apt-get -f install for the dependencies)"
if ! dpkg -i "$deb"; then
  apt-get -f install -y -q
fi
status=$(dpkg-query -W -f='${Status} ${Version}' plurx-player)
case $status in "install ok installed "*) pass "installed: $status" ;; *) fail "status: $status" ;; esac

step "private mpv --version"
"$private/bin/mpv" --no-config --version | tee /tmp/private-version.txt
grep -E '^libplacebo version: v[0-9.]+ \(v[0-9.]+-plurx' /tmp/private-version.txt \
  || fail "private mpv does not report the private libplacebo"
pass "private mpv reports $(sed -n 's/^libplacebo version: //p' /tmp/private-version.txt)"

check_private_ldd() {
  ldd "$private/bin/mpv" > /tmp/private-ldd.txt
  ! grep -q 'not found' /tmp/private-ldd.txt || { cat /tmp/private-ldd.txt; fail "unresolved libraries"; }
  local lp
  lp=$(awk '$1 ~ /^libplacebo\.so/ {print $3}' /tmp/private-ldd.txt)
  grep libplacebo /tmp/private-ldd.txt
  case $(realpath "$lp") in
    "$private"/lib/libplacebo.so.*) pass "private mpv loads $(realpath "$lp")" ;;
    *) fail "private mpv loads libplacebo from $lp" ;;
  esac
}
step "ldd private mpv"
check_private_ldd

step "scheme registration and desktop entry"
grep -x 'x-scheme-handler/plurx-player=plurx-player.desktop;' "$cache" \
  && pass "dpkg's desktop-database trigger registered the scheme" || fail "scheme not in $cache"
desktop-file-validate /usr/share/applications/plurx-player.desktop && pass "desktop-file-validate"
! grep -s -l xdg-mime /var/lib/dpkg/info/plurx-player.* || fail "a maintainer script calls xdg-mime"
pass "no maintainer script calls xdg-mime ($(cd /var/lib/dpkg/info && ls plurx-player.*inst plurx-player.*rm 2>/dev/null | tr '\n' ' '))"

step "launcher"
# As root, deliberately: a root run may write .pyc files, and dpkg -r below
# must still remove the directory.
plurx-player --help >/dev/null && pass "plurx-player --help"
resolved=$(python3 -I -c 'import sys; sys.path.insert(0, "/usr/lib/plurx-player"); from plurx_player import mpv; print(mpv.resolve_binary())')
[ "$resolved" = "$private/bin/mpv" ] && pass "launcher resolves mpv to $resolved" || fail "launcher resolves mpv to $resolved"

step "system mpv alongside"
apt-get install -y -q mpv >/dev/null
mpv --no-config --version | tee /tmp/system-version.txt
grep -q -- '-plurx' /tmp/system-version.txt && fail "system mpv reports the private libplacebo"
ldd /usr/bin/mpv | grep libplacebo
case $(realpath "$(ldd /usr/bin/mpv | awk '$1 ~ /^libplacebo\.so/ {print $3}')") in
  /usr/lib/x86_64-linux-gnu/libplacebo.so.*) pass "system mpv loads the system libplacebo ($(dpkg-query -W -f='${Version}' 'libplacebo[0-9]*'))" ;;
  *) fail "system mpv loads a libplacebo outside /usr/lib/x86_64-linux-gnu" ;;
esac
check_private_ldd

step "render a 10-bit PQ HEVC clip through vo=gpu-next on Vulkan (lavapipe, Xvfb)"
apt-get install -y -q --no-install-recommends ffmpeg xvfb xauth mesa-vulkan-drivers vulkan-tools >/dev/null
# libplacebo skips software devices unless one is named.
device=$(vulkaninfo --summary 2>/dev/null | sed -n 's/^[[:space:]]*deviceName[[:space:]]*=[[:space:]]*//p' | head -n1)
[ -n "$device" ] || fail "no Vulkan device"
echo "Vulkan device: $device"
ffmpeg -loglevel error -f lavfi -i testsrc2=size=1280x720:rate=24 -t 2 \
  -c:v libx265 -pix_fmt yuv420p10le -x265-params log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display='G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,50)':max-cll=1000,400 \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc /tmp/pq.mkv
render() {
  local bin=$1 want=$2
  xvfb-run -a "$bin" --no-config --vo=gpu-next --gpu-api=vulkan --gpu-context=x11vk \
    --vulkan-device="$device" --hwdec=no --ao=null --frames=24 --msg-level=all=v /tmp/pq.mkv \
    > /tmp/render.txt 2>&1 || { tail -40 /tmp/render.txt; fail "$bin could not render"; }
  local init
  init=$(grep -m1 'vo/gpu-next/libplacebo\] Initialized libplacebo' /tmp/render.txt) || fail "$bin did not start libplacebo"
  echo "$init"
  grep -m1 -E '^VO: \[gpu-next\] 1280x720 yuv420p10' /tmp/render.txt || fail "$bin did not open vo=gpu-next for the 10-bit clip"
  case $want in
    private) case $init in *-plurx*) ;; *) fail "$bin rendered with a libplacebo that is not the private one" ;; esac ;;
    system) case $init in *-plurx*) fail "$bin rendered with the private libplacebo" ;; esac ;;
  esac
  pass "$bin rendered PQ frames through gpu-next/Vulkan with the $want libplacebo"
}
render "$private/bin/mpv" private
render /usr/bin/mpv system

step "launcher unit tests ($(python3 --version))"
python3 -m unittest discover -s /src/tests -p 'test_*.py' && pass "launcher tests"

step "dpkg -r"
dpkg -r plurx-player
[ ! -e "$private" ] || { find "$private"; fail "$private left behind"; }
[ ! -e /usr/bin/plurx-player ] || fail "/usr/bin/plurx-player left behind"
! grep -q plurx-player "$cache" || fail "scheme still in $cache"
dpkg -P plurx-player
dpkg-query -W -f='${Status}\n' plurx-player 2>/dev/null | grep -q -v 'not-installed' && fail "dpkg still lists plurx-player" || true
mpv --no-config --version >/dev/null && pass "removed cleanly; the system mpv still runs"

printf '\nALL CHECKS PASSED\n'
