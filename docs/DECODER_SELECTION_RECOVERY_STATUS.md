# Decoder selection and recovery — implementation status

**Status:** M1 diagnostic full-suite repairs in progress · **Updated:** 2026-09-06 ·
**Integration branch:** `effort/decoder-selection-recovery` · **Authoritative task base:**
Forgejo `main` at `4a6a0268bd314ad5587cb3037f12ebd992c0074e`

The original M0 research baseline was `main` at
`3d847b58b081dcb15a8d2e566d8d0ac1700882fd`. It remains useful as historical
content evidence, but no command or review performed on that tree is presented
as exact-tree evidence for the current Forgejo base.

This is the live execution ledger for
[DECODER_SELECTION_AND_RECOVERY_PLAN.md](DECODER_SELECTION_AND_RECOVERY_PLAN.md).
It records what is merged, what was actually tested, and what remains unsafe.
An unchecked item is not implied by a nearby passing check.

## Current checkpoint

| Field | Current value |
|---|---|
| Milestone | M1 — explicit plans and bound facts |
| Task branch | `codex/decoder-selection-m1` |
| Task PR | Not opened; the corrected branch will be published to Forgejo after exact-head approval and a clean full-suite qualification |
| Effort PR | Not opened yet |
| Working compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` via `rustup run 1.97.1` |
| Focused validation | M1 code head `c03a98b3`: selector matrix 34/34, macOS bound FFprobe collector 14/14, Linux collector 15/15, and neutral observation policy 1/1 pass; default-feature Clippy denies warnings |
| Exact receipt | `8cc77fa6`: history 1,353, catalog 24/30/1,431, operations 211/211, formatting, pinned all-target workspace compile, status contract, and effort-base diff check pass |
| Full PR validation | Exact code head `59d0a4d1` passed `make validate-full`: 23 passed, 0 failed, 2 declared skips for M0. Historical pre-rebase head `01368ce1` remains history only. M1 diagnostic head `bb25576c`: 20 passed, 3 failed, 2 declared skips; the rejected run found stale ownership counts and two test-only Clippy findings, then exhausted disk during cluster compilation after all preceding cluster tests passed |
| Blocker | None; generated build artifacts were reclaimed, the diagnostic findings are being repaired, and only a later clean exact-head run can qualify M1 |

## Milestones

| Milestone | State | Exit evidence |
|---|---|---|
| M0 · baseline and diagnostic qualification | Merged | [Forgejo #62](http://192.168.4.7:3000/noirr/plurx/pulls/62) fast-forwarded qualified receipt head `a8bbe574` into the effort after two final approvals and the Forgejo effort gate |
| M1 · explicit plan and facts | Diagnostic repairs | Code head `c03a98b3` and exact receipt `8cc77fa6` passed focused and fast validation; diagnostic full-suite head `bb25576c` rejected stale ownership counts and two test-only Clippy findings, while cluster compilation later exhausted disk rather than failing a test |
| M2 · arguments and identity use one plan | Not started | — |
| M3 · owned observation and health receipts | Not started | — |
| M4 · mixed resource admission | Not started | — |
| M5 · durable budget and prepublication recovery | Not started | — |
| M6 · postpublication replacement and client intent | Not started | — |
| M7 · offline, shared cache, and handoff enforcement | Not started | — |
| M8 · fleet qualification and promotion | Not started | — |

## M1 explicit plan and bound-fact extraction

M1 adds pure `DecodeFacts`, `DecodeCapabilities`, `DecodePolicySnapshot`,
`AttemptRestrictions`, `ResolvedDecode`, and `ResolvedTranscode` contracts. A
single selector validates complete decoder, renderer, encoder, surface, source,
and presentation semantics before returning a plan. It retains the current
legacy preference order without making advertised decoder names qualified
evidence, and keeps the MPEG-4/VideoToolbox compatibility exclusion independent
of container and profile.

The daemon collector discovers and fingerprints the configured FFprobe binary,
then executes a private snapshot of those validated bytes: Linux launches the
retained descriptor and macOS protects the snapshot inode from mutation and
replacement before launching its path. Every probe owns a process session so
timeout or cancellation signals the complete wrapper process group and reaps
the direct child before releasing the already-held source descriptor's
exclusive offset lease. The supported configured wrapper contract requires
probe descendants to remain in that assigned process session. Lease admission
is charged to the same end-to-end deadline. Legacy video ordinals are resolved to
absolute input indices, and file-level catalog metadata is merged only when the
selection is the catalog's first non-attached playable stream. The fact digest
binds source and selected-stream facts; the bounded cache key additionally
binds the FFprobe build. Source, held build, private snapshot, and configured
build path are revalidated immediately before cache publication.

The global hash worker and probe singleflight retain their owned admission when
a waiter times out or cancels, preventing detached work from fanning out.
Deadlines, oversize output, changed source identity, and replaced probe binaries
fail closed. Because M1 only observes facts, its two-second subdeadline logs and
continues the unchanged legacy production route, and the time spent observing
is added back to the producer deadline so it cannot consume the legacy FFmpeg
startup budget. Already assembled output is checked before probing. Command
construction remains an M2 migration.

Current focused code evidence on M1 code head `c03a98b3`:

- `cargo test -p plurx-core --test decoder_selection`: 34 passed.
- macOS `cargo test -p plurxd decode_facts::tests:: -- --nocapture`: 14 passed.
- pinned Linux 1.97.1 container `cargo test --locked -p plurxd decode_facts::tests:: -- --nocapture`: 15 passed, including descriptor-exec and crossed-fd assignment.
- `cargo test -p plurxd transcode::tests::neutral_decoder_fact_deadline_retains_the_legacy_route -- --nocapture`: 1 passed.
- `cargo clippy -p plurxd --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and the working-tree whitespace diff pass.

