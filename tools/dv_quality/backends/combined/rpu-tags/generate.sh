#!/bin/bash
set -euo pipefail
rustc --version
cargo run --locked --release --manifest-path /work/libdovi-source/Cargo.toml \
  -p dolby_vision --features dolby_vision/serde --example m0_p7_tags -- /tags
