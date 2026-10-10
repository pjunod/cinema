#!/bin/bash
set -euo pipefail
exec > >(tee /work/controls.log) 2>&1
rustc --version
cargo run --locked --release --manifest-path /work/src/dovi_tool/Cargo.toml -p dolby_vision --features dolby_vision/serde --example m0_generate -- /work/fixtures
g++ -std=c++20 -O2 -I/work/src/DoViBaker/include /work/cpu_processor.cpp /work/src/DoViBaker/DoViBaker/DoViProcessor.cpp /work/deps/lib/libdovi.a -lpthread -ldl -lm -o /work/cpu_processor
mkdir -p /work/cpu-outputs
rm -f /work/cpu-outputs/missing-el.rgb48le /work/cpu-outputs/malformed.rgb48le
/work/cpu_processor /work/fixtures/p7_fel_linear.nal /work/cpu-outputs/nonzero.rgb48le 1 16
/work/cpu_processor /work/fixtures/p7_fel_linear.nal /work/cpu-outputs/zero.rgb48le 0 16
/work/cpu_processor /work/fixtures/p81_identity.nal /work/cpu-outputs/disabled.rgb48le 1 16
/work/cpu_processor /work/fixtures/p81_adapted_linear.nal /work/cpu-outputs/adapted-base.rgb48le 1 16
/work/cpu_processor /work/fixtures/p7_fel_nonstandard_linear_matrix.nal /work/cpu-outputs/nonstandard.rgb48le 1 16
/work/cpu_processor /work/fixtures/p7_fel_bounded_residual.nal /work/cpu-outputs/bounded.rgb48le 1 16
set +e
/work/cpu_processor /work/fixtures/p7_fel_linear.nal /work/cpu-outputs/missing-el.rgb48le 1 0
result=$?
set -e
missing_el_status=$result
printf '{"negative_control":"missing_el","exit_code":%s}\n' "$result"
if test "$result" -ne 67; then exit 69; fi
cp /work/fixtures/p7_fel_linear.nal /work/fixtures/truncated.nal
truncate -s 16 /work/fixtures/truncated.nal
set +e
/work/cpu_processor /work/fixtures/truncated.nal /work/cpu-outputs/malformed.rgb48le 1 16
result=$?
set -e
printf '{"negative_control":"truncated_rpu","exit_code":%s}\n' "$result"
if test "$result" -ne 67; then exit 70; fi
printf '{"missing_el":%s,"truncated_rpu":%s}\n' "$missing_el_status" "$result" > /work/negative-controls.json
python3 /work/cpu_reference.py
python3 /work/check_cpu_outputs.py
python3 /work/check_cpu_outputs_selftest.py
python3 /work/check_metrics.py
sha256sum /work/cpu_processor /work/cpu-outputs/*.rgb48le
