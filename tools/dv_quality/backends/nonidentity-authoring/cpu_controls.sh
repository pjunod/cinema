#!/bin/bash
set -euo pipefail
exec > >(tee /work/cpu-controls.log) 2>&1
g++ -std=c++20 -O2 -I/work/cpu-source/DoViBaker/include /work/cpu_processor.cpp \
    /work/cpu-source/DoViBaker/DoViBaker/DoViProcessor.cpp /work/cpu-lib/libdovi.a \
    -lpthread -ldl -lm -o /work/cpu_processor
sha256sum /work/cpu_processor
/work/cpu_processor /work/p7-affine-fel.nal /work/reconstructed16.rgb48le 1 16
/work/cpu_processor /work/p7-affine-fel.nal /work/zero16.rgb48le 0 16
/work/cpu_processor /work/p7-identity-input.nal /work/identity16.rgb48le 1 16
