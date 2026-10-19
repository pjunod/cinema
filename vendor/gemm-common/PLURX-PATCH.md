# Vendored gemm-common 0.19.0 — preserve generic ARM code generation

This is the crates.io `gemm-common` 0.19.0 package from
[sarah-ek/gemm](https://github.com/sarah-ek/gemm), licensed under MIT in
[LICENSE](LICENSE). [UPSTREAM-SHA256.json](UPSTREAM-SHA256.json) records every
original package file hash and the original registry archive checksum;
[registry-provenance.lock](registry-provenance.lock) preserves its registry
identity for advisory scanning.

## Four FP16 helpers declare their instruction requirements

Rust 1.97.1 rejects eleven inline assembly instructions when compiling this
package for the generic `aarch64-unknown-linux-gnu` baseline: the four vector
helpers emit FP16 arithmetic without declaring the `fp16` target feature.
The scalar and conversion helpers already declare that feature.

The only production source changes add
`#[target_feature(enable = "neon,fp16")]` to `vmulq_f16`, `vaddq_f16`,
`vfmaq_f16` and `vfmaq_laneq_f16` in [src/simd.rs](src/simd.rs). The last
helper has eight lane variants, accounting for eight of the eleven rejected
instructions. Function-local annotations let Rust compile these helpers
without requiring FP16 in the executable's general instruction baseline.

## Runtime dispatch still protects CPUs without FP16

Upstream `gemm-f16` 0.19.0 detects NEON and FP16 before selecting its FP16
microkernels. Without FP16 it selects the ordinary NEON implementation;
without NEON it selects scalar arithmetic. `gemm-common::NeonFp16::try_new`
also checks both CPU features, and its vectorization entry points declare
`neon,fp16`. The workspace enables `std`, so these checks use
`std::arch::is_aarch64_feature_detected!`. No detector, dependency version or
global target feature changes in this patch.

## Portability has separate fallback and native checks

The independent [portability fixture](portability/src/lib.rs) runs the real
GEMM dispatcher on three matrix shapes and compares every half-precision
result against exact integer arithmetic. It also executes all four patched
helpers, including all eight lane variants, only after runtime FP16 detection.

```bash
# Compile executable code for the unchanged generic ARM baseline.
cargo test --locked --manifest-path vendor/gemm-common/portability/Cargo.toml --no-run
# Run its test binary on native ARM with FP16 hardware.
PLURX_FP16_EXPECTED=present <test-binary> --nocapture
# Emulate an ARMv8 Cortex-A53 without FP16 arithmetic to prove fallback.
PLURX_FP16_EXPECTED=absent qemu-aarch64 -cpu cortex-a53 <test-binary> --nocapture
```

The expected-feature assertion prevents a fallback test from silently running
on an FP16 CPU. Cortex-A53 emulation proves instruction portability and the
fallback calculation; it is not physical hardware acceptance. Neither the
fixture nor this patch raises the minimum ARM CPU requirement.

Remove this vendor when an upstream release annotates these helpers and
passes the same generic-baseline and runtime-dispatch checks.
