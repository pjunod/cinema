#!/bin/bash
set -euo pipefail
exec > >(tee /work/generate.log) 2>&1
rustc --version
cargo run --locked --release --manifest-path /work/libdovi-source/Cargo.toml \
    -p dolby_vision --features dolby_vision/serde --example m0_nonidentity -- /work