Exact validation receipt `8cc77fa6` adds:

- `make validation-lint`: 24 points, 30 checks, and 1,431 audited files pass.
- `make history-check`: 1,353 corrective commits pass.
- `make operations-check`: 211 passed.
- `cargo check --workspace --locked --all-targets` on Rust 1.97.1: passed.

The corrected head closes both independent reviews: selected-stream catalog
binding, first-playable catalog provenance, typed Dolby compatibility,
PQ/Dolby refinement, Dolby-compatible legacy routing, RPU-aware profile-5
refusal, negative-capability precedence, one-pixel geometry, neutral probe
timeout behavior and budget retention, descriptor-bound or immutable probe
execution, collision-safe child-fd assignment, process-group signalling,
end-to-end probe deadlines, final source fencing,
cancellation-safe hash admission, and source-offset lifecycle ownership now
have regressions. The no-default-feature `-D warnings` Clippy profile still
reports the same 116 pre-existing dead-code diagnostics on the exact M0 base;
M1's no-default `cargo check` passes, while default-feature Clippy is clean.

Fresh independent reviews cover the pure planner and the bound probe/cache
owner separately. Their latest requests exposed incomplete Profile 5
compatibility validation, neutral observation budget erosion, Linux snapshot
writer ownership, and crossed fixed descriptors. Code head `c03a98b3` closes
those gaps; both reviewers are being asked to verify the documentation-only
successor to exact validation receipt `8cc77fa6` before the single full suite
and Forgejo PR.

## M0 frozen source inventory

The machine-checked inventory is
[`decoder-selection-m0-inventory.toml`](../tests/playback/decoder-selection-m0-inventory.toml).
It stores a stable identifier, source file, exact anchor, classification, and
migration obligation for every row summarized below. The focused inventory
test fails when an anchor moves, a fourth movie `hls_args` call appears, or a
direct shipping Live TV FFmpeg command is added or moved without an inventory
update.

The shipping HLS surface has three movie `hls_args` callers and one direct Live
TV builder. Core calls below `mod tests` are argument regressions, not shipping
construction paths.

| Caller | Owner | Current purpose | Migration obligation |
|---|---|---|---|
| `PrepublicationTranscodeRetry::build` | `crates/plurxd/src/transcode.rs` | Freezes the one-step retry arguments and fingerprint | Freeze a validated reason-specific alternate plan and one shared budget |
| `ProducerRunner::produce_into` | `crates/plurxd/src/transcode.rs` | Builds each resumable pre-transcode/offline part | Carry plan identity and health evidence across every part and assembly |
| `Manager::start_with_audio_offset` | `crates/plurxd/src/transcode.rs` | Builds the live HLS producer command | Resolve once from the held source and use that plan for command, identity, and admission |
| `live_ffmpeg_command` | `crates/plurxd/src/live_tv.rs` | Builds direct tuner-input HLS without the movie builder | Freeze current arguments, then resolve decoder choice through the same bound plan and health model |

The media subprocess ownership surface is broader than the three builders.
The distinction between owner, consumer, and support process is deliberate:

| Inventory ID | Current state | Required owner/evidence |
|---|---|---|
| `process.observed_ffmpeg` | `transcode::spawn_ffmpeg` owns live, retry, and offline HLS children; progress and stderr readers are detached | Return one owned observed-child handle and join diagnostics before qualification |
| `process.fragmented_ffmpeg` | `transcode::spawn_ffmpeg_pipe` owns fragmented-copy children | Preserve its pipe split while giving the attempt a complete observer result |
| `process.live_tv_producer` | `live_tv::spawn_live_ffmpeg` owns the direct tuner HLS child | Use the same plan identity, observation result, and recovery budget as every other decoded producer |
| `process.live_tv_stderr` | `live_tv::capture_live_stderr` currently owns a bounded substring latch | Replace it with selected-stream, build-bound diagnostics and join it before health classification |
| `process.live_tv_graph_probe` | `live_tv::run_graph_probe` builds a synthetic startup/capability graph | Bind its result to the exact build, device, encoder, renderer, and surface; never qualify arbitrary media |
| `process.vod_generation` | `vodserve::spawn_generation` owns the FFmpeg copy producer and hands stdout onward | Classify copied output separately and stop discarding stderr |
| `process.vod_head_regeneration` | `vodserve::regenerate_init_head` owns a bounded FFmpeg head probe | Keep it probe/head-only; it cannot attest a complete producer |
| `process.vod_pipe_consumer` | `vodgen::run` consumes the pipe; it does not spawn production FFmpeg | Do not create a second child or health owner here |
| `process.pgs_demux` | `pgs_overlay::prepare_stage` copies one subtitle stream to SUP | Keep this support process out of video health evidence |
| `process.fragment_index`, `process.progressive_remux` | `fragindex::build_with_args` and `http::stream::remux` own copy/remux FFmpeg children | Never claim decoded-video qualification; classify audio-only transcode separately |
| `process.subtitle_extract`, `process.subtitle_window` | Whole-track and bounded-window subtitle FFmpeg extraction | Keep support output outside video qualification |
| `process.settings_ffmpeg_version`, `process.pipe_probe` | Settings version display and `pipeprobe::Spawn` capability runs | A successful version/probe query is not producer completion |
| `process.ffmpeg_*`, `process.dovi_*`, `process.hdr10_*`, `process.pacing_probe` | Exact build, graph, pixel, and pacing probes in `ffmpeg.rs` | Retain node/per-file capability evidence without promoting arbitrary media |
| `process.media_origin_probe`, `process.chapter_probe` | Bounded FFprobe timeline/metadata helpers | Keep facts distinct from decoded-video health |
| `process.dv_disk_capability`, `process.dv_disk_media_probe` | Version and descriptor-bound FFprobe children | Keep capability evidence distinct and retain the media source fence |
| `process.dv_disk_unbound_conversion`, `process.dv_disk_bound_conversion` | Unbound and descriptor-bound offline Dolby conversion children | Retain independent timeout, source, and verification receipts |

