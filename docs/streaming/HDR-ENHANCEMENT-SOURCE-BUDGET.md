# HDR enhancement source budget — preserve the selected video rate

**Status:** implemented, source reviewed and locally validated; picture acceptance
remains open · **Updated:** 2026-11-08 (America/New_York) · **Owner:** Codex

Companion to [the processing ledger](DV_HDR_PROCESSING_STATUS.md) (serving and
picture-acceptance limits) and [offline authoring](../../tools/dv_processing/AUTHORING.md)
(packet and metadata association). This records the source-budget correction;
it does not qualify final enhancement fidelity.

## An Original enhancement keeps the measured video budget

The previous reconstructed P8.1 target clipped the container average at
60,000 kb/s, floored it at 1,000 kb/s, and invented 60,000 kb/s for missing
measurements. HDR10-E could instead inherit the ordinary 20,000 kb/s 2160p
rung. Those choices could reduce the source budget solely because enhancement
was enabled.

When Original would otherwise copy the source, the enhancement's re-encode
now keeps a measured video budget, using the selected held video stream's
positive bitrate where available. A multi-HEVC source does not
use a base-layer-only rate as the combined FEL budget. Otherwise the positive
container average, or file size divided by duration, is a conservative video
upper bound. Conversion rounds upward to whole kb/s with checked arithmetic.
A missing budget stays unresolved. An explicitly selected encode rung keeps
its own rate; native-copy compatibility is checked again against current
capabilities, learned limits, audio and source facts.

## One frozen envelope binds planning to the encoded result

| Boundary | Contract |
|---|---|
| Candidate and admission | Advertise actual average and peak video plus canonical audio cost; replay only a current server-derived recipe. |
| Encoder | Preserve the target and prefer 1.5× peak / 2× CPB on a legal Main10 High-tier HEVC level through 6.2. If those preferences alone exceed the highest legal level, bound peak or CPB to that level. Independent decoder/delivery upper bounds remain authoritative; an incompatible target stays unresolved. |
| Dolby Vision | Select DV level 1–13 from output raster, cadence and peak; completed signalling reports the level observed from the authored init. |
| Headers and authoring | Check actual hvcC, VPS/SPS level/profile/tier, available HRD and authored DV configuration against the envelope. Absence of HRD is recorded separately from observing a matching signalled buffer. |
| Qualification | Bind receipts to backend, export raster/cadence/stream, rate envelope and source size/mtime. One encoder cannot consume another encoder's measurement. |
| Title worker | Keep bounded peer requests/verdicts; retain broker source custody and both pipe partners through pause, cancellation and reap. |
| Installed helpers | Authenticate changed `author_p81.c` in both manifest producer and daemon verifier: 18 source inputs, libplacebo ABI 374. This is the existing main helper architecture. |

The highest-level fallback uses the codec's BR/CPB ceilings to bound automatic
VBV preferences while retaining the measured video target. A target above the
legal coded maximum remains unresolved. It never raises a declared client or
delivery upper bound.

The level guard accepts the envelope's exact vocabulary and rejects unknown
or duplicate level arguments. The offline author takes all five envelope
fields explicitly; command-line values cannot relabel incompatible encoded
pictures. Existing source-custody methods, cache destination helpers and
queue schema regressions are preserved.

## Validation and remaining acceptance

The Rust 1.97.1 compiler loop was established with a capped compile-and-run
control before source edits. Four helper manifest packaging controls pass.
The actual helper bundle builds with ABI 374 and all 18 source inputs; its
source offers, manifest and four executable loader checks pass. Actual C
authoring controls preserve packet bytes, decoded YUV, associated RPUs and
timing for six synthetic 64×64 pictures at HEVC High 4.1 and 6.2, plus a
uniform fragmented-MP4 control. These checks exercise authoring mechanics.
Workspace and isolated standalone/cluster checks and Clippy pass with Rust
1.97.1; formatting passes. All 32 named regressions pass on the integrated
source: 14 core tests, 17 daemon tests and one Store contract. That contract
completes memory, file and actual three-voter Hiqlite backends.

Focused validation covers source-budget ceiling arithmetic and missing rates,
odd-rate peak replay, legal level escalation, actual headers and authoring,
including captured FFmpeg 8.1.3/libx265 High-tier HRD at levels 4.1 and 6.2,
backend-specific receipt requests, reduced-raster qualification, the eight/nine
thread admission boundary, process-group cleanup, current ABI 374 bundle
capture and replicated variant retention. Replicated Store evidence requires
`hiqlite-contract-tests` (which includes `hiqlite-store`); enabling
`hiqlite-store` alone does not run the three-voter contract fixture.

Rate preservation is necessary for this correction. It does not establish
lossless re-encoding, final picture fidelity, physical display acceptance or
real-time performance. Those remain in the processing ledger and its owning
quality investigation.
