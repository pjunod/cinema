#!/usr/bin/env bash
# M0-P0 fixture corpus for scripts/vod-plan-probe.
#
# Deliberately adversarial timing: every HEVC fixture runs at 24000/1001 fps
# with keyint=42, so one GOP is 1.751750 s — a duration that is NOT an integral
# number of milliseconds and does NOT divide the 6 s segment floor. A plan
# boundary therefore never coincides with a round `-ss` value, which is exactly
# the condition under which an origin bug or a rounding bug survives a test that
# used a GOP dividing the offset (VOD-PRESENTATION-IMPLEMENTATION-HANDOFF §2,
# "measure, don't assert").
#
# x265/x264 parameter strings are copied verbatim from
# crates/plurx-core/src/testfixtures.rs so the corpus cannot drift onto a
# different GOP structure than the Rust suites classify.
set -euo pipefail

FF=${PLURX_FFMPEG:-ffmpeg}
OUT=${1:-target/vod-probe/fixtures}
mkdir -p "$OUT"

# 23.976 fps, keyint 42 -> GOP = 42*1001/24000 = 1.751750 s
R="24000/1001"
DUR=${VOD_PROBE_DURATION:-90}

enc_hevc() { # name params duration [size] [rate]
  local name=$1 params=$2 dur=$3 size=${4:-640x360} rate=${5:-$R}
  [ -f "$OUT/$name.mkv" ] && { echo "have  $name"; return; }
  echo "build $name"
  $FF -y -v error \
    -f lavfi -i "testsrc2=size=$size:rate=$rate:duration=$dur" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=$dur" \
    -c:v libx265 -preset ultrafast -x265-params "$params" \
    -c:a aac -pix_fmt yuv420p -ac 2 -shortest -f matroska "$OUT/$name.mkv.tmp"
  mv "$OUT/$name.mkv.tmp" "$OUT/$name.mkv"
}

enc_hevc closed-gop-2397 \
  "keyint=42:min-keyint=42:open-gop=0:bframes=4:scenecut=0:repeat-headers=1:log-level=none" "$DUR"
enc_hevc open-gop-2397 \
  "keyint=42:min-keyint=42:open-gop=1:bframes=4:b-pyramid=2:scenecut=0:repeat-headers=1:log-level=none" "$DUR"
enc_hevc clean-cra-2397 \
  "keyint=42:min-keyint=42:open-gop=1:bframes=0:scenecut=0:repeat-headers=1:log-level=none" "$DUR"

# H.264 at 30000/1001 fps, keyint 45 -> GOP = 1.501500 s. Same adversarial
# property on the other codec branch of `classify`.
if [ ! -f "$OUT/h264-2997.mkv" ]; then
  echo "build h264-2997"
  $FF -y -v error \
    -f lavfi -i "testsrc2=size=640x360:rate=30000/1001:duration=$DUR" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=$DUR" \
    -c:v libx264 -preset ultrafast -x264-params "keyint=45:min-keyint=45:open_gop=1:bframes=3" \
    -c:a aac -pix_fmt yuv420p -ac 2 -shortest -f matroska "$OUT/h264-2997.mkv.tmp"
  mv "$OUT/h264-2997.mkv.tmp" "$OUT/h264-2997.mkv"
else
  echo "have  h264-2997"
fi

# High-bitrate: 720p random noise defeats every predictor, so the byte ceiling
# binds before the duration ceiling — the CutReason::ByteCeiling branch that a
# clean testsrc2 stream never reaches.
if [ ! -f "$OUT/dense-2397.mkv" ]; then
  echo "build dense-2397"
  $FF -y -v error \
    -f lavfi -i "nullsrc=size=1280x720:rate=$R:duration=40,geq=random(1)*255:128:128" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=40" \
    -c:v libx265 -preset ultrafast \
    -x265-params "keyint=42:min-keyint=42:open-gop=0:bframes=4:scenecut=0:repeat-headers=1:crf=12:log-level=none" \
    -c:a aac -pix_fmt yuv420p -ac 2 -shortest -f matroska "$OUT/dense-2397.mkv.tmp"
  mv "$OUT/dense-2397.mkv.tmp" "$OUT/dense-2397.mkv"
else
  echo "have  dense-2397"
fi

