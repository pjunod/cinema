#!/bin/sh
set -eu
rustc --version | tee /work/rust-version.txt
grep '^rustc 1.97.1 ' /work/rust-version.txt
export CARGO_HOME=/work/cargo-home
cargo run --offline --locked --release --manifest-path /work/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/Cargo.toml -p dolby_vision --features serde --example m0_reuse -- /work/fresh-rpus
