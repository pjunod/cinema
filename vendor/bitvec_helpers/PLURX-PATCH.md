# Vendored bitvec_helpers 4.0.2

This directory is the crates.io `bitvec_helpers` 4.0.2 package, licensed under
MIT, from [quietvoid/bitvec_helpers](https://github.com/quietvoid/bitvec_helpers).
It is the bit reader under `dolby_vision` (also vendored, see
`vendor/dolby_vision/PLURX-PATCH.md`), so every field of a Dolby Vision RPU
that `plurxd` rewrites passes through it. Plurx builds it with only the
`bitstream-io` feature, as `dolby_vision` does.

One patch, in `src/bitstream_io_impl/bitstream_io_reader.rs`:

| # | Function | Change |
|---|---|---|
| 1 | `read_ue` | Refuses an Exp-Golomb code with 64 or more leading zero bits as `InvalidData`. Upstream computed `1 << leading_zeroes`, which overflows at 64: a panic in a build with overflow checks (the fuzz build, and any debug build) and a wrong value in the release `plurxd`. Sixty-four zero bits in a row are never a valid code. |
| 1 | `read_se` | Maps the code to its signed value with integer arithmetic and a checked `i64` conversion instead of `(code_num + 1) as f64 / 2.0` and `-(m as i64)`, both of which overflow at the top of the range (63 leading zeroes). The result for every representable code is unchanged: `m = ⌈code_num / 2⌉`, negated for even codes. |

Found by the adversarial review of `plan/P-02-2` while the `rpu_rewrite`
fuzz target was being added: eight zero bytes at the header's
`coefficient_log2_denom` reach `read_ue` before any CRC check, so the nightly
campaign - which runs with debug assertions on, as cargo-fuzz does by
default - would have gone red on the first night for a panic the shipped
binary does not have. Vendoring the reader keeps that campaign honest and
gives the release binary a refusal where it had a silently wrong value.

Cargo records this package as path-sourced, which means cargo-audit skips it.
The weekly `rust-audit.yml` job uses `scripts/vendor-audit-lock` to restore this
exact release's registry source and checksum before a second advisory scan.
That check must remain until this directory is removed.

Source: <https://crates.io/crates/bitvec_helpers/4.0.2>

Only `Cargo.toml` and `src/` are load-bearing in Plurx's build. The retained
license and README are upstream provenance; the workspace excludes this
directory deliberately. Remove this vendor when an upstream release carries
the same two checks; they are small enough to offer upstream as is.