# 5.1 AC-3: production transcodes this branch at 320k with an explicit 5.1
# layout (transcode/mod.rs:1330-1337), and it is also the only fixture on which
# the `-c:a copy` branch carries a non-AAC bitrate into the headroom bound.
if [ ! -f "$OUT/surround-2397.mkv" ]; then
  echo "build surround-2397"
  $FF -y -v error \
    -f lavfi -i "testsrc2=size=640x360:rate=$R:duration=60" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=60" \
    -c:v libx265 -preset ultrafast \
    -x265-params "keyint=42:min-keyint=42:open-gop=0:bframes=4:scenecut=0:repeat-headers=1:log-level=none" \
    -filter:a "pan=5.1|c0=c0|c1=c1|c2=c0|c3=c1|c4=c0|c5=c1" \
    -c:a ac3 -b:a 448k -pix_fmt yuv420p -shortest -f matroska "$OUT/surround-2397.mkv.tmp"
  mv "$OUT/surround-2397.mkv.tmp" "$OUT/surround-2397.mkv"
else
  echo "have  surround-2397"
fi

echo
ls -l "$OUT"

# The byte-ceiling exerciser: open GOP (every keyframe a CRA with leading
# pictures, so `classify` says dirty and no clean cut is available) at a
# bitrate where 64 MiB accumulates in well under the 15 s duration ceiling.
# Closed-GOP high-bitrate content never reaches the byte ceiling, because
# `CutPolicy::cut_before` takes a clean cut before it tests bytes.
if [ ! -f "$OUT/dense-open-2397.mkv" ]; then
  echo "build dense-open-2397"
  $FF -y -v error \
    -f lavfi -i "nullsrc=size=1280x720:rate=$R:duration=30,geq=random(1)*255:128:128" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=30" \
    -c:v libx265 -preset ultrafast \
    -x265-params "keyint=42:min-keyint=42:open-gop=1:bframes=4:b-pyramid=2:scenecut=0:repeat-headers=1:crf=12:log-level=none" \
    -c:a aac -pix_fmt yuv420p -ac 2 -shortest -f matroska "$OUT/dense-open-2397.mkv.tmp"
  mv "$OUT/dense-open-2397.mkv.tmp" "$OUT/dense-open-2397.mkv"
else
  echo "have  dense-open-2397"
fi

# Mixed verdicts in one stream. Every other fixture is all-clean or all-dirty,
# and on such a stream `Segmenter::push`'s decide-BEFORE-accumulate ordering is
# unobservable: the decision stops depending on the arriving fragment's
# verdict, so an off-by-one partition is identical to the right one. Forced
# IDRs every 3.503 s on top of an open GOP interleave clean and dirty
# fragments, which makes that ordering falsifiable.
if [ ! -f "$OUT/mixed-2397.mkv" ]; then
  echo "build mixed-2397"
  $FF -y -v error \
    -f lavfi -i "testsrc2=size=640x360:rate=$R:duration=$DUR" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=$DUR" \
    -c:v libx265 -preset ultrafast -force_key_frames "expr:gte(t,n_forced*3.503)" \
    -x265-params "keyint=42:min-keyint=42:open-gop=1:bframes=4:b-pyramid=2:scenecut=0:repeat-headers=1:log-level=none" \
    -c:a aac -pix_fmt yuv420p -ac 2 -shortest -f matroska "$OUT/mixed-2397.mkv.tmp"
  mv "$OUT/mixed-2397.mkv.tmp" "$OUT/mixed-2397.mkv"
else
  echo "have  mixed-2397"
fi

# A title whose audio runs past its video — the c58a4307 trailing-audio case.
# `frag_keyframe` has no video keyframe left to cut the tail on, so production
# emits the whole tail inside the last fragment while the plan must split it
# from the probe's per-track durations. Without this fixture the plan's
# TARGETDURATION is never tested against a stream that has a tail.
if [ ! -f "$OUT/audiotail-2397.mkv" ]; then
  echo "build audiotail-2397"
  $FF -y -v error \
    -f lavfi -i "testsrc2=size=640x360:rate=$R:duration=30" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=55" \
    -c:v libx265 -preset ultrafast \
    -x265-params "keyint=42:min-keyint=42:open-gop=0:bframes=4:scenecut=0:repeat-headers=1:log-level=none" \
    -c:a aac -pix_fmt yuv420p -ac 2 -f matroska "$OUT/audiotail-2397.mkv.tmp"
  mv "$OUT/audiotail-2397.mkv.tmp" "$OUT/audiotail-2397.mkv"
else
  echo "have  audiotail-2397"
fi
