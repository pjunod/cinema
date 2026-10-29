#!/usr/bin/env bash
# Inside a fresh Ubuntu 26.10 container (packaging/build-deb.sh starts it, with
# the repository read-only at /repo and the build output read-only at /out):
# install the .deb the way a user would and check it end to end, then remove
# it. The checks, in order: checksums; lintian on the .deb and the source
# package; dpkg -i + apt-get -f install; the private mpv runs its linked-in
# libplacebo, exports none of its symbols and loads only the system one
# (for libavfilter), also under LD_LIBRARY_PATH; the
# scheme registration and desktop entry; the launcher; the system mpv beside
# it; both mpvs rendering a 10-bit PQ clip through vo=gpu-next on Vulkan
# (Mesa's software device under Xvfb); the launcher's own mpv command line
# against a hostile ~/.config/mpv and with the ytdl hook, each with a
# positive control; the launcher tests with no skip allowed; dpkg -r and -P.
#
# Every check is written `command || fail "..."` and then `pass "..."`, never
# `command && pass`, which under set -e lets a failing command fall through
# (tests/test_packaging.py rejects that form). With PLURX_VERIFY_SELFTEST=1
# the first check is given a wrong digest; build-deb.sh runs that once and
# requires the script to fail there.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8 PYTHONDONTWRITEBYTECODE=1

private=/usr/lib/plurx-player
packaging=/repo/clients/linux-player/packaging
cache=/usr/share/applications/mimeinfo.cache
system_lib=/usr/lib/x86_64-linux-gnu
step() { printf '\n== %s\n' "$*"; }
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

cd /out
deb=$(ls plurx-player_*_amd64.deb)
dsc=$(ls plurx-player_*.dsc)

step "SHA256SUMS"
sums=SHA256SUMS
if [ "${PLURX_VERIFY_SELFTEST:-}" = 1 ]; then
  echo "self-test: the first digest is altered, so this check must fail"
  awk 'NR == 1 { c = substr($1, 1, 1); $1 = (c == "0" ? "1" : "0") substr($1, 2) } { print $1 "  " $2 }' SHA256SUMS > /tmp/SHA256SUMS
  sums=/tmp/SHA256SUMS
fi
sha256sum --strict -c "$sums" || fail "checksums"
pass "checksums of $(wc -l < "$sums") files"

step "apt snapshot"
bash "$packaging/apt-snapshot.sh" "$packaging/upstream.lock" > /tmp/apt.txt 2>&1 || { cat /tmp/apt.txt; fail "apt snapshot"; }
tail -n1 /tmp/apt.txt
# desktop-file-utils owns the update-desktop-database trigger, as on a desktop.
apt-get install -y -q --no-install-recommends desktop-file-utils lintian binutils git lua5.2 >/dev/null || fail "install the tools"
pass "tools installed"

step "lintian $deb $dsc"
lintian --allow-root --fail-on error --info "$deb" "$dsc" > /tmp/lintian.txt 2>&1 \
  || { cat /tmp/lintian.txt; fail "lintian reported errors (or did not run)"; }
cat /tmp/lintian.txt
warnings=$(awk '/^W:/ { n++ } END { print n + 0 }' /tmp/lintian.txt)
pass "lintian: no errors, $warnings warning(s)"

step "dpkg -i (then apt-get -f install for the dependencies)"
if ! dpkg -i "$deb"; then
  apt-get -f install -y -q || fail "apt-get -f install"
fi
status=$(dpkg-query -W -f='${Status} ${Version}' plurx-player) || fail "dpkg-query"
case $status in "install ok installed "*) ;; *) fail "status: $status" ;; esac
pass "installed: $status"

step "private mpv --version"
"$private/bin/mpv" --no-config --version > /tmp/private-version.txt || fail "private mpv --version"
cat /tmp/private-version.txt
grep -E '^libplacebo version: v[0-9.]+ \(v[0-9.]+-plurx' /tmp/private-version.txt \
  || fail "private mpv does not report the private libplacebo"
