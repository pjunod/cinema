# Decoder selection and recovery — implementation status

**Status:** M0 full PR qualification · **Updated:** 2026-09-05 ·
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
| Task PR | [#915](https://github.com/pjunod/plurx/pull/915) · draft; remote state being reconciled |
| Effort PR | Not opened yet |
| Working compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` via `rustup run 1.97.1` |
| Focused validation | Both final adversarial reviews approve `7649ccdd`; repaired fast lane and exact media verification pass |
| Full PR validation | Unit suite passes; one `validate-full` run is starting on the corrected head |
| Blocker | None |

## Milestones

| Milestone | State | Exit evidence |
|---|---|---|
| M0 · baseline and diagnostic qualification | Full PR qualification | [PR #915](https://github.com/pjunod/plurx/pull/915); unit suite and both final adversarial reviews pass |
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
| Local Apple build host | Homebrew FFmpeg cannot start: missing `libx265.216.dylib` | No deployed daemon inspected | Unknown | Unavailable |

All four containers advertise software MPEG-4 plus QSV/CUVID families. None of
those names proves a usable device, correct surface transfer, metadata
preservation, or decoded pixels. Each node row retains the exact `sha256:` image
ID, full decoder-list hash, hardware-acceleration-list hash and decoded list;
no fleet backend class is qualified in M0.

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
Their initial verdict was request changes; those repairs were committed before
both agents performed a second pass. Both second passes also requested changes;
their repairs were committed at `f9d68467`. Both third passes requested the
additional repairs committed at `195b7574`; both fourth passes approved
`6009367b` with no actionable findings. The fifth pass found missing direct
probe-normalization test coverage. That repair is committed at `a16db3da`;
both reviewers approved the repaired tree at `7649ccdd` with no remaining
actionable findings.
GitHub had deleted the temporary effort base and closed #914; the same effort
and task refs were restored, and #915 is the active review record.

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
| Process/renderer/manifest-cache inventory remained incomplete | Inventory expanded to 67 exact IDs with equality, count, and unique-anchor assertions plus the missing subprocess/method/cache owners |
| No actual media baseline existed | Reproducible H.264, MPEG-4 AVI, and HEVC HDR10 source-to-HLS evidence captured on a build-bound FFmpeg 8 host |
| Fleet hashes were global and lacked canonical commands | Binary/build/decoder/image hashes are now stored per node with exact capture and byte-canonicalization commands |

| Third-pass finding | Resolution on working tree |
|---|---|
| Contract metadata was declared but not enforced | A closed typed schema now validates version, hashes, stderr mode, and host against retained fleet evidence |
| Address/codec rejection tests were pre-rejected by fixture identity | Structural mismatches are tested directly with a fully validated contract |
| Fixture hash and parse used different opens | One descriptor now supplies both the streaming digest and classified records before action is decided |
| Light VA-API and legacy override spellings were not frozen | A sixteenth argv case proves light VA-API stays on software decode; a pure table covers `off`, `0`, `false`, and `no` |
| `dv_disk` subprocess inventory was incomplete and ambiguous | Capability, bound probe, unbound conversion, and Unix bound conversion have distinct rows; all 67 anchors must occur exactly once |
| Media evidence was not rerunnable from the ledger | The exact `nynuc` verifier rejected three nondeterminism defects; after repair, two fresh generations matched byte-for-byte and the exact verifier returned `"verified": true` |
| Image IDs and hardware acceleration lacked exact per-node capture | Exact prefixed image IDs and per-node `-hwaccels` output/hash evidence are retained for all four nodes |

| Fifth-pass finding | Resolution on working tree |
|---|---|
| Probe normalization had no direct unit coverage and the ledger overstated the suite | A pure helper now has single, duplicate-identical, empty, and conflicting-row tests; the ledger separates those tests from the remote generation evidence |

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
| `6c23d17a` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 15 tests, including exact 16 KiB and bounded repeat-count edges |
| `6c23d17a` | `git diff --check` | Pass |
| `f9d68467` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 21 tests |
| `f9d68467` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test |
| `f9d68467` | `PLURX_HWDECODE=off PLURX_VAAPI_DEVICE=/unexpected/device rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test; hostile environment cannot alter fixture output |
| `f9d68467` | `make operations-check` | Pass · 207 tests; run outside restricted socket sandbox |
| `f9d68467` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files after mapping the media baseline |
| `f9d68467` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| `f9d68467` | `sh -n scripts/decoder-media-baseline` | Pass |
| `f9d68467` | `git diff --check` | Pass |
| `195b7574` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 22 tests, including contract provenance and media-verifier mutation controls |
| `195b7574` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 16 argument cases |
| `195b7574` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_compatibility_override_values_are_stable -- --exact` | Pass · all four legacy false spellings and non-matches |
| `195b7574` | `PLURX_HWDECODE=off PLURX_VAAPI_DEVICE=/unexpected/device rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · hostile environment cannot alter fixture output |
| `195b7574` | `make operations-check` | Pass · 208 tests; run outside restricted socket sandbox |
| `195b7574` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| `195b7574` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| `195b7574` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| `195b7574` | `git diff --check` | Pass |
| `a16db3da` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 23 tests, including direct single/duplicate/empty/conflicting probe-row coverage |
| `a16db3da` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| `a16db3da` | `/tmp/codex-decoder-m0-review5/decoder-media-baseline --verify /tmp/codex-decoder-m0-review5/decoder-media-baseline-2026-09-05.toml /tmp/plurx-decoder-m0-media-review8` on `nynuc` | Pass · `"verified": true` |
| `7649ccdd` | Fifth diagnostic and scope adversarial re-reviews | Pass · both approve; no actionable findings |
| `7649ccdd` | `make operations-check` | Pass · 209 tests; run outside restricted socket sandbox |
| `7649ccdd` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| `7649ccdd` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| `4ec13f3b` | `make operations-check` | Pass · 208 tests; run outside restricted socket sandbox |
| `4ec13f3b` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| `4ec13f3b` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| `4ec13f3b` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| `4ec13f3b` | two fresh `scripts/decoder-media-baseline` generations on `nynuc` | Pass · every retained artifact digest matches byte-for-byte |
| `4ec13f3b` | `/tmp/codex-decoder-m0-review4/decoder-media-baseline --verify /tmp/codex-decoder-m0-review4/decoder-media-baseline-2026-09-05.toml /tmp/plurx-decoder-m0-media-review7` on `nynuc` | Pass · `"verified": true` |
| `4ec13f3b` | `git diff --check` | Pass |
| `3fe6a6ca` | `CARGO='rustup run 1.97.1 cargo' make unit` | Invalid environment run · 143 FFmpeg-backed tests failed because Homebrew FFmpeg could not load retained `libx265.216.dylib`; no non-loader failure observed |
| `3fe6a6ca` | `PLURX_FFMPEG=/private/tmp/codex-ffmpeg-abi216 PLURX_FFPROBE=/private/tmp/codex-ffprobe-abi216 CARGO='rustup run 1.97.1 cargo' make unit` | Pass · 2,785 tests; 3 declared ignores; task-scoped wrappers use the retained installed x265 ABI 216 library without modifying the host |
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
