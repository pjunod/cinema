#!/bin/bash
set -euo pipefail
exec > >(tee /work/render.log) 2>&1
export PKG_CONFIG_PATH=/work/prefix/lib/aarch64-linux-gnu/pkgconfig
export LD_LIBRARY_PATH=/work/prefix/lib/aarch64-linux-gnu
export XDG_RUNTIME_DIR=/tmp/runtime-nonidentity
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json
mkdir "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
cc -Wall -Wextra -Werror -std=c11 -I/work/include /work/p81_nonidentity_probe.c \
   /work/lib/libdovi.a -o /work/p81_nonidentity_probe \
   $(pkg-config --cflags --libs libplacebo) -lm -lpthread -ldl
cc -Wall -Wextra -Werror -std=c11 -I/work/include /work/hdr10_baseline_probe.c \
   /work/lib/libdovi.a -o /work/hdr10_baseline_probe \
   $(pkg-config --cflags --libs libplacebo) -lm -lpthread -ldl
for control in correct wrong hdr10; do
    for frame in 0 1 2 3; do
        mkdir -p "/work/gpu/$control/frame-$frame/outputs"
        cd "/work/gpu/$control/frame-$frame"
        cp /work/decoded.yuv420p10le .
        if test "$control" = wrong; then rpu="/work/wrong-rpu-frame$frame.nal"; else rpu="/work/adapted-rpu-frame$frame.nal"; fi
        if test "$control" = hdr10; then probe=/work/hdr10_baseline_probe; else probe=/work/p81_nonidentity_probe; fi
        cp "$rpu" consumed-rpu.nal
        "$probe" consumed-rpu.nal "$frame" > probe.jsonl 2> probe.stderr
    done
done
mkdir -p /work/gpu/matrix-rejected/outputs
cd /work/gpu/matrix-rejected
cp /work/decoded.yuv420p10le .
set +e
/work/p81_nonidentity_probe /work/p81-unsupported-matrix.nal 0 > probe.jsonl 2> probe.stderr
status=$?
set -e
if test "$status" -ne 1; then exit 71; fi
matrix_status=$status
mkdir -p /work/gpu/trim-rejected/outputs
cd /work/gpu/trim-rejected
cp /work/decoded.yuv420p10le .
set +e
/work/p81_nonidentity_probe /work/p81-unsupported-trim.nal 0 > probe.jsonl 2> probe.stderr
status=$?
set -e
if test "$status" -ne 1; then exit 72; fi
printf '{"unsupported_matrix":%s,"creative_trims":%s}\n' "$matrix_status" "$status" > /work/negative-admission.json