pass "private mpv reports $(sed -n 's/^libplacebo version: //p' /tmp/private-version.txt)"

step "private mpv has libplacebo linked in"
dynamic=$(readelf -d "$private/bin/mpv") || fail "readelf"
! grep -F '(NEEDED)' <<< "$dynamic" | grep -F '[libplacebo' || fail "mpv needs a shared libplacebo"
! grep -E '\((RPATH|RUNPATH)\)' <<< "$dynamic" || fail "mpv has an rpath"
exported=$(nm -D --defined-only "$private/bin/mpv" | awk '$3 ~ /^(plurx_)?pl_/') || fail "nm"
[ -z "$exported" ] || fail "mpv exports libplacebo symbols: $(head -n 3 <<< "$exported")"
imported=$(nm -D --undefined-only "$private/bin/mpv" | awk '$2 ~ /^pl_/') || fail "nm"
[ -z "$imported" ] || fail "mpv would call the system libplacebo: $(head -n 3 <<< "$imported")"
! ls "$private"/lib 2> /dev/null || fail "the package installs a private library directory"
pass "mpv needs no shared libplacebo, has no rpath, and exports and imports no libplacebo symbol"

# The only libplacebo mpv's process loads is the system one, for libavfilter.
check_private_ldd() {
  local label=$1; shift
  env "$@" ldd "$private/bin/mpv" > /tmp/private-ldd.txt || fail "$label: ldd"
  ! grep 'not found' /tmp/private-ldd.txt || fail "$label: unresolved libraries"
  local lp
  for lp in $(awk '$1 ~ /^libplacebo\.so/ { print $3 }' /tmp/private-ldd.txt); do
    lp=$(realpath "$lp")
    case $lp in "$system_lib"/libplacebo.so.*) ;; *) fail "$label: private mpv loads libplacebo from $lp" ;; esac
  done
  pass "$label: private mpv loads no libplacebo but the system one ($(awk '$1 ~ /^libplacebo\.so/ { print $1 }' /tmp/private-ldd.txt | paste -sd' ' -))"
}
step "ldd private mpv"
check_private_ldd "alone"

step "scheme registration and desktop entry"
grep -x 'x-scheme-handler/plurx-player=plurx-player.desktop;' "$cache" || fail "the scheme is not in $cache"
pass "dpkg's desktop-database trigger registered the scheme"
desktop-file-validate /usr/share/applications/plurx-player.desktop || fail "desktop-file-validate"
pass "desktop-file-validate"
! grep -s -l xdg-mime /var/lib/dpkg/info/plurx-player.* || fail "a maintainer script calls xdg-mime"
pass "no maintainer script calls xdg-mime ($(cd /var/lib/dpkg/info && echo plurx-player.*inst plurx-player.*rm))"

step "launcher"
# As root, deliberately: a root run may write .pyc files, and dpkg -r below
# must still remove the directory.
plurx-player --help > /dev/null || fail "plurx-player --help"
pass "plurx-player --help"
resolved=$(python3 -I -c 'import sys; sys.path.insert(0, "/usr/lib/plurx-player"); from plurx_player import mpv; print(mpv.resolve_binary())') \
  || fail "import the installed launcher"
[ "$resolved" = "$private/bin/mpv" ] || fail "launcher resolves mpv to $resolved"
pass "launcher resolves mpv to $resolved"

step "system mpv alongside"
apt-get install -y -q mpv > /dev/null || fail "install the system mpv"
mpv --no-config --version > /tmp/system-version.txt || fail "system mpv --version"
cat /tmp/system-version.txt
! grep -- '-plurx' /tmp/system-version.txt || fail "system mpv reports the private libplacebo"
system_lp=$(ldd /usr/bin/mpv | awk '$1 ~ /^libplacebo\.so/ { print $3 }') || fail "ldd /usr/bin/mpv"
system_lp=$(realpath "$system_lp")
case $system_lp in "$system_lib"/libplacebo.so.*) ;; *) fail "system mpv loads $system_lp" ;; esac
pass "system mpv loads $system_lp ($(dpkg-query -W -f='${Version}' 'libplacebo[0-9]*'))"
check_private_ldd "beside the system mpv"