Renderer correctness is distributed across the candidate order and eleven
existing methods. Candidate validation must consume all twelve contracts
rather than duplicating their decisions:

| Inventory ID | Frozen constraint |
|---|---|
| `renderer.candidates` | `CANDIDATES` defines the deterministic graph order |
| `renderer.live_tv_filter` | `live_video_filter` independently owns deinterlace, scale, pixel-format, and upload composition for tuner HLS |
| `renderer.pairing` | `Pipeline::pairs_with` restricts vendor graphs and measured HDR10 encoders |
| `renderer.residency` | `Pipeline::on_gpu` distinguishes GPU-resident and system-memory frames |
| `renderer.dynamic_range` | `Pipeline::handles` restricts HDR, HLG, and Dolby Vision inputs |
| `renderer.decode_arguments` | `Pipeline::decode_args` claims QSV or VA-API input surfaces for vendor graphs |
| `renderer.device_initialization` | `Pipeline::init_args` supplies Vulkan/OpenCL device setup |
| `renderer.filters` | `Pipeline::filters` owns exact graph, pixel format, and transfer composition |
| `renderer.software_requirement` | `Pipeline::requires_software_decode` preserves Dolby Vision frame side data |
| `renderer.session_selection` | `Pipeline::for_session` applies source, encoder, and workload constraints |
| `renderer.output_grade` | `Pipeline::output_grade` binds SDR/HDR10 filter and encoder arguments |
| `renderer.fallback` | `Pipeline::fallback` refuses grade- or Dolby-incompatible fallback |
| `renderer.declined` | `Pipeline::declined` records bounded graph-refusal reasons |

Cache identity and delivery cross distinct write, lookup, and serve paths:

| Path class | Frozen owners | M7 obligation |
|---|---|---|
| Node-local write | `cache.local_publish`, `cache.manifest_publish`, `cache.manifest_capability_publish` | Publish decoder plan and completed health receipt with the generation |
| Shared root and write | `cache.shared_mount_io`, `cache.shared_root_identity`, `cache.shared_root_admission`, `cache.shared_publish` | Enter the verified root, copy, and reverify the same receipt; never mint qualification during replication |
| Local/shared lookup | `cache.location_read`, `cache.local_location_read`, `cache.speculative_hit`, `cache.offer_verification`, `cache.shared_read_prepare`, `cache.decoded_manifest` | Require compatible plan and complete receipt before calling bytes reusable |
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
normalizes source/output paths and the configured VA-API device path; option
order and every other token remain exact. The test injects the legacy
`PLURX_HWDECODE` compatibility choice, so it is hermetic while freezing both
default and forced-software behavior.

Live TV bypasses `hls_args`, so its complete software-producer argument vector
is frozen independently by
`live_tv_software_hls_argument_baseline_is_stable`. The adjacent probe-budget
and encoder-filter matrix tests freeze its selection-sensitive initialization,
deinterlace, scale, pixel format, and upload behavior. The M0 vector contains no
independent `-hwaccel` choice; M1–M2 must introduce any decoder choice through
the shared bound plan rather than a second Live TV policy.

| Cases | Decoder/renderer/encoder baseline frozen |
|---|---|
| `software-sdr-h264` | Software input, CPU renderer, x264 |
| `qsv-light-h264`, `vaapi-light-h264` | Software input, CPU renderer/upload, hardware encoder; no light-source hardware decode |
| `qsv-heavy-hevc-hdr`, `vaapi-heavy-hevc-hdr` | Heavy hardware input, CPU HDR renderer/download-upload, matching encoder |
| `qsv-vendor-renderer`, `vaapi-vendor-renderer` | Vendor input surface and renderer, matching encoder |
| `nvenc-libplacebo-renderer`, `vaapi-opencl-renderer` | Vulkan/OpenCL device initialization and neutral GPU graphs |
| `nvenc-light-h264`, `videotoolbox-sdr-h264` | Normal CUDA and VideoToolbox input with CPU renderer |
| `videotoolbox-avi-mpeg4` | Incident-sensitive legacy VideoToolbox input behavior |
| `qsv-heavy-hevc-hdr-forced-software` | Legacy operator software-decode override while retaining QSV encode |
| `dovi-tonemapx-videotoolbox` | Required software decode, Dolby reshape, SDR VideoToolbox output |
| `dovi-passthrough-qsv`, `hdr10-passthrough-qsv` | Grade-preserving Main10/PQ output and their distinct decode rules |

