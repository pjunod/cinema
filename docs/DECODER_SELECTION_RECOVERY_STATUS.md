# Decoder selection and recovery — implementation status

**Status:** M0 implementation in progress · **Updated:** 2026-09-05 ·
**Integration branch:** `effort/decoder-selection-recovery` · **Baseline:**
`main` at `3d847b58b081dcb15a8d2e566d8d0ac1700882fd`

This is the live execution ledger for
[DECODER_SELECTION_AND_RECOVERY_PLAN.md](DECODER_SELECTION_AND_RECOVERY_PLAN.md).
It records what is merged, what was actually tested, and what remains unsafe.
An unchecked item is not implied by a nearby passing check.

## Current checkpoint

| Field | Current value |
|---|---|
| Milestone | M0 — capture baseline and freeze diagnostic qualification |
| Task branch | `codex/decoder-selection-m0` |
| Task PR | Not opened yet |
| Effort PR | Not opened yet |
| Working compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` via `rustup run 1.97.1` |
| Focused validation | 6 M0 tests and all 192 operations tests pass |
| Full PR validation | Pending adversarial review and fixes |
| Blocker | None; physical Apple/Android client and original-media runs remain later milestone evidence |

## Milestones

| Milestone | State | Exit evidence |
|---|---|---|
| M0 · baseline and diagnostic qualification | In progress | Inventory, fixtures, 6 focused tests, 192 operations tests, catalog lint, and pinned compile pass; review pending |
| M1 · explicit plan and facts | Not started | — |
| M2 · arguments and identity use one plan | Not started | — |
| M3 · owned observation and health receipts | Not started | — |
| M4 · mixed resource admission | Not started | — |
| M5 · durable budget and prepublication recovery | Not started | — |
| M6 · postpublication replacement and client intent | Not started | — |
| M7 · offline, shared cache, and handoff enforcement | Not started | — |
| M8 · fleet qualification and promotion | Not started | — |

## M0 source inventory

The production `hls_args` surface has three daemon callers. Core calls below
`mod tests` are argument regressions, not shipping construction paths.

| Caller | Owner | Current purpose | Migration obligation |
|---|---|---|---|
| `PrepublicationTranscodeRetry::build` | `crates/plurxd/src/transcode.rs` | Freezes the one-step retry arguments and fingerprint | Freeze a validated reason-specific alternate plan and one shared budget |
| `ProducerRunner::produce_into` | `crates/plurxd/src/transcode.rs` | Builds each resumable pre-transcode/offline part | Carry plan identity and health evidence across every part and assembly |
| `Manager::start_with_audio_offset` | `crates/plurxd/src/transcode.rs` | Builds the live HLS producer command | Resolve once from the held source and use that plan for command, identity, and admission |

The media subprocess ownership surface is broader than the three builders:

| Surface | Current state | Required owner/evidence |
|---|---|---|
| `spawn_ffmpeg` | Live, retry, and offline children; progress and stderr readers are detached | Return one owned observed-child handle and join diagnostics before qualification |
| `spawn_ffmpeg_pipe` | Fragmented copy stream; progress and diagnostics share stderr | Preserve the split while giving the owning attempt a complete observer result |
| `vodserve` / `vodgen` | VOD materialization/index-generation subprocesses outside `hls_args` | Classify media-producing decode work separately from probe/index-only work before M3/M7 |
| `pgs_overlay` | Bitmap-subtitle support subprocesses | Bind any video-decoding output producer to the same attempt receipt; exclude pure support probes |
| `copyseg` / `fragindex` | Copy segmentation and indexing | Record that copy/index-only work does not claim decoded-video qualification |
| `ffmpeg.rs`, `pipeprobe`, `subtitles`, `http/stream`, `http/offline`, `dv_disk` | Detection, probe, extraction, serving, or validation helpers | Do not mistake a successful helper process for producer health evidence |

Planning currently crosses these separate contracts:

- `transcode::decode_setup` rereads `PLURX_HWDECODE`, derives hardware decode
  from the encoder, and uses `heavy_source` for QSV/VA-API.
- `Pipeline::{decode_args, requires_software_decode, for_session, fallback}`
  owns renderer constraints and some decode overrides.
- `PipelineDigest` and `Recipe` identify FFmpeg/encoder/filter semantics but
  do not identify the decoder or diagnostic qualification contract.
- `GenerationManifest::{publish_controlled, open_verified_object}` and the
  daemon's local/shared cache readers authenticate bytes, but no producer
  health receipt exists.
- `ValidatedRetryRecipe`, `PreparedSuccessorAction`, and the
  `replace_failed_producer` action provide useful lifecycle boundaries. They
  do not yet persist one decoder-recovery budget or wire a decoder fault into
  an end-to-end successor.
- Settings already has a Developer section and tests its rule that there is no
  hidden server flag. Decoder enablement will extend that section.

## M0 diagnostic contract v1

The qualification harness is
`scripts/decoder-diagnostic-qualification`. Its fixture format is one monotonic
observation-time millisecond value, a tab, then one sanitized FFmpeg stderr
line. This is development evidence, not production parsing.

| Constant | Frozen M0 value | Result |
|---|---:|---|
| Video error window | 2,000 ms | Count selected primary records in `(now - 2,000 ms, now]` |
| Video error limit | 5 | The fifth retained record latches the fault |
| Automatic recovery limit | 1 | One budget across retry/replacement for a logical playback epoch |
| Diagnostic drain budget | 2,000 ms | Later implementation must join stderr completion inside this bound |
| Maximum retained line | 16 KiB | Oversize input makes qualification incomplete while draining continues |

The retained #913 capture supplies five explicit primary MPEG-4 video decoder
records in 361 ms, plus 49 messages hidden behind two legacy repeat summaries.
The explicit records meet the proposed threshold, but the compressed capture
is deliberately not eligible for automatic action. The qualified companion
expands only the five directly observed primary records into the expected
`repeat+level+error` shape and latches at 361 ms. The tolerant control contains
audio, unselected-video, encoder, filename, and subordinate messages; its five
selected primaries never place five timestamps in the open-lower-bound window.

Known grammar coverage is intentionally narrow: only
`[vist#INPUT:STREAM/codec] [dec:decoder] Error submitting packet to decoder`
is a counted primary. `No frame decoded?` is supporting evidence. Fatal
backend initialization grammars and every additional deployed FFmpeg family
remain unqualified until retained fixtures prove their attribution.

## FFmpeg and client qualification gaps

Read-only inventory captured 2026-09-05. The running container is the producer
that matters; a host binary is diagnostic context only.

| Node | Host FFmpeg | Running `plurxd` FFmpeg | Advertised container acceleration | Qualification |
|---|---|---|---|---|
| `nynuc` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `m6` | Not on host `PATH` | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `nuc4` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `nuc3` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| Local Apple build host | Homebrew FFmpeg cannot start: missing `libx265.216.dylib` | No deployed daemon inspected | Unknown | Unavailable |

All four containers advertise software MPEG-4 plus QSV/CUVID families. None of
those names proves a usable device, correct surface transfer, metadata
preservation, or decoded pixels. No fleet backend class is qualified in M0.

Postpublication recovery is likewise unqualified for web, Apple, and Android.
The current protocol has action, replay, acknowledgment, preparation, and
commit machinery; M6 still owes real successor production and physical client
evidence. Until then, the Developer card must describe the missing evidence and
refuse `full` recovery rather than pretend it is active.

## Decisions and deviations

| Date | Decision | Reason |
|---|---|---|
| 2026-09-05 | Use one effort branch with M0–M8 task PRs | Required large-effort lane; keeps incomplete intermediate states out of `main` |
| 2026-09-05 | Decoder activation belongs in Settings → Developer | Operator request and existing UI convention; no compile feature or hidden environment gate |
| 2026-09-05 | Unsafe requested modes are explicit refusals | Safety prerequisites stay visible without silently accepting a nonfunctional setting |
| 2026-09-05 | Treat container FFmpeg 5.1.9 as deployed evidence | It runs the producer; host 8.0.1 does not define container diagnostics |
| 2026-09-05 | Keep the proposed 2 s / 5-record threshold | #913 reaches five explicit records in 361 ms; tolerant controls do not trigger |
| 2026-09-05 | Do not expand legacy repeat summaries into timestamps | Their timing is unknowable, so expansion would manufacture recovery evidence |

## Validation ledger

No passing run is recorded until its command finishes on the named tree. Each
task PR gets adversarial agent review, findings are fixed, then one full suite
runs on the corrected head before merge. During development, the effort fast
lane and focused tests provide earlier feedback.

| Commit/tree | Command | Result |
|---|---|---|
| M0 pre-review tree | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 6 tests |
| M0 pre-review tree | `make operations-check` | Pass · 192 tests; rerun outside restricted socket sandbox |
| M0 pre-review tree | `make validation-lint` | Pass · 23 points, 28 checks, 1,376 files |
| M0 pre-review tree | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| M0 pre-review tree | `git diff --check` | Pass |
| Pending | Full PR suite after review fixes | Not run |

## Remaining evidence before release

- Original #913 media on an Apple VideoToolbox node, compared with software
  decode while retaining the hardware encoder.
- Qualified FFmpeg diagnostic output from each supported build/backend class.
- Pixel, metadata, startup, concurrency, and recovery-latency evidence for the
  fleet workload matrix.
- Web, Apple, and Android prepare/readiness/commit/retirement runs with one
  replacement, preserved position/pause/tracks/grade, and no reopen loop.
- Exact-tree Main promotion qualification after current `main` is merged into
  the frozen effort branch.