step "LD_LIBRARY_PATH pointing at the system libplacebo"
ls "$system_lib"/libplacebo.so.* || fail "no system libplacebo to point at"
check_private_ldd "LD_LIBRARY_PATH=$system_lib" LD_LIBRARY_PATH="$system_lib"
LD_LIBRARY_PATH="$system_lib" "$private/bin/mpv" --no-config --version > /tmp/ldpath-version.txt || fail "mpv --version under LD_LIBRARY_PATH"
grep -E '^libplacebo version: .*-plurx' /tmp/ldpath-version.txt || fail "under LD_LIBRARY_PATH the private mpv runs the system libplacebo"
pass "under LD_LIBRARY_PATH=$system_lib the private mpv still runs the private libplacebo"

step "render a 10-bit PQ HEVC clip through vo=gpu-next on Vulkan (lavapipe, Xvfb)"
apt-get install -y -q --no-install-recommends ffmpeg xvfb xauth mesa-vulkan-drivers vulkan-tools > /dev/null || fail "install the render tools"
# libplacebo skips software devices unless one is named.
device=$(vulkaninfo --summary 2>/dev/null | sed -n 's/^[[:space:]]*deviceName[[:space:]]*=[[:space:]]*//p' | head -n1)
[ -n "$device" ] || fail "no Vulkan device"
echo "Vulkan device: $device"
ffmpeg -loglevel error -f lavfi -i testsrc2=size=1280x720:rate=24 -t 2 \
  -c:v libx265 -pix_fmt yuv420p10le -x265-params log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display='G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,50)':max-cll=1000,400 \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc /tmp/pq.mkv || fail "make the PQ clip"
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