The reproducible
[`decoder-media-baseline`](../scripts/decoder-media-baseline) ran three actual
encoded sources through the baseline software HLS command on `nynuc` FFmpeg
8.0.1. Source hashes, probe facts, playlist/segment hashes, decoded-frame
digests, output facts, generator hash, and binary identity are retained in
[`decoder-media-baseline-2026-09-05.toml`](../tests/playback/decoder-media-baseline-2026-09-05.toml).
Generated controls cover unaffected behavior without pretending to replace the
unavailable incident media or hardware qualification. The generator's
`--verify` mode regenerates every source/output on the recorded FFmpeg build
and compares build identity, generator hash, source/playlist/segment/frame
hashes, and probe facts with the TOML.

The exact verifier was copied with only its evidence TOML to a task-scoped
directory on `nynuc`. Its first run rejected the evidence: repeated identical
probe rows exposed ambiguous extraction, Matroska/x265 output was not byte
reproducible, and encoder scheduling changed the MPEG-4-derived HLS segment.
The repaired generator collapses only identical repeated probe rows, rejects
conflicting rows, writes HEVC to MP4, and fixes x264/x265 to deterministic
single-threaded settings. Two independent fresh generations then matched
byte-for-byte. The final exact run returned `"verified": true` for every
retained build, source, output, frame, and probe fact.

| Control | Available evidence | M0 result / prerequisite |
|---|---|---|
| #913 MPEG-4 Part 2 ASP/XVID AVI, MP3 audio, 624×352 at 25 fps | Sanitized FFmpeg 7.1.4-Jellyfin diagnostic capture | Five explicit selected-video primaries in 361 ms; original media is not present, so VideoToolbox/software output comparison remains required |
| Generated H.264 SDR, MPEG-4 Simple Profile AVI, and HEVC Main10 HDR10 | Actual encoded source → HLS runs with retained probe and pixel digests | All three baseline software runs completed; MPEG-4 is not the #913 ASP/XVID file |
| Malformed rawvideo on host FFmpeg 8.0.1 | Exact `repeat+level+error` grammar plus version/binary/build hash | Only the addressed `rawvideo` contract is action-qualified; replayed #913 offsets are a separate timing control |
| Synthetic malformed rawvideo in deployed FFmpeg 5.1.9 container | Exact retained replay fixture | Top-level stream error lacks selected-stream attribution; observation-only, not automatic action |
| Hardware decode, Dolby Vision, subtitles, and multi-stream source controls | No M0 output evidence | Preserve source facts and add backend/pixel/metadata/startup controls in M8 |

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
| Maximum retained counter | `u64::MAX` | Every published count saturates instead of growing without bound |

The harness streams bounded binary records instead of loading a capture into
memory. An oversize, malformed, invalid-UTF-8, or non-monotonic record marks
the observation incomplete but is drained so later diagnostics remain
visible. A repeat summary belongs only to the immediately preceding classified
record; any repeat summary, including a severity-prefixed or unrelated one,
proves the stream is compressed and blocks automatic action. Detection latency
begins at the oldest record in the window that actually triggered the fifth
error, not at a stale earlier error.

The retained #913 capture supplies five explicit primary MPEG-4 video decoder
records in 361 ms, plus 49 messages hidden behind two legacy repeat summaries.
The explicit records meet the proposed threshold, but the capture has neither
severity markers nor uncompressed repeat timing and is deliberately not
eligible for automatic action. The FFmpeg 8 rawvideo companion replays the
observed offsets `0, 0, 294, 299, 361` in that build's verified rawvideo
grammar and latches at 361 ms. This composes a real grammar capture with a
deterministic timing control; it does not claim the rawvideo errors happened at
those times. The
tolerant control contains audio, unselected-video, encoder, filename, and
subordinate messages; its five selected primaries never place five timestamps
in the open-lower-bound window.

Tolerant observation matching recognizes the structured stream/decoder shape.
Automatic action additionally requires an explicit versioned contract from
[`diagnostic-contracts.toml`](../tests/playback/decoder-health/diagnostic-contracts.toml)
that binds FFmpeg version, binary/build hashes, codec, decoder, context
addresses, severity, exact error detail, and retained fixture hash. The sole
M0 harness parses those fields into a closed typed schema, cross-checks the
host version and hashes against the fleet evidence, then hashes and classifies
one open fixture descriptor. Its sole action contract is host
FFmpeg 8.0.1 `rawvideo`; it is not deployed-producer or MPEG-4 qualification.
`No frame decoded?` is supporting evidence. The #913 FFmpeg 7.1.4 MPEG-4
capture, deployed FFmpeg 5.1 shape, fatal backend initialization grammars, and
every additional build/backend grammar remain observation-only until retained
fixtures prove their exact attribution.

## FFmpeg and client qualification gaps

Read-only inventory captured 2026-09-05. The running container is the producer
that matters; a host binary is diagnostic context only.

