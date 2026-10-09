#!/bin/sh
set -eu
meson setup --wipe /work/libplacebo-build /work/libplacebo-0d043c7f6f79cd3687c023454bdacbe615e4d96f --prefix=/work/prefix --buildtype=release -Dvulkan=enabled -Dopengl=disabled -Dd3d11=disabled -Dglslang=enabled -Dshaderc=disabled -Ddovi=enabled -Dlibdovi=disabled -Ddemos=false -Dtests=false -Dxxhash=enabled
meson compile -C /work/libplacebo-build -j 3
meson install -C /work/libplacebo-build
cp /opt/m0/package-versions.txt /work/package-versions.txt
vulkaninfo --summary > /work/vulkan-summary.txt 2>&1
