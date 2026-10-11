#!/bin/sh
set -eu
export PKG_CONFIG_PATH=/work/prefix/lib/aarch64-linux-gnu/pkgconfig
export LD_LIBRARY_PATH=/work/prefix/lib/aarch64-linux-gnu
export XDG_RUNTIME_DIR=/tmp/runtime-m0
mkdir -p "$XDG_RUNTIME_DIR" /work/outputs
chmod 700 "$XDG_RUNTIME_DIR"
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json
cc -Wall -Wextra -Werror -std=c11 /work/fel_export_probe.c -o /work/fel_export_probe $(pkg-config --cflags --libs libplacebo) -lm
cd /work
./fel_export_probe > /work/probe.jsonl 2> /work/probe.stderr