Exact image IDs and hashes of the container binary, build configuration, and
complete decoder-list output are retained per node in
[`fleet-ffmpeg-2026-09-05.toml`](../tests/playback/decoder-health/fleet-ffmpeg-2026-09-05.toml).

| Node | Host FFmpeg | Running `plurxd` FFmpeg | Advertised container acceleration | Qualification |
|---|---|---|---|---|
| `nynuc` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `m6` | Not on host `PATH` | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `nuc4` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `nuc3` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| Local Apple build host | Full Homebrew FFmpeg 8.1.2 with `zscale`, run through a task-scoped x265 ABI 216 wrapper | No deployed daemon inspected | VideoToolbox | Qualified only as the local validation toolchain; not fleet evidence |

All four containers advertise software MPEG-4 plus QSV/CUVID families. None of
those names proves a usable device, correct surface transfer, metadata
preservation, or decoded pixels. Each node row retains the exact `sha256:` image
ID, full decoder-list hash, hardware-acceleration-list hash and decoded list;
no fleet backend class is qualified in M0.

Postpublication recovery is likewise unqualified. The ordinary movie clients
advertise only `hold`, `retry_resource`, and `terminal`; their existing parsers
intentionally reject undeclared `prepare_replacement`. Server preparation/commit
machinery does not make a client perform a two-player handoff. Live TV Web is a
separate lifecycle: it never enters `playback-control`, has no action
negotiation, and stops/releases the tuner session on failure. Live TV is limited
to prepublication recovery until that distinct lifecycle implements and
physically qualifies a two-player successor.

| Client | Declared actions | Parses `prepare_replacement` | Two players and readiness | Commit/retirement evidence | Current effective recovery |
|---|---|---|---|---|---|
| Web | `hold`, `retry_resource`, `terminal` | No | No | No | At most `prepublication` after server qualification |
| Apple | `hold`, `retry_resource`, `terminal` | No; unit test expects terminal protocol error | No | No physical run | At most `prepublication` after server qualification |
| Android | `hold`, `retry_resource`, `terminal` | No; unit test expects terminal protocol error | No | No physical run | At most `prepublication` after server qualification |
| Live TV Web | None; outside `playback-control` | No protocol participant | No; one owned media element | No physical run | Prepublication only after server qualification |

The visible Settings → Developer values are operator-requested upper bounds,
not a claim that every node or session can execute them. Each node computes an
effective plan policy from its current local capability/grammar receipt. Each
playback computes an effective recovery mode from that node result, owned
observation/resource/store readiness, and the actual client's advertised and
qualified behavior. The card must show requested value, per-node readiness,
session-effective value, and every bounded refusal. Unsupported sessions end
explicitly; they do not silently run an unqualified automatic replacement.

## Adversarial review ledger

Two independent agents reviewed the original pre-rebase M0 candidate before the full suite.
Their initial verdict was request changes; those repairs were committed before
both agents performed a second pass. Both second passes also requested changes;
their repairs were committed at pre-rebase `f9d68467`. Both third passes requested
the additional repairs committed at pre-rebase `195b7574`; both fourth passes
approved pre-rebase `6009367b` with no actionable findings. The fifth pass found
missing direct probe-normalization test coverage. That repair is committed at
pre-rebase `a16db3da`; both reviewers approved pre-rebase `7649ccdd` with no
remaining actionable findings. Both independent reviewers then approved
pre-rebase receipt head `2c266dce` after matching the retained full-suite JSON
and JUnit evidence to the historical ledger.

Those approvals are historical content evidence only. After rebasing onto
Forgejo `main` at `4a6a0268`, both fresh reviews of `3fbeedb2` requested changes:
the newly inherited direct Live TV producer/probe/filter/diagnostic path was
missing from the inventory; historical receipts had been relabeled with rebased
hashes; the baseline/current checkpoint was stale; and diagnostic counters did
not saturate. Those findings were repaired. Both independent reviewers approved
exact code head `59d0a4d1` with no actionable findings after separately checking
the formatting-only final delta, focused timing regressions, history audit, and
full-base diff. The exact head then passed the full PR suite recorded below.

| Finding | Resolution on working tree |
|---|---|
| Severity token was matched in the wrong position and addresses were omitted | Grammar and fixtures now preserve FFmpeg 8 context/address/severity order; FFmpeg 5 is explicitly unqualified |
| Fixture reader allocated the whole input | Bounded streaming reader drains oversize tails and records coverage loss |
| Latency began at the first historical error | Latency begins at the oldest record in the triggering window |
| Normalized baseline argv and media controls were absent | Sixteen exact argv snapshots plus explicit real-media/prerequisite matrix added |
| Client replacement support was overstated | Web/Apple/Android capability matrix records parse-only and physical-evidence gaps |
| Activation semantics depended on “the node/current client” | Persisted values are requested upper bounds; effective mode is node/session-local and visible |
| Legacy repeats were counted without provenance | Only an immediately preceding classified record owns a repeat summary; ambiguous summaries refuse action |
| Producer/cache inventory was inaccurate and not auditable | Machine-checked owner/consumer/renderer/local/shared/offline inventory added; `vodgen` corrected to consumer |

