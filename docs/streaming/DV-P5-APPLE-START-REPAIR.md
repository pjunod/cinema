# Dolby Vision Profile 5 startup — repairing the muxer's incomplete-array claim

**Status:** implemented and locally qualified; adversarial review approved; device acceptance pending · **Date:** 2026-10-13

Companion to [PLAYBACK.md](../PLAYBACK.md) (the delivery path). This record
explains the Apple TV failure for *Taylor Tomlinson: Prodigal Daughter*, the
bounded repair, and what its evidence does and does not establish.

## The server fails before Apple TV receives a playable stream

At 21:33:54 EDT on October 13, media1 selected copy-video HLS for file 91.
The client advertised Dolby Vision profiles 5 and 8; the decision preserved
Profile 5. The VOD index was unavailable (`vod_index_pending`), so startup
used the temporary rolling copy path. Its producer failed before publication:

```text
copy segmenter produced invalid HEVC configuration:
an out-of-band HEVC decoder configuration has no complete VPS/SPS/PPS set
```

The master playlist returned 502 (`session_failed`, not retryable). The Apple
client reported `Server returned 502`. Retrying produced the same failure;
a subsequent 410 followed session retirement. Neither message establishes a
TV decoder failure: the server refused the initialization segment first.

A six-second copy with the deployed Jellyfin FFmpeg reproduced the bytes.
The Matroska source has a 23-byte hvcC with no parameter arrays. The production
`hevc_mp4toannexb,extract_extradata` filters recover VPS/SPS/PPS, but the emitted
`dvh1` descriptions mark all three arrays `array_completeness=0`. There are
two equivalent sample descriptions; one PPS differs only by a trailing zero.
Collapsing them preserves the incorrect flags and the strict publication
validator rejects them.

Jellyfin's [MOV muxer](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/libavformat/movenc.c)
requests complete arrays for `hvc1` but not `dvh1`. An otherwise identical
`hvc1` experiment set all three flags but lost the P5 Dolby Vision record.
Changing the tag alone is therefore not the repair.

## Repair only a proven matching configuration

The fMP4 promotion boundary captures a separate canonical opening-sample
VPS/SPS/PPS tuple for incomplete `dvh1` descriptions. The repair requires
both the record and that tuple to contain all three types, and their sorted,
deduplicated NAL sets to match in both directions after trimming trailing
zero padding. The parser checks complete array bounds, nonempty arrays and
matching array/NAL types before changing any flags. It never repairs ordinary
`hvc1`, `hev1` or `dvhe` descriptions. The existing publication validator
remains strict.

Only the parameter-array completeness bits change. Dolby Vision records,
RPUs, picture data and audio are preserved. The ordinary segment merge
continues to remove the promoted in-band VPS/SPS/PPS from `dvh1` media.
Known retaining-copy recipes cannot use this repair: the live producer and
both VOD creation paths refuse that combination before publication.

The opening tuple is persisted separately from missing-array promotion in
`PromotionInputs.dvh1_parameter_sets`, with an empty serde default for old
rows. A separate default-false malformed-evidence marker prevents a truncated
later sample from being mistaken for an absent repetition. Planned generations replay it without consulting their landing GOP.
For index constancy, a later absent tuple is permitted; a present conflicting
tuple is not. Canonicalization makes ordering, repeated identical NALs and
trailing-zero padding immaterial. This is header consistency evidence, not
a new full-film parameter-stability proof; existing source gates still apply.

A `--plurx-dvh1-completeness=1` token changes only the affected non-retaining
preserved-DV recipe identity. Previously cached `Unsupported` index outcomes
are terminal and otherwise would survive upgrading. HDR10, compatible-base
P8, stripped and retaining recipes keep their previous identities. Old nodes
cannot regenerate an init using an unknown proof field; existing promotion
identity checks refuse that version skew rather than serving different bytes.

## Review findings and their disposition

| Finding | Disposition |
|---|---|
| Cached failed indexes never retry | Scope a new recipe identity to the repaired path. |
| Existing promotion intentionally drops configured NAL types | Store separate opening proof with backward-compatible deserialization. |
| Later GOPs need not repeat parameter sets | Ignore only absence; preserve contradictory nonempty evidence. |
| Known retaining copies bypass the source stability gate | Refuse their use of the completeness repair on live and VOD paths. |
| Presence alone can hide malformed or contradictory arrays | Parse strictly, require exact two-sided equality, collect offsets before mutation. |
| New capture could reclassify unrelated multi-description errors | Leave structural selection failures with their existing validator; run the full fMP4 regression group. |
| Regeneration and metadata may drift | Test persisted replay, duplicate descriptions, DOVI bytes, RPU/picture preservation and merge behavior. |

## Qualification and remaining acceptance

The required local loop uses Rust 1.97.1. Synthetic regressions cover positive,
negative and persisted-input cases. Opt-in private capture tests exercise the
real failing muxer output through the core promotion, rolling copy publisher
and fragment-index/rendition identity paths. The copyrighted capture remains
outside the repository; the tests accept its path through
`PLURX_DVH1_CAPTURE`. The indexer capture must map video only (`-an`);
the playback capture includes the selected audio track.

Results, exact commands and final review disposition are recorded below. A successful source repair and merged PR do not prove deployment
or physical Apple TV playback; those remain separate acceptance evidence.

The single adversarial reviewer approved the revised code with no unresolved
findings, conditional on passing compiler/tests and the merge gate. Local core
results: 90 fMP4 tests passed (three optional capture tests skipped), and all
four selected DV completeness tests passed when the private capture was
provided. Decoding the original and repaired six-second capture produced 145
identical frame hashes, including timestamps; copied audio SHA-256 matched.
This verifies sample preservation, not calibrated Dolby Vision display output.

| Private capture | SHA-256 |
|---|---|
| Original production-shaped pipe | `88ba617494ff71f458ed6f5c84d87dde242c4faace92d4791a17d93adcc8aa55` |
| Repaired segmented stream | `edfc2aa1f08bae13ec188a320a0f09bf71512f72b3391c2a1f505c6555dbb2eb` |
| Decoded frame-hash listing, identical for both | `a1c7fc017da22045b2c2712d5115e1a0eead3fcf52a6fbaf553d8264bb33de22` |

All three selected daemon regressions passed on the production-shaped
video-only capture: rolling publication, explicit retaining-copy refusal,
index constancy and persisted identity replay, and affected-only failure-cache
invalidation. An initial index harness attempt used the playback capture with
an audio track and correctly failed the indexer's video-only input contract;
the corrected capture follows the actual index recipe.

All-target compilation and workspace Clippy with warnings denied passed on
Rust 1.97.1. Documentation/index and infrastructure-name checks passed all 18
tests; the validation catalog and formatting checks passed. The merge gate
is recorded on the pull request.

```bash
cargo test -p plurx-core --lib --features hiqlite-store fmp4::tests
PLURX_DVH1_CAPTURE=<playback-capture> cargo test -p plurx-core --lib --features hiqlite-store dvh1_completeness -- --include-ignored
PLURX_DVH1_CAPTURE=<video-only-index-capture> cargo test -p plurxd --bin plurxd dvh1_completeness -- --include-ignored
cargo check -p plurxd --all-targets
cargo clippy --workspace --all-targets -- -D warnings
python3 -m unittest tests.operations.test_docs_index tests.operations.test_infra_names
```
