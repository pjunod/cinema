# Decoder selection and recovery — implementation status

**Status:** M0 adversarial findings being resolved · **Updated:** 2026-09-05 ·
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
| Task PR | [#915](https://github.com/pjunod/plurx/pull/915) · draft |
| Effort PR | Not opened yet |
| Working compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` via `rustup run 1.97.1` |
| Focused validation | 14 diagnostic/inventory tests and 1 normalized argument-baseline test pass at `2d4900e1` |
| Full PR validation | Pending adversarial review and fixes |
| Blocker | None; physical Apple/Android client and original-media runs remain later milestone evidence |

## Milestones

| Milestone | State | Exit evidence |
|---|---|---|
| M0 · baseline and diagnostic qualification | Second review | [PR #915](https://github.com/pjunod/plurx/pull/915); first-round findings fixed at `cae1aa43` |
| M1 · explicit plan and facts | Not started | — |
| M2 · arguments and identity use one plan | Not started | — |
| M3 · owned observation and health receipts | Not started | — |
| M4 · mixed resource admission | Not started | — |
| M5 · durable budget and prepublication recovery | Not started | — |
| M6 · postpublication replacement and client intent | Not started | — |
| M7 · offline, shared cache, and handoff enforcement | Not started | — |
| M8 · fleet qualification and promotion | Not started | — |

## M0 frozen source inventory

The machine-checked inventory is
[`decoder-selection-m0-inventory.toml`](../tests/playback/decoder-selection-m0-inventory.toml).
It stores a stable identifier, source file, exact anchor, classification, and
migration obligation for every row summarized below. The focused inventory
test fails when an anchor moves or a fourth production `hls_args` call appears.

The production `hls_args` surface has three daemon callers. Core calls below
`mod tests` are argument regressions, not shipping construction paths.

| Caller | Owner | Current purpose | Migration obligation |
|---|---|---|---|
| `PrepublicationTranscodeRetry::build` | `crates/plurxd/src/transcode.rs` | Freezes the one-step retry arguments and fingerprint | Freeze a validated reason-specific alternate plan and one shared budget |
| `ProducerRunner::produce_into` | `crates/plurxd/src/transcode.rs` | Builds each resumable pre-transcode/offline part | Carry plan identity and health evidence across every part and assembly |
| `Manager::start_with_audio_offset` | `crates/plurxd/src/transcode.rs` | Builds the live HLS producer command | Resolve once from the held source and use that plan for command, identity, and admission |

The media subprocess ownership surface is broader than the three builders.
The distinction between owner, consumer, and support process is deliberate:

| Inventory ID | Current state | Required owner/evidence |
|---|---|---|
| `process.observed_ffmpeg` | `transcode::spawn_ffmpeg` owns live, retry, and offline HLS children; progress and stderr readers are detached | Return one owned observed-child handle and join diagnostics before qualification |
| `process.fragmented_ffmpeg` | `transcode::spawn_ffmpeg_pipe` owns fragmented-copy children | Preserve its pipe split while giving the attempt a complete observer result |
| `process.vod_generation` | `vodserve::spawn_generation` owns the FFmpeg copy producer and hands stdout onward | Classify copied output separately and stop discarding stderr |
| `process.vod_head_regeneration` | `vodserve::regenerate_init_head` owns a bounded FFmpeg head probe | Keep it probe/head-only; it cannot attest a complete producer |
| `process.vod_pipe_consumer` | `vodgen::run` consumes the pipe; it does not spawn production FFmpeg | Do not create a second child or health owner here |
| `process.pgs_demux` | `pgs_overlay::prepare_stage` copies one subtitle stream to SUP | Keep this support process out of video health evidence |
| `copyseg` / `fragindex` | Copy segmentation and indexing do not decode video | Never claim decoded-video qualification |
| Detection and extraction helpers | `ffmpeg.rs`, `pipeprobe`, `subtitles`, `http/stream`, `http/offline`, and `dv_disk` probe, extract, serve, or validate | A successful helper is not producer health evidence |

Renderer correctness is distributed across six existing methods. Candidate
validation must consume all six rather than duplicating their decisions:

| Inventory ID | Frozen constraint |
|---|---|
| `renderer.pairing` | `Pipeline::pairs_with` restricts vendor graphs and measured HDR10 encoders |
| `renderer.dynamic_range` | `Pipeline::handles` restricts HDR, HLG, and Dolby Vision inputs |
| `renderer.decode_arguments` | `Pipeline::decode_args` claims QSV or VA-API input surfaces for vendor graphs |
| `renderer.software_requirement` | `Pipeline::requires_software_decode` preserves Dolby Vision frame side data |
| `renderer.session_selection` | `Pipeline::for_session` applies source, encoder, and workload constraints |
| `renderer.fallback` | `Pipeline::fallback` refuses grade- or Dolby-incompatible fallback |

Cache identity and delivery cross distinct write, lookup, and serve paths:

| Path class | Frozen owners | M7 obligation |
|---|---|---|
| Node-local write | `cache.local_publish`, `cache.manifest_publish`, `cache.manifest_capability_publish` | Publish decoder plan and completed health receipt with the generation |
| Shared write | `cache.shared_publish` | Copy and reverify that same receipt; never mint qualification during replication |
| Local/shared lookup | `cache.location_read`, `cache.local_location_read`, `cache.speculative_hit`, `cache.offer_verification`, `cache.shared_read_prepare` | Require compatible plan and complete receipt before calling bytes reusable |
| Live cache serving | `cache.session_serve`, `cache.playlist_read`, `cache.object_read` | Install and serve the exact authenticated generation under its read fence |
| Offline ownership | `offline.prepare`, `offline.publish_ready` | Keep one job-scoped recovery budget and refuse `ready` without producer evidence |
| Offline lookup | `offline.generation_cache`, `offline.location_validation` | Cache decoded manifests without bypassing local/shared receipt checks |
| Offline serving | `offline.playlist`, `offline.segment` | Serve authenticated snapshots/handles from the qualified generation |

Planning currently also crosses `transcode::decode_setup`, `PipelineDigest`,
`Recipe`, `ValidatedRetryRecipe`, `PreparedSuccessorAction`, and
`replace_failed_producer`. None currently provides a complete decoder plan,
producer-health receipt, or durable one-shot decoder-recovery budget.

## M0 argument and media controls

The test-only snapshot
[`decoder-selection-m0-args.json`](../tests/playback/decoder-selection-m0-args.json)
freezes normalized token arrays from the current `hls_args` builder. It
normalizes only the source and output paths; option order and every other token
remain exact.

| Case | Decoder/renderer/encoder baseline frozen |
|---|---|
| `software-sdr-h264` | Software input, CPU renderer, x264 |
| `qsv-light-h264` | Software input, CPU renderer/upload, QSV encoder |
| `qsv-heavy-hevc-hdr` | QSV input, CPU HDR renderer/download-upload, QSV encoder |
| `qsv-vendor-renderer` | QSV input and `vpp_qsv`, QSV encoder |
| `nvenc-light-h264` | CUDA input, CPU renderer, NVENC encoder |
| `videotoolbox-avi-mpeg4` | VideoToolbox input, CPU renderer, VideoToolbox encoder; this is the incident-sensitive legacy behavior |

Real-media controls are evidence, not inferred success. Missing rows remain
prerequisites for M8 rather than being filled with synthetic claims.

| Control | Available evidence | M0 result / prerequisite |
|---|---|---|
| #913 MPEG-4 Part 2 ASP/XVID AVI, MP3 audio, 624×352 at 25 fps | Sanitized FFmpeg 7.1.4-Jellyfin diagnostic capture | Five explicit selected-video primaries in 361 ms; original media is not present, so VideoToolbox/software output comparison remains required |
| Synthetic malformed rawvideo on host FFmpeg 8.0.1 | Exact `repeat+level+error` replay fixture | Structured stream/decoder/severity grammar is action-qualified for the retained shape only |
| Synthetic malformed rawvideo in deployed FFmpeg 5.1.9 container | Exact retained replay fixture | Top-level stream error lacks selected-stream attribution; observation-only, not automatic action |
| Representative H.264, HEVC/HDR, Dolby Vision, subtitles, and multi-stream files | Existing repository tests do not constitute decoder-backend qualification | Preserve source facts and add pixel/metadata/startup controls in M8 |

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

The harness streams bounded binary records instead of loading a capture into
memory. An oversize, malformed, invalid-UTF-8, or non-monotonic record marks
the observation incomplete but is drained so later diagnostics remain
visible. A repeat summary belongs only to the immediately preceding classified
record; a repeat after another repeat is ambiguous and cannot authorize action.
Detection latency begins at the oldest record in the window that actually
triggered the fifth error, not at a stale earlier error.

The retained #913 capture supplies five explicit primary MPEG-4 video decoder
records in 361 ms, plus 49 messages hidden behind two legacy repeat summaries.
The explicit records meet the proposed threshold, but the capture has neither
severity markers nor uncompressed repeat timing and is deliberately not
eligible for automatic action. The qualified FFmpeg 8 companion preserves the
observed five timestamps in the verified grammar and latches at 361 ms. The
tolerant control contains audio, unselected-video, encoder, filename, and
subordinate messages; its five selected primaries never place five timestamps
in the open-lower-bound window.

Known action grammar coverage is intentionally narrow:
`[vist#INPUT:STREAM/codec @ ADDRESS] [dec:decoder @ ADDRESS] [error] Error
submitting packet to decoder` is a counted primary. Addresses are sanitized to
`<address>` in retained fixtures. `No frame decoded?` is supporting evidence.
The deployed FFmpeg 5.1 shape, fatal backend initialization grammars, and every
additional build/backend grammar remain observation-only until retained
fixtures prove selected-stream attribution and severity.

## FFmpeg and client qualification gaps

Read-only inventory captured 2026-09-05. The running container is the producer
that matters; a host binary is diagnostic context only.

Exact image IDs and hashes of the container binary, build configuration, and
relevant decoder inventory are retained in
[`fleet-ffmpeg-2026-09-05.toml`](../tests/playback/decoder-health/fleet-ffmpeg-2026-09-05.toml).

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

Postpublication recovery is likewise unqualified. The clients advertise only
`hold`, `retry_resource`, and `terminal`; their existing parsers intentionally
reject undeclared `prepare_replacement`. Server preparation/commit machinery
does not make a client perform a two-player handoff.

| Client | Declared actions | Parses `prepare_replacement` | Two players and readiness | Commit/retirement evidence | Current effective recovery |
|---|---|---|---|---|---|
| Web | `hold`, `retry_resource`, `terminal` | No | No | No | At most `prepublication` after server qualification |
| Apple | `hold`, `retry_resource`, `terminal` | No; unit test expects terminal protocol error | No | No physical run | At most `prepublication` after server qualification |
| Android | `hold`, `retry_resource`, `terminal` | No; unit test expects terminal protocol error | No | No physical run | At most `prepublication` after server qualification |

The visible Settings → Developer values are operator-requested upper bounds,
not a claim that every node or session can execute them. Each node computes an
effective plan policy from its current local capability/grammar receipt. Each
playback computes an effective recovery mode from that node result, owned
observation/resource/store readiness, and the actual client's advertised and
qualified behavior. The card must show requested value, per-node readiness,
session-effective value, and every bounded refusal. Unsupported sessions end
explicitly; they do not silently run an unqualified automatic replacement.

## Adversarial review ledger

Two independent agents reviewed the first M0 PR head before the full suite.
Their initial verdict was request changes; the listed repairs are committed
and in second-pass review on PR #915. GitHub had deleted the temporary effort
base and closed #914; the same effort and task refs were restored, and #915 is
the active review record.

| Finding | Resolution on working tree |
|---|---|
| Severity token was matched in the wrong position and addresses were omitted | Grammar and fixtures now preserve FFmpeg 8 context/address/severity order; FFmpeg 5 is explicitly unqualified |
| Fixture reader allocated the whole input | Bounded streaming reader drains oversize tails and records coverage loss |
| Latency began at the first historical error | Latency begins at the oldest record in the triggering window |
| Normalized baseline argv and media controls were absent | Six exact argv snapshots plus explicit real-media/prerequisite matrix added |
| Client replacement support was overstated | Web/Apple/Android capability matrix records parse-only and physical-evidence gaps |
| Activation semantics depended on “the node/current client” | Persisted values are requested upper bounds; effective mode is node/session-local and visible |
| Legacy repeats were counted without provenance | Only an immediately preceding classified record owns a repeat summary; ambiguous summaries refuse action |
| Producer/cache inventory was inaccurate and not auditable | Machine-checked owner/consumer/renderer/local/shared/offline inventory added; `vodgen` corrected to consumer |

## Decisions and deviations

| Date | Decision | Reason |
|---|---|---|
| 2026-09-05 | Use one effort branch with M0–M8 task PRs | Required large-effort lane; keeps incomplete intermediate states out of `main` |
| 2026-09-05 | Decoder activation belongs in Settings → Developer | Operator request and existing UI convention; no compile feature or hidden environment gate |
| 2026-09-05 | Persist requested upper bounds; compute effective modes per node/session | One settings writer cannot prove every node or client; qualified peers should not be disabled by an unqualified peer |
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
| `a9cb879b` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 6 tests |
| `a9cb879b` | `make operations-check` | Pass · 192 tests; rerun outside restricted socket sandbox |
| `a9cb879b` | `make validation-lint` | Pass · 23 points, 28 checks, 1,376 files |
| `a9cb879b` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| `a9cb879b` | `git diff --check` | Pass |
| `2d4900e1` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 14 tests |
| `2d4900e1` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test |
| `2d4900e1` | `make operations-check` | Pass · 200 tests; run outside restricted socket sandbox |
| `2d4900e1` | `make validation-lint` | Pass · 23 points, 28 checks, 1,381 files |
| `2d4900e1` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| `2d4900e1` | `git diff --check` | Pass |
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