| Second-pass finding | Resolution on working tree |
|---|---|
| An unrelated repeat summary still allowed automatic action | Every repeat provenance now blocks action; severity-prefixed summaries and threshold controls are covered |
| Tolerant grammar was mistaken for build/codec qualification | Automatic action requires an explicit versioned contract; M0 qualifies only the captured FFmpeg 8 rawvideo family |
| Rawvideo timing replay changed the second #913 offset from 0 to 1 ms | Fixture now preserves `0, 0, 294, 299, 361` and labels grammar versus synthetic timing provenance |
| Argument baseline inherited `PLURX_HWDECODE` | Compatibility input is injected into the internal builder; default and forced-software cases are both frozen and pass under hostile process environment |
| VA-API and materially distinct renderers/output grades were absent | Baseline expanded to 16 cases covering every renderer family, normal VideoToolbox, heavy/light VA-API, and forced software decode |
| Process/renderer/manifest-cache inventory remained incomplete | Pre-rebase inventory expanded to 67 exact IDs with equality, count, and unique-anchor assertions plus the missing subprocess/method/cache owners |
| No actual media baseline existed | Reproducible H.264, MPEG-4 AVI, and HEVC HDR10 source-to-HLS evidence captured on a build-bound FFmpeg 8 host |
| Fleet hashes were global and lacked canonical commands | Binary/build/decoder/image hashes are now stored per node with exact capture and byte-canonicalization commands |

| Third-pass finding | Resolution on working tree |
|---|---|
| Contract metadata was declared but not enforced | A closed typed schema now validates version, hashes, stderr mode, and host against retained fleet evidence |
| Address/codec rejection tests were pre-rejected by fixture identity | Structural mismatches are tested directly with a fully validated contract |
| Fixture hash and parse used different opens | One descriptor now supplies both the streaming digest and classified records before action is decided |
| Light VA-API and legacy override spellings were not frozen | A sixteenth argv case proves light VA-API stays on software decode; a pure table covers `off`, `0`, `false`, and `no` |
| `dv_disk` subprocess inventory was incomplete and ambiguous | Capability, bound probe, unbound conversion, and Unix bound conversion have distinct rows; every retained anchor must occur exactly once |
| Media evidence was not rerunnable from the ledger | The exact `nynuc` verifier rejected three nondeterminism defects; after repair, two fresh generations matched byte-for-byte and the exact verifier returned `"verified": true` |
| Image IDs and hardware acceleration lacked exact per-node capture | Exact prefixed image IDs and per-node `-hwaccels` output/hash evidence are retained for all four nodes |

| Fifth-pass finding | Resolution on working tree |
|---|---|
| Probe normalization had no direct unit coverage and the ledger overstated the suite | A pure helper now has single, duplicate-identical, empty, and conflicting-row tests; the ledger separates those tests from the remote generation evidence |

| Post-rebase finding | Resolution on working tree |
|---|---|
| Current `main` added direct Live TV FFmpeg construction outside the frozen movie builders | Five producer-side Live TV rows plus its distinct Web client boundary bring the inventory to 73; static discovery and a complete software argv baseline prevent silent omission |
| Live TV Web bypasses the ordinary playback-control replacement protocol | A separate client capability row and gap matrix limit it to prepublication recovery until two-player handoff is implemented and physically qualified |
| Historical receipts were relabeled with rebased hashes | Original hashes are retained as pre-rebase evidence; only commands actually run on the current tree may be recorded as current qualification |
| Baseline and milestone state remained stale after rebase | The authoritative Forgejo base is explicit; exact head `59d0a4d1` is now qualified and linked to Forgejo PR #62 |
| Diagnostic counters were unbounded | Every published diagnostic counter saturates at `u64::MAX`; exact maximum and maximum-plus-one repeat summaries are covered |

| Final-review finding | Resolution on working tree |
|---|---|
| The historical mapping named `catalog-contract` without a test that read the mapped status page | The page is now catalog-governed and a validation contract cross-checks its frozen artifacts, safety boundaries, review repairs, and invalid-run labels |

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
| 2026-09-05 | Use generated encoded controls for M0 while preserving the original-media gap | This creates reproducible source-to-HLS evidence without pretending the controls are the #913 ASP/XVID asset |
| 2026-09-05 | Bind automatic diagnostic action to one exact build/codec grammar | Structural matching remains useful for observation, but cannot safely authorize recovery across unqualified FFmpeg builds |
| 2026-09-05 | Use MP4 and explicit single-thread encoder controls for generated media evidence | The exact remote verifier proved Matroska/x265 and unconstrained encoder scheduling were not byte reproducible |
| 2026-09-05 | Collapse only identical repeated probe rows | FFprobe may emit the same selected-stream fact more than once; conflicting rows remain an evidence failure |
| 2026-09-05 | Map review-only documentation commits to a status contract inside `catalog-contract` instead of ignoring them | The governed test reads the page and verifies its frozen artifacts, safety boundaries, review repairs, and invalid-run labels |
| 2026-09-05 | Use full Homebrew FFmpeg 8.1.2 plus its retained x265 ABI 216 library for local qualification | The default FFmpeg 8.1.2 lacks `zscale`; FFmpeg 9.0.1 has `zscale` but changes muxer output identities expected by the repository's FFmpeg 8 contracts |
| 2026-09-06 | Run qualification through task-scoped `ffmpeg-full` 8.1.2_2 wrappers | The installed full build provides `zscale`; the wrappers add only the retained x265 ABI 216 library path and leave the host installation unchanged |
| 2026-09-06 | Accept two declared full-suite skips on this Darwin builder | Android device validation requires unavailable `adb`; the two-node Live TV drill is Linux-only. Both remain explicit M8 fleet/client prerequisites rather than passing claims |
| 2026-09-06 | Reuse warmed cluster targets for the final exact-head suite | The first diagnostic run spent its 1,800-second bound compiling vendor targets. Warm targets change no source or test semantics and let the complete three-node workload run inside the same fixed bound |

