# Vendored dolby_vision 3.4.0

This directory is the crates.io `dolby_vision` 3.4.0 package, licensed under
MIT, from [quietvoid/dovi_tool](https://github.com/quietvoid/dovi_tool/tree/main/dolby_vision).
Plurx uses it for one thing: the Profile 7 → Profile 8.1 RPU rewrite in
`crates/plurx-core/src/transcode/dvconvert.rs`, which runs inside `plurxd`
on every video sample of a converted disc remux.

Plurx changes nothing about what a well-formed RPU parses or writes to. It
changes what the crate does with a malformed one. Three places sized an
allocation from a ue(v) count read straight out of the RPU with no check
that the count could fit in the bytes that remained, so a corrupted RPU — or
one made on purpose — could ask the allocator for tens of gigabytes, and
Rust's answer to a failed allocation is an abort, not an error; on the far
side of the muxer that abort is `plurxd`'s. Two more places panicked on
input a valid RPU is a few bits away from; those unwind the converting task
rather than the process (the release profile keeps `panic = "unwind"` and
`plurxd`'s hook reports them), and are refusals now for the same reason.

The `rpu_rewrite` fuzz target (`fuzz/parsers/fuzz_targets/rpu_rewrite.rs`,
P-02 M8) found the first of these within its first minute on 2026-09-24:
eight bytes of `tests/playback/dv-p7-rpu.hex` changed so that
`num_ext_blocks` read in the hundreds of millions, and
`CmV40DmData::with_blocks_allocation` asked for 0x603c2cfd0 bytes (~25.8 GB).
The input is kept as `tests/playback/dv-p7-rpu-hostile-ext-blocks.hex` and
pinned by `an_rpu_claiming_billions_of_extension_blocks_is_refused_without_allocating`
in `dvconvert`'s tests. The other two allocation sites were found by reading
the crate for the same shape; patches 2 and 3 are the target's second and
third findings; patches 4 and 5 came out of the branch's adversarial review.

| # | File | Change |
|---|---|---|
| 1 | `src/rpu/extension_metadata/mod.rs` | `DmData::parse` refuses a `num_ext_blocks` larger than the bits left divided by 9 (one block is at least ue(0) plus an 8-bit level) before `with_blocks_allocation` runs. |
| 1 | `src/rpu/rpu_data_mapping.rs` | `RpuDataMapping::parse` refuses a `num_pivots_minus2` whose pivots (each `bl_bit_depth` bits) cannot fit in the bits left, with a checked `+ 2`, before `vec![0; num_pivots]`. The `num_pieces` capacities further down derive from the same count and are bounded by it. |
| 2 | `src/rpu/rpu_data_mapping.rs` | `DoviPolynomialCurve::parse` refuses a curve with `poly_order_minus1 == 0` and `linear_interp_flag` set, where upstream stops with `unimplemented!()`. Found by the same target on its second run: two bits of the fixture turn a parse into a panic. |
| 3 | `src/rpu/extension_metadata/blocks/level8.rs`, `level9.rs`, `level10.rs` | Each `parse` refuses an `ext_block_length` outside the set the block knows (8: 10/12/13/19/25; 9: 1/17; 10: 5/21) before reading the block. Upstream read the block and then hit `unreachable!()` in `required_bits` during validation. The target's third finding, at ten thousand executions. |
| 4 | `src/rpu/extension_metadata/{mod,cmv29,cmv40}.rs`, `src/rpu/rpu_data_mapping.rs` | The pre-allocations those counts drive are capped as capacity hints (`MAX_EXT_BLOCKS_PREALLOCATED` = 64 blocks, `MAX_PIECES_PREALLOCATED` = 16 pieces). Patch 1's bounds scale with the input, and an fMP4 sample may carry a NAL of up to 2 GiB, so a count that fits the bits left could still reserve hundreds of bytes per input byte; a hint changes nothing about what parses. `dvconvert` adds the absolute ceiling (`MAX_RPU_NAL_BYTES` = 64 KiB) on its side. |
| 5 | `src/rpu/rpu_data_mapping.rs` | `RpuDataMapping::parse` refuses a piece whose `mapping_idc` differs from piece 0's in the same component. The struct holds one method per component and the writer emits it for every piece, but the parser accepted a mix and kept the last; a curve with one polynomial and one MMR piece then indexed past the polynomial's coefficient vectors in `write_hevc_unspec62_nalu` — reachable through `To81` on a MEL stream, whose mapping is kept. |
| 1 | `src/rpu/extension_metadata/blocks/reserved.rs` | `ReservedExtMetadataBlock::parse` refuses an `ext_block_length` longer than the bytes left before `vec![0; ext_block_length]`. Unreachable today (both block parsers bail on an unknown level first) and bounded anyway, so a future upstream that reaches it inherits the bound. |

Each refusal is an ordinary `anyhow` error, which `dvconvert` reports as
`DvConvertError::Unreadable` with the whole chain (`{error:#}`), so the
operator reads the count that was refused rather than the metadata version
it sat in. Well-formed RPUs are unaffected: every bound is one the RPU's own
bytes must already satisfy for the parse that follows to succeed, and the
caps are capacity hints. The crate's bit reader, `bitvec_helpers`, is
vendored beside this one for two overflows of its own.

Cargo records this package as path-sourced, which means cargo-audit skips it.
The weekly `rust-audit.yml` job uses `scripts/vendor-audit-lock` to restore this
exact release's registry source and checksum before a second advisory scan.
That check must remain until this directory is removed.

Source: <https://crates.io/crates/dolby_vision/3.4.0>

Only `Cargo.toml` and `src/` are load-bearing in Plurx's build. The retained
license, README, changelog, examples and benches are upstream provenance, not
an in-place test suite; the workspace excludes this directory deliberately.
Remove this vendor when an upstream release bounds these allocations; the
change is small enough to offer upstream as is.
