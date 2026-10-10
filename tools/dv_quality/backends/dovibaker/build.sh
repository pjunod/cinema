#!/bin/bash
set -euo pipefail
exec > >(tee /work/build.log) 2>&1
uname -a
rustc --version
cargo --version
g++ --version
/opt/m0-venv/bin/cmake --version
dpkg-query -W > /work/debian-packages.tsv
/opt/m0-venv/bin/pip freeze > /work/python-build-packages.txt
cargo rustc --locked --release \
    --manifest-path /work/src/dovi_tool/Cargo.toml \
    -p dolby_vision --features dolby_vision/capi --crate-type staticlib
mkdir -p /work/deps/lib
cp /work/src/dovi_tool/target/release/libdolby_vision.a /work/deps/lib/libdovi.a
/opt/m0-venv/bin/cmake \
    -S /work/src/DoViBaker -B /work/build \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_LIBRARY_PATH=/work/deps/lib
/opt/m0-venv/bin/cmake --build /work/build --parallel 2
sha256sum /work/deps/lib/libdovi.a /work/build/libdovibaker.*.so