## Validation ledger

No passing run is recorded until its command finishes on the named tree. Each
task PR gets adversarial agent review, findings are fixed, then one full suite
runs on the corrected head before merge. During development, the effort fast
lane and focused tests provide earlier feedback.

| Commit/tree | Command | Result |
|---|---|---|
| Pre-rebase `a9cb879b` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 6 tests |
| Pre-rebase `a9cb879b` | `make operations-check` | Pass · 192 tests; rerun outside restricted socket sandbox |
| Pre-rebase `a9cb879b` | `make validation-lint` | Pass · 23 points, 28 checks, 1,376 files |
| Pre-rebase `a9cb879b` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `a9cb879b` | `git diff --check` | Pass |
| Pre-rebase `2d4900e1` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 14 tests |
| Pre-rebase `2d4900e1` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test |
| Pre-rebase `2d4900e1` | `make operations-check` | Pass · 200 tests; run outside restricted socket sandbox |
| Pre-rebase `2d4900e1` | `make validation-lint` | Pass · 23 points, 28 checks, 1,381 files |
| Pre-rebase `2d4900e1` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `2d4900e1` | `git diff --check` | Pass |
| Pre-rebase `6c23d17a` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 15 tests, including exact 16 KiB and bounded repeat-count edges |
| Pre-rebase `6c23d17a` | `git diff --check` | Pass |
| Pre-rebase `f9d68467` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 21 tests |
| Pre-rebase `f9d68467` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test |
| Pre-rebase `f9d68467` | `PLURX_HWDECODE=off PLURX_VAAPI_DEVICE=/unexpected/device rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test; hostile environment cannot alter fixture output |
| Pre-rebase `f9d68467` | `make operations-check` | Pass · 207 tests; run outside restricted socket sandbox |
| Pre-rebase `f9d68467` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files after mapping the media baseline |
| Pre-rebase `f9d68467` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `f9d68467` | `sh -n scripts/decoder-media-baseline` | Pass |
| Pre-rebase `f9d68467` | `git diff --check` | Pass |
| Pre-rebase `195b7574` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 22 tests, including contract provenance and media-verifier mutation controls |
| Pre-rebase `195b7574` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 16 argument cases |
| Pre-rebase `195b7574` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_compatibility_override_values_are_stable -- --exact` | Pass · all four legacy false spellings and non-matches |
| Pre-rebase `195b7574` | `PLURX_HWDECODE=off PLURX_VAAPI_DEVICE=/unexpected/device rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · hostile environment cannot alter fixture output |
| Pre-rebase `195b7574` | `make operations-check` | Pass · 208 tests; run outside restricted socket sandbox |
| Pre-rebase `195b7574` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| Pre-rebase `195b7574` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `195b7574` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| Pre-rebase `195b7574` | `git diff --check` | Pass |
| Pre-rebase `a16db3da` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 23 tests, including direct single/duplicate/empty/conflicting probe-row coverage |
| Pre-rebase `a16db3da` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| Pre-rebase `a16db3da` | `/tmp/codex-decoder-m0-review5/decoder-media-baseline --verify /tmp/codex-decoder-m0-review5/decoder-media-baseline-2026-09-05.toml /tmp/plurx-decoder-m0-media-review8` on `nynuc` | Pass · `"verified": true` |
| Pre-rebase `7649ccdd` | Fifth diagnostic and scope adversarial re-reviews | Pass · both approve; no actionable findings |
| Pre-rebase `7649ccdd` | `make operations-check` | Pass · 209 tests; run outside restricted socket sandbox |
| Pre-rebase `7649ccdd` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| Pre-rebase `7649ccdd` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `4ec13f3b` | `make operations-check` | Pass · 208 tests; run outside restricted socket sandbox |
| Pre-rebase `4ec13f3b` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| Pre-rebase `4ec13f3b` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `4ec13f3b` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| Pre-rebase `4ec13f3b` | two fresh `scripts/decoder-media-baseline` generations on `nynuc` | Pass · every retained artifact digest matches byte-for-byte |
| Pre-rebase `4ec13f3b` | `/tmp/codex-decoder-m0-review4/decoder-media-baseline --verify /tmp/codex-decoder-m0-review4/decoder-media-baseline-2026-09-05.toml /tmp/plurx-decoder-m0-media-review7` on `nynuc` | Pass · `"verified": true` |
| Pre-rebase `4ec13f3b` | `git diff --check` | Pass |
| Pre-rebase `3fe6a6ca` | `CARGO='rustup run 1.97.1 cargo' make unit` | Invalid environment run · 143 FFmpeg-backed tests failed because Homebrew FFmpeg could not load retained `libx265.216.dylib`; no non-loader failure observed |
| Pre-rebase `3fe6a6ca` | `PLURX_FFMPEG=/private/tmp/codex-ffmpeg-abi216 PLURX_FFPROBE=/private/tmp/codex-ffprobe-abi216 CARGO='rustup run 1.97.1 cargo' make unit` | Pass · 2,785 tests; 3 declared ignores; task-scoped wrappers use the retained installed x265 ABI 216 library without modifying the host |
| Pre-rebase `ac456b44` | `make validate-full` with the initial task-scoped FFmpeg wrapper | Diagnostic pass · 18 passed, 3 failed, 3 skipped; history lacked five review-doc mappings, playback selected an FFmpeg without `zscale`, cold cluster work exceeded 1,800 s, Playwright was not on `PATH`, and `adb` was unavailable |
| Pre-rebase working tree before `01368ce1` | `make history-check` | Pass · 1,322 corrective commits had current evidence; review-only M0 documentation mapped to `catalog-contract` and was not ignored |
| Pre-rebase working tree before `01368ce1` | `make ui-check` through the existing `plurx-ui` Playwright 1.62.0 environment | Pass · 60 captures and 5,464 structural facts matched the golden; no console or page errors |
| Pre-rebase working tree before `01368ce1` | `scripts/reader-browser` through the existing `plurx-ui` environment | Pass · online/native/offline handoff, profile isolation, force-relaunch restore, style, TOC, search, finish, stale revision, and hostile-content checks |
| Pre-rebase working tree before `01368ce1` | `make cluster-check` with the warmed Rust 1.97.1 targets | Pass · 133 three-voter store contracts, topology/growth/failure drills, 7 activation tests, and 2 activity tests |
| Pre-rebase working tree before `01368ce1` | `make unit` with full FFmpeg 9.0.1 | Invalid tool-version run · 2 FFmpeg 8 muxer-identity tests failed; all other tests passed |
| Pre-rebase working tree before `01368ce1` | the two failed FFmpeg-sensitive tests with full FFmpeg 8.1.2 and retained x265 ABI 216 | Pass · copy-segment decode equivalence and mid-film generation identity |
| Pre-rebase working tree before `01368ce1` | `make unit` with full FFmpeg 8.1.2 and retained x265 ABI 216 | Pass · 2,785 tests; 3 declared ignores |
| Pre-rebase working tree before `01368ce1` | `make playback-smoke` with full FFmpeg 8.1.2 and Playwright 1.62.0 | Pass · 11/11 Chrome cases, including HDR tone-map, copy-HLS, no-MSE, seek, audio switch, and subtitle toggle |
| Pre-rebase `01368ce1` | `make validate-full` with full FFmpeg 8.1.2, retained x265 ABI 216, Playwright 1.62.0, and anonymous pinned Android container preflight | Pass · 23 runnable checks; Android-device was the sole declared skip because `adb` was unavailable; cluster-auth passed in 1,638.3 s |
| Pre-rebase `2c266dce` | Independent diagnostic-safety and milestone-scope adversarial reviews | Pass · both reviewers approved with no actionable findings after checking the retained full-suite JSON and JUnit evidence |
| `59d0a4d1` | `python3 -m unittest tests.operations.test_decoder_diagnostic_qualification tests.validation.test_decoder_recovery_status` and focused Rust Live TV/timing regressions | Pass · 29 Python tests plus current-base diagnostic saturation, inventory, exact Live TV arguments/grammar, and wedge-boundary timing controls |
| `59d0a4d1` | `CARGO='rustup run 1.97.1 cargo' make unit` | Pass · complete Rust unit profile; daemon 1,759 passed with 3 declared ignores, core 955 passed, store contracts 87 passed, and no failures |
| `59d0a4d1` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check`; `make history-check`; `make validation-lint`; `git diff --check effort/decoder-selection-recovery...HEAD` | Pass · pinned format/compile evidence, 1,344 corrective commits mapped, 24 catalog points / 30 checks / 1,425 files, and clean diff |
| `59d0a4d1` | Independent milestone-scope and diagnostic-safety adversarial reviews | Pass · both reviewers approved the exact current-base code head with no actionable findings |
| `59d0a4d1` | First current-base `make validate-full` diagnostic run | Diagnostic pass · 20 passed, 3 failed, 2 skipped; failures were the non-full FFmpeg missing `zscale`/x265 ABI and cold cluster compilation exceeding 1,800 seconds, not product assertions |
| `59d0a4d1` | `make playback-smoke`; `scripts/reader-browser`; warmed `make cluster-check` plus exact `cluster_activity` rerun, all through the corrected FFmpeg wrapper where media was involved | Pass · browser playback 11/11, reader online/offline lifecycle, 135 cluster store contracts with 2 helpers ignored, topology/growth/failure drills, 7 activation tests, and 2 activity tests |
| `59d0a4d1` | `PATH=/private/tmp/plurx-toolbin:/private/tmp/plurx-full-venv/bin:... PLURX_FFMPEG=/private/tmp/plurx-toolbin/ffmpeg PLURX_FFPROBE=/private/tmp/plurx-toolbin/ffprobe CARGO='rustup run 1.97.1 cargo' make validate-full` | Pass · 23 passed, 0 failed, 2 declared skips; generated `2026-09-06T06:46:16Z`, aggregate 2,794.94 check-seconds; Rust gate 130.9 s, UI 72 captures / 6,648 facts, playback 11/11, Apple tvOS 357/357, cluster 1,651.4 s; skips were missing `adb` and Linux-only two-node Live TV |

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