step "the launcher's own mpv command line"
mapfile -t argv < <(python3 -I -c 'import os, sys; sys.path.insert(0, "/usr/lib/plurx-player")
from plurx_player import mpv, trust
m = mpv.Mpv(mpv.resolve_binary(), "/tmp/plurx-ipc.sock", config_dir=os.path.join(trust.default_config_dir(), "mpv"))
print("\n".join(m.args()))')
[ "${#argv[@]}" -gt 1 ] || fail "could not get the launcher's argv"
printf '%s\n' "${argv[@]}"
printf '%s\n' "${argv[@]}" | grep -qx -- "--config-dir=/root/.config/plurx-player/mpv" || fail "argv has no plurx-player --config-dir"
printf '%s\n' "${argv[@]}" | grep -qx -- "--ytdl=no" || fail "argv has no --ytdl=no"
pass "argv has --config-dir=/root/.config/plurx-player/mpv and --ytdl=no"
# A run whose status is not the point: the file fails to load on purpose,
# and the log is what is checked.
expect_log() {
  local log=$1; shift
  if "$@" > "$log.out" 2>&1; then :; fi
}

# A hostile general mpv config. Control: without --config-dir the same mpv
# reads it and names it in its log.
mkdir -p /root/.config/mpv
printf 'tone-mapping=clip\nytdl=yes\n' > /root/.config/mpv/mpv.conf
expect_log /tmp/control "$private/bin/mpv" --vo=null --ao=null --frames=1 --log-file=/tmp/control.txt /tmp/pq.mkv
grep -m1 '/root/.config/mpv/mpv.conf' /tmp/control.txt || fail "control: mpv without --config-dir did not name ~/.config/mpv/mpv.conf"
pass "control: without --config-dir, mpv reads ~/.config/mpv/mpv.conf"
# --no-terminal (from the launcher) silences stdout, so these log to a file.
# These standalone probes have no launcher to send quit. Override only their
# lifetime so EOF/failure terminates instead of waiting for Next indefinitely;
# all renderer, private config and Cinema script arguments remain in effect.
xvfb-run -a "${argv[@]}" --idle=no --keep-open=no --vulkan-device="$device" --ao=null --frames=24 \
  --log-file=/tmp/launcher-argv.txt /tmp/pq.mkv > /tmp/launcher-argv.out 2>&1 \
  || { cat /tmp/launcher-argv.out; tail -40 /tmp/launcher-argv.txt; fail "the launcher's argv did not play"; }
grep -m1 -E 'VO: \[gpu-next\]' /tmp/launcher-argv.txt || fail "the launcher's argv did not open vo=gpu-next"
! grep -m3 '/root/.config/mpv' /tmp/launcher-argv.txt || fail "the launcher's mpv read ~/.config/mpv"
pass "the launcher's argv plays through gpu-next and reads no ~/.config/mpv"

# ytdl. Control: with --ytdl=yes, a URL mpv cannot open reaches the hook.
# Both runs are read from --log-file, the channel the launcher's argv uses.
expect_log /tmp/ytdl-control "$private/bin/mpv" --no-config --ytdl=yes --msg-level=ytdl_hook=trace \
  --vo=null --ao=null --log-file=/tmp/ytdl-control.txt http://127.0.0.1:1/x.mkv
grep -m1 '\[ytdl_hook\]' /tmp/ytdl-control.txt || fail "control: --ytdl=yes did not run the ytdl hook"
pass "control: with --ytdl=yes the ytdl hook runs on an unopenable URL"
expect_log /tmp/ytdl-launcher xvfb-run -a "${argv[@]}" --idle=no --keep-open=no --msg-level=ytdl_hook=trace \
  --log-file=/tmp/ytdl-launcher.txt http://127.0.0.1:1/x.mkv
grep -m1 'Failed to open http://127.0.0.1:1/x.mkv' /tmp/ytdl-launcher.txt || fail "the launcher's argv never tried the URL"
# The module tag, as in the control: mpv's own echo of the command line
# names ytdl_hook in --msg-level.
! grep -m3 '\[ytdl_hook\]' /tmp/ytdl-launcher.txt || fail "the launcher's argv ran the ytdl hook"
pass "the launcher's argv never runs the ytdl hook"
rm -rf /root/.config/mpv

step "launcher tests ($(python3 --version)), no skip allowed"
python3 -B "$packaging/run-tests" /repo/clients/linux-player/tests || fail "launcher tests (a failure, an error or a skip)"
pass "launcher tests, none skipped"

step "installed Cinema overlay and Lua behavior"
test -r "$private/plurx_player/scripts/cinema.lua" || fail "Cinema overlay missing from installed package"
lua5.2 /repo/clients/linux-player/tests/test_cinema.lua "$private/plurx_player/scripts/cinema.lua" || fail "installed Cinema overlay scenarios"
! grep -E 'cinema.*(Error|error)|Lua error' /tmp/launcher-argv.txt || fail "Cinema overlay failed during gpu-next playback"
pass "installed Cinema overlay: behavior and gpu-next render"

step "dpkg -r"
dpkg -r plurx-player || fail "dpkg -r"
[ ! -e "$private" ] || { find "$private"; fail "$private left behind"; }
[ ! -e /usr/bin/plurx-player ] || fail "/usr/bin/plurx-player left behind"
! grep plurx-player "$cache" || fail "the scheme is still in $cache"
dpkg -P plurx-player || fail "dpkg -P"
if status=$(dpkg-query -W -f='${Status}' plurx-player 2>/dev/null); then
  case $status in *not-installed*) ;; *) fail "dpkg still lists plurx-player: $status" ;; esac
fi
mpv --no-config --version > /dev/null || fail "the system mpv no longer runs"
pass "removed cleanly; the system mpv still runs"

printf '\nALL CHECKS PASSED\n'
