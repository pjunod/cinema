# Architecture review assessment — every claim, its evidence, and its limits

**Status:** complete review of both documents · **Code reviewed:** `a14143684007`
· **Documents pulled at:** `0afefd92a` · **Written:** 2026-09-20

This assesses the entire [consolidated review](ARCHITECTURE-REVIEW-2026-09-20.md)
and its [nine-report appendix](ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md):
**4,212 lines**, **82 consolidated entries**, and **128 appendix findings**.
The entries overlap; 210 is a coverage count, not a count of independent defects.
Each appendix finding has its own disposition below. The prose, timelines,
argument examples, route inventory, 95 positive assessments and 53 open questions
are also assessed. The consolidated section numbers retain the source's numbering.

The newly pulled commit adds the review documents, status prose and a validation
receipt; the reviewed implementation is unchanged from `a14143684`. The appendix
supersedes the earlier assessment's availability limit. This revision also
corrects the earlier assessment's mistaken qualification of Hiqlite loopback
queries and incorporates the appendix's more precise dependency proposal.

The review is against the cited source tree, not an observation of the fleet.
No Rust or client code was changed, no deployment was performed, and no device
playback or Rust test suite was run. Repository queries, the real CI selector,
SDK headers and dependency source were used where they can answer the question.
Unrelated checkout files were left untouched.

## Verdict — useful defect inventory, unsafe as a direct implementation handoff

The review identifies several concrete defects worth acting on: missing child
output pipes, oversized-guide clipping inside the DVR scheduler, source-derived
transcode master metadata, repeated font probes, synchronous scanning, missing
client interruption handling and account-token export to external players.
It also correctly identifies the absence of a supported activated-cluster
backup/restore procedure and the compile-only main Rust gate.

However, several proposed changes remove the condition that makes the existing
system safe. Other findings elevate an unmeasured cost into a confirmed outage,
or describe an explicitly documented trade-off as an accidental default. The
document needs the corrections below before another agent builds from it.

**How to read the tables:** **Keep** means the source supports the core finding
and direction. **Amend** means a real concern is present but the description,
scope, severity or remedy needs correction. **Reject** means the stated claim
or remedy conflicts with the evidence. **Measure** means static code does not
establish the asserted operational outcome. A Keep verdict does not claim that
an unrun performance experiment has passed.

## Corrections that must reach the implementation plan

1. **Preserve acknowledged credential revocation (C7, S1).**
   [The cached admin proof](../../crates/plurxd/src/http/extract.rs) lives for
   five minutes and deliberately answers without reading the Store. A token
   deletion followed by best-effort peer notification can leave a revoked
   administrator authorized on a peer. Extending that cache to ordinary auth
   expands the impact. Keep the peer fence and its ambiguous-write handling;
   address contention using bounded admission without weakening revocation.

2. **B-frames require a presentation/decode timeline change (Q2).**
   [The encoded fragment validator](../../crates/plurxd/src/vodgen.rs), around
   line 396, rejects every nonzero composition offset. Reordered B-frames do
   not satisfy that invariant, with or without negative composition offsets.
   The current choice is explicitly documented in
   [VOD encoding](../streaming/VOD-ENCODING.md). Do not remove the guard merely
   to make new flags pass; prove random-access, init identity, leading pictures,
   timing and restart splices in production decode tests.

3. **Heartbeat age is not clock skew (S9).**
   [Membership](../../crates/plurx-core/src/cluster/membership.rs) heartbeats
   every ten seconds. Reading `last_seen_at` and fencing at a two-second
   difference would reject synchronized healthy nodes. A clock check needs a
   remote timestamp exchange or a trusted time-service measurement, explicit
   delay/uncertainty bounds, and separate handling of missing observations.

4. **An off-writer Raft snapshot needs one consistent cut (S2).**
   [The writer](../../vendor/hiqlite/src/store/state_machine/sqlite/writer.rs)
   persists in-memory last-applied/membership metadata immediately before the
   copy, while application is serialized. Moving the copy to another
   connection must keep the database image, last-applied ID and membership at
   the same logical point. Otherwise restore may replay already-applied writes
   or omit writes. Specify and test that boundary before changing execution.

5. **A stopped encoder must yield to viewers who arrive later (§2.6).**
   [Encoding::is_waiting](../../crates/plurxd/src/vodencode.rs) describes its
   own wait. It is not a global queue query. The shared pool has
   [Admissions::live_is_waiting](../../crates/plurxd/src/admission.rs), but a
   previously stopped producer subsequently yields `Step::Nothing`, not
   another `Step::Stop`, in [prodexec](../../crates/plurxd/src/prodexec.rs).
   Add bounded re-evaluation and an explicit stopped-to-release transition;
   cover a second viewer arriving after the first has stopped.

6. **Do not replace immutable-engine attestation with a blind TTL (§2.7).**
   [EncodedEngine::is_current](../../crates/plurxd/src/ffmpeg.rs) re-enumerates
   fonts to catch additions as well as removals and replacements. A font or
   rule change during a 60-second cache window can change the pixels emitted
   under an existing immutable recipe. Freeze a recipe's font environment or
   define an invalidation mechanism with conservative handling of missed events.
   Moving blocking metadata work off the runtime is independently reasonable.

7. **Retain Apple's status-driven recovery when reducing polling (A6).**
   [startStatusPolling](../../clients/apple/Sources/PlayerController.swift)
   calls `observeDeliveryStarvation` and samples prepared switches, not only
   the stats panel. A ten-second cadence when the panel is closed delays a
   recovery path built around two-second observations. Separate presentation
   telemetry from recovery evidence before changing either cadence. Likewise,
   replacing readiness polls with KVO must retain deadlines and cancellation.

8. **Use actual platform APIs and measured memory budgets (§2.8–2.9).**
   The proposed `AVDisplayCriteria(refreshRate:videoDynamicRange:)` initializer
   is not in the installed tvOS SDK. For this AVPlayer-based app, load the
   asset's preferred criteria and apply them through the active window, guarded
   against stale attachments. Apple's documentation recommends that approach;
   its public manual initializer takes a format description.
   [Apple AVDisplayCriteria](https://developer.apple.com/documentation/avfoundation/avdisplaycriteria).
   Media3's documented frame-rate strategies are OFF and ONLY_IF_SEAMLESS;
   a Surface API constant is not automatically a supported Media3 strategy.
   [Media3 C reference](https://developer.android.com/reference/androidx/media3/common/C).
   Android's `largeHeap` is not a guaranteed increase. Budget all concurrently
   live players and decoder/image memory before imposing a 48 MiB floor.
   [Android application attributes](https://developer.android.google.cn/guide/topics/manifest/application-element?hl=en).

9. **Keep ownership and integrity checks when optimizing their cost (C6, L2).**
   A bare size/mtime/inode check does not prove an artwork file matches its
   content-addressed name. Cache a verified digest against a suitably strong
   file identity and revalidate changes. Likewise, Live TV's settings read
   checks enabled state, owner, generation and drain admission. A process-local
   watch updated only by writes on that node cannot replace replicated
   observation, and an unbounded “unknown, continue” policy loses that fence.

10. **Treat changing the CI policy as a policy change (§2.2).**
    [The pipeline's current opening ruling](../DEVELOPMENT_PIPELINE.md)
    explicitly disables runtime schedules and gives main PRs a compile gate.
    The coverage gap is real; the claimed accidental divergence from the
    accepted policy is not. `cargo check` does not pay test code generation and
    linking. The two proposed `cargo test` commands are also not `make unit`,
    which includes workspace targets and two explicit FFmpeg restart checks.
    Scheduling the existing full workflow requires job conditions to keep heavy
    lanes out, plus a changed concurrency expression to cancel schedule events.

11. **Preserve search fallback after metadata changes (F-sc-8).**
    The proposed `NOT EXISTS` against `media_classifications` is not equivalent
    to excluding current `classification_fts` rows. The
    [classification schema](../../crates/plurx-core/src/store/classification.rs)
    deliberately deletes the FTS entry when a title or other source fact changes,
    retaining the classification record for regeneration. The existing query
    then finds the updated title through `items_fts`; the proposed replacement
    excludes it from both branches. A SQLite reproduction using the actual schema
    returned item 1 with the existing predicate and no items with the proposal.
    Optimize lookup against the current index membership, preserving this state.

12. **Never evaluate wall-clock SQL separately on replicated voters (F-sc-10).**
    Its alternative to skew measurement suggests a SQL `unixepoch('subsec')`
    default in the state machine. Hiqlite replays the SQL on every voter;
    that produces different values and replay results. The
    [replicated SQL guard](../../crates/plurx-core/src/store/replicated.rs):1–20
    explicitly forbids this. If a leader assigns time, compute and bind it once
    before replication, and specify leader changes and clock discontinuities.

13. **Keep temporary-file accounting when reducing scratch scans
    (F-stream-11, F-ltv-12).**
    [Rolling scratch accounting](../../crates/plurxd/src/transcode.rs):6566
    includes every file, not just published segments. Live TV's
    [inventory](../../crates/plurxd/src/live_tv.rs):6362 also enforces 128 MiB,
    regular-file, temporary-file and deletion-lag limits. A temporary segment
    grows before the playlist changes. Accounting only for segment-index deltas,
    or skipping inventory until playlist mtime changes, removes those checks.
    A bounded blocking scan can reduce dispatch overhead; event notifications
    need reconciliation and continued accounting for unpublished bytes.

14. **Cache the complete probe identity, and retain publication fences
    (F-stream-9, F-stream-16).**
    [DecodeFactCache](../../crates/plurxd/src/decode_facts.rs):2939 keys facts
    by source identity, FFprobe build, catalogue digest and selected stream.
    Persisting by inode/size/timestamps alone drops three keys and ignores
    node-local identities and inode reuse across restarts. A session's
    [SourceFence](../../crates/plurxd/src/fragment_index_cluster.rs):159 proves
    continuity since opening; it is not automatically an attestation of the
    earlier scan. Define that persisted attestation before skipping the held
    probe, and do not rate-limit the final source check across immutable
    publication. Moving the check off the runtime must retain its ordering.

15. **Remove the alleged production cover-art frame-rate defect
    (F-stream-12).**
    The cited [video_frame_rate helper](../../crates/plurxd/src/http/hls.rs):10130
    is marked `#[cfg(test)]`. Its first-video-stream selection cannot cause the
    claimed production master-playlist bug. Progress-parser drift remains a
    useful separate finding. Production frame-rate fixes need a production call
    path, not this helper.

16. **Correct the Live TV HLS command and latency claim (F-ltv-13).**
    `-hls_start_time_offset` is absent from the
    [FFmpeg 8 HLS option table](https://ffmpeg.org/doxygen/8.0/hlsenc_8c_source.html).
    The local FFmpeg 9.0.1 muxer help also has no such option. Establish support
    in the exact deployed fork before proposing it as a one-line change.
    Upstream initializes program date-time from the muxer's wall clock;
    adding that tag does not recover the original broadcast timestamp or measure
    tuner/probe/encode delay. Define the timestamp origin and client behavior.

17. **Compile the shipped semantic-search configuration
    (F-build-ops-codehealth-7).**
    Turning the feature off in the fast compile lane while leaving it on in
    Docker creates another configuration that can reach deployment without
    compilation. Feature isolation is reasonable, but retain a required compile
    check for the shipping feature set. Benchmark tokenizer backend equivalence;
    a process-wide Rayon pool change also affects other Rayon users.

18. **Correct the proposed release-profile snippet
    (F-build-ops-codehealth-11).**
    Its semicolon-separated TOML assignments fail parsing. In addition,
    `strip = "debuginfo"` removes the line tables the same example enables.
    Define symbol retention or separate debug artifacts explicitly. Fat LTO,
    linker choice and overflow checks need measured builds and behavior tests;
    enabling overflow traps changes runtime behavior and is not a free size win.

## §2 — all ten first actions

| Item | Verdict | Source evidence and required disposition |
|---|---|---|
| 2.1 Output helper | Keep; amend test and deployment claim | [process_control.rs](../../crates/plurxd/src/process_control.rs):39 does not pipe either stream. [dovi_probe_output](../../crates/plurxd/src/ffmpeg.rs):2660 and [probe_media_origin](../../crates/plurxd/src/transcode.rs):9264 consume their missing stdout. The listed diagnostic callers also consume stderr. Make the helper honor output capture and test both streams, failure exit status and cancellation. `/bin/echo` alone is not a Windows-port regression test. The code establishes the failure, not the date or number of fleet occurrences. |
| 2.2 Rust gate | Amend | [main-fast-lane.yml](../../.github/workflows/main-fast-lane.yml):127 runs check and vendored Clippy, not workspace tests/Clippy. [ci.yml](../../.github/workflows/ci.yml) and [lint.yml](../../.github/workflows/lint.yml) are tag/manual. The lint comment is stale. The [precommit hook](../../scripts/pre-commit) still invokes workspace Clippy; focused tests remain contributor obligations. Restore enforced execution as an explicit policy decision, with measured runtime and a correctly scoped schedule. A latent coverage gap is not evidence of a current P0 outage. |
| 2.3 Cluster backups | Keep; expand contract | [OPERATIONS](../OPERATIONS.md) explicitly identifies the missing portable post-activation backup/restore path. Enabling a vendor feature alone does not define recovery: include credentials/encryption keys, schema compatibility, activation identity, old-cluster fencing, off-node retention, integrity verification and a real restore drill. Distinguish portable logical backup from a Raft snapshot. A nightly file beside the same three disks does not cover the stated site-loss risk. |
| 2.4 Body chunk sizes | Keep chunk change; amend acknowledgement change | The four `ReaderStream::new` sites are present in [stream.rs](../../crates/plurxd/src/http/stream.rs):2940/2957 and [hls.rs](../../crates/plurxd/src/http/hls.rs):13402/14012. Larger buffers plausibly reduce per-read work. Their actual optimum and end-to-end cost require a throughput/concurrency measurement. The HLS acknowledgement also proves downstream body acceptance before delivery accounting and completion; byte-budget batching must retain exact final-byte, dropped-consumer, deadline and ownership behavior. |
| 2.5 HTTP serving | Amend | [main.rs](../../crates/plurxd/src/main.rs):2418 uses `axum::serve`; the cached pinned axum/hyper-util source installs no HTTP/1 timer there. Header-timeout hardening is justified. “No limits” is too broad: [http/mod.rs](../../crates/plurxd/src/http/mod.rs) has body limits, a capacity gate and endpoint-specific budgets. Apply handler deadlines by semantics, including legitimately long JSON operations. For gzip, honor encoding negotiation, `Vary: Accept-Encoding` and representation validators. Script execution order does not prove sequential network fetching. |
| 2.6 Encoder churn | Amend | The encoded Stop-to-Terminate conversion and lack of ahead-fill hysteresis are real in [vodserve.rs](../../crates/plurxd/src/vodserve.rs):6316 and [prodsched.rs](../../crates/plurxd/src/prodsched.rs):372. The global-queue/later-arrival correction above is required. The claimed 1,400 launches is a model, not an observation. The quoted 7.629 s benchmark fetched two entries near the end of a two-hour source and included reap; it is not a fixed cost for every restart. Add production launch counts and preserve fairness under capacity contention. |
| 2.7 Font probes | Amend | [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs):1612/1806 and [vodserve.rs](../../crates/plurxd/src/vodserve.rs):7483 establish repeated probes and synchronous metadata checks. Cost is real; a 60 s TTL changes the immutable-recipe contract. Freeze inputs or design loss-safe invalidation, then measure process/stat savings. |
| 2.8 tvOS matching | Keep gap; correct API and certainty | [PlayerSurface.swift](../../clients/apple/Sources/PlayerSurface.swift):310 uses AVPlayerLayer; source search finds no display-manager integration. Use asset-derived criteria and reset the exact active window/attachment on teardown. Do not infer the current HDMI output, universal 3:2 cadence or in-device tone mapping from that absence; those require a display-mode observation on a named title/device. |
| 2.9 Android matching and buffer | Amend | [PlaybackLoadControl.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackLoadControl.kt):16 sets the byte target and disables time priority. At 80 Mbit/s, 16 MiB is about 1.68 seconds, but a small buffer is not itself an eight-second stall. Its [instrumented test](../../clients/android/app/src/androidTest/java/tv/plurx/app/player/PlaybackLoadControlTest.kt) explicitly proves a full byte target can start playback before the time threshold. Validate memory, throughput and actual starvation before raising allocations. Frame-rate matching needs supported platform integration and device acceptance. |
| 2.10 Apple interruptions | Keep missing integration; measure sequence | No interruption or route-change observer was found in the Apple sources. [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift):5120 monitors desired playback rather than interruption state. The proposed system-pause state is appropriate; reconcile user pause, background/PiP, item replacement and resume permission. The specific six/twelve/sixty-second phone-call story is hypothetical until tested; a reopen is not proof that two live server sessions leak. Keep the existing iOS/tvOS audio-session distinction. |

## §3.1 — Q1 through Q11, video quality

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| Q1 | Amend | [encoder.rs](../../crates/plurx-core/src/transcode/encoder.rs):106 defaults to Bitrate, and [transcode.rs](../../crates/plurxd/src/transcode.rs):12552 gates measured quality modes. This is an explicit qualified/fallback policy. CRF/QVBR should be compared on representative content; source average bitrate is not a safe universal peak cap, especially when converting HEVC/AV1 to H.264. Preserve HDR10's separately qualified policy and VBV constraints. |
| Q2 | Reject flag-only remedy | [vod.rs](../../crates/plurx-core/src/transcode/vod.rs):270 disables reorder deliberately; [vodgen.rs](../../crates/plurxd/src/vodgen.rs):399 rejects nonzero CTO. The claimed universal efficiency percentages are unmeasured. Change the timeline contract and prove splices before enabling reorder; `establish_or_verify` alone is not the complete production publication/decode proof. |
| Q3 | Amend | [transcode/mod.rs](../../crates/plurx-core/src/transcode/mod.rs):1040 has Hable, no explicit peak/dither and post-curve primaries conversion. However FFmpeg's CPU tonemap reads peak side data when no override is supplied; absence of `peak=` does not establish missing metadata. Test metadata retention, units relative to `npl=100`, missing/misleading metadata, gradients and gamut handling. Do not substitute the `bt2390` spelling across different filters. [FFmpeg 8 source](https://www.ffmpeg.org/doxygen/8.0/vf__tonemap_8c_source.html). |
| Q4 | Keep gap; amend sizing | No file-path deinterlacer or recorded field-order contract was found in [scan](../../crates/plurx-core/src/scan/mod.rs) or [decode_facts](../../crates/plurxd/src/decode_facts.rs). [Live TV](../../crates/plurxd/src/live_tv.rs):6135 already deinterlaces. Add probe/storage/planner support and choose frame versus field output deliberately; double-rate output also changes the grid, bitrate, decoder limits and frame-rate metadata. Hardware graph support needs qualification. |
| Q5 | Amend scope and recipe | Full video transcodes inherit stereo AAC/160k from [TranscodeOptions](../../crates/plurx-core/src/transcode/mod.rs):895. But copy-video audio conversion already preserves six channels at 320k with explicit 5.1 layout, and encoded VOD explicitly sets 48 kHz in [vod.rs](../../crates/plurx-core/src/transcode/vod.rs):266. “Every audio transcode” and “no -ar” are false globally. Add codec/channel negotiation to recipe identity, muxing and manifests; changing the AAC-specific VOD audio contract to EAC3 needs more than an option. |
| Q6 | Amend | [encoder.rs](../../crates/plurx-core/src/transcode/encoder.rs):436 does not explicitly set NVENC `main`; it leaves profile to the encoder. AQ/lookahead are not explicitly enabled and [pipeline.rs](../../crates/plurx-core/src/transcode/pipeline.rs) has no CUDA graph. Defaults and supported flags depend on the shipped build/GPU. Keep this conditional on actual fleet use and measured compatibility, latency, memory and throughput; do not force one NVENC recipe onto VideoToolbox or every HDR grade. |
| Q7 | Keep | [master_playlist_with_shape](../../crates/plurxd/src/http/hls.rs):12413 uses source bitrate/dimensions, and only emits CODECS inside the HDR branch. Caller inspection shows it receives the source file. Emit the resolved output geometry and exact codec/sample-entry identity, plus a justified peak aggregate bandwidth. Target bitrate is not automatically peak bandwidth; count audio and other variant components. The Apple-panel consequence still needs a reproduced observation. |
| Q8 | Amend | [hls_args_inner](../../crates/plurx-core/src/transcode/mod.rs):1651 forces keys at two-second intervals. Extra scene-cut keys do not by themselves prove that the forced grid drifts. MPEG-TS and OpenCL Hable are present, but neither is an unconditional defect. Validate actual segment starts, client/container support and color output. This is not a justified bundle of one-line fixes. |
| Q9 | Keep bitrate concern; amend caption diagnosis | [live_video_bitrate_kbps](../../crates/plurxd/src/live_tv.rs):6110 uses source rate while the field-mode filter can double output rate. Derive output cadence in the planner and preserve bandwidth caps. `-sn -dn` is not proof that in-video A53 SEI captions were removed; audit preservation through each decode/filter/encode path. Advertising CLOSED-CAPTIONS without surviving captions and correct service IDs creates a phantom track. |
| Q10 | Amend | Worker disabling and immediate compatibility rescue are visible in [player.js](../../crates/plurxd/src/web/player/player.js):560/756 and [prepared-replacement.js](../../crates/plurxd/src/web/player/prepared-replacement.js):275. Enabling workers is a plausible isolated improvement. Recovery must participate in the existing attempt fences, control evidence and shared retry budget; the fatal path already reports failed before rescue. Do not apply audio-codec swapping indiscriminately to video/manifest faults or add an independent recovery loop. H.264 ceiling claims need device capability evidence. |
| Q11 | Keep capability gaps; qualify additions | [Caps.swift](../../clients/apple/Sources/Caps.swift):142 omits FLAC/PCM despite naming FLAC/WAV containers. [CapsPolicy.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/CapsPolicy.kt):260 omits Vorbis; [Caps.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/Caps.kt):157 probes 30 fps and does not express Main10 profile detail there. Add claims only after exact container/transport/profile/channel paths are proven. A fixture confirming a new string alone does not establish decoder support. |

## §3.2 — S1 through S11, store and cluster

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| S1 | Amend | [Hiqlite user_for_token](../../crates/plurx-core/src/store/hiqlite.rs):3980 uses a consistent read; [config](../../crates/plurx-core/src/config.rs):170 disables bounded replica reads by default. “Everything” and “nothing local” ignore opt-in catalogue reads, local media, caches and recovery auth. The pinned Hiqlite client routes consistent queries through its WebSocket stream even on the leader; the appendix is correct about that loopback path. This does not make every HTTP request a consensus read or every poster fetch sequential. Enabling bounded catalogue reads is a consistency-policy change requiring lag/fence/fallback coverage. An ordinary-auth cache needs complete user/role state and revocation semantics, not the admin-only proof copied verbatim. A source-site census is not a request-rate measurement. |
| S2 | Amend | The [writer](../../vendor/hiqlite/src/store/state_machine/sqlite/writer.rs):503 serializes VACUUM with apply; [migration](../../crates/plurx-core/src/cluster/migration.rs) configures 10,000 logs. The size estimate and universal eight-second lease-expiry consequence are not measured. Preserve snapshot consistency as described above. Raising thresholds increases log retention/replay cost; choose from build-time, disk and recovery measurements, not a blanket 100k–500k value. A side table in the same SQLite database does not remove those bytes from VACUUM. |
| S3 | Keep mechanism; amend optimization | [watched.rs](../../crates/plurxd/src/watched.rs):192 ticks each eligible node every second; [due_watched](../../crates/plurx-core/src/store/hiqlite_durable.rs):702 proposes UPDATE even when nothing is due. Three continuously eligible voters imply 259,200 proposals/day, not necessarily that many independent fsyncs. Use a local precheck only as a hint and retain the atomic replicated claim. MONARR_URL is a Store setting, not merely an environment variable. Idle backoff or singleton ownership must retain bounded pickup/failover. The disabled takeover poll also reads settings. |
| S4 | Keep | Same finding as §2.3; supported by [OPERATIONS](../OPERATIONS.md). Do not count it as a second independent defect. |
| S5 | Keep concern; measure floor | [membership.rs](../../crates/plurx-core/src/cluster/membership.rs) uses a 512 MiB storage floor; snapshot-copy failure propagates as a storage error. A database-sized copy can exceed that reserve. Account for database image, WAL, retained snapshots and concurrent temporary files on the actual filesystem. `1.5 × DB` is a proposed policy, not an established safe bound; alerting must include errors and low-space refusal. |
| S6 | Keep targeted changes; measure pool size | [sqlite/users.rs](../../crates/plurx-core/src/store/sqlite/users.rs):273 runs lookup plus a conditional UPDATE under `with_conn`; many catalogue methods share that writer. [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs):1300 has two reader connections. Move genuinely read-only operations while preserving multi-query consistency. The token UPDATE runs even when its predicate changes no row. Split/rate-gate carefully around deletion races. Eight readers is not automatically faster and multiplies cache memory. |
| S7 | Keep candidates; amend universal complexity | [media.rs](../../crates/plurx-core/src/store/sqlite/media.rs):856/970/1021 and [watch.rs](../../crates/plurx-core/src/store/sqlite/watch.rs):488 show the expensive query shapes and FTS exclusion. Existing indexes cover library/kind, parent and added time, but not all proposed composites. Use EXPLAIN QUERY PLAN and representative data for each query; prove result order and pagination equivalence. The appendix’s specific replacement of classification_fts membership with media_classifications existence loses renamed titles; see correction 11. Do not promise every rewrite or NOT EXISTS shape is faster. No application-side ANALYZE/optimize invocation was found for the standalone backend; the vendor writer does run optimize. |
| S8 | Keep documentation correction; reject automatic TTL migration | The [workspace dependency](../../Cargo.toml) disables hiqlite cache and durable SQL carries the relevant state. Correct the old tier description. Losing authoritative lease, owner or epoch state through TTL/cache semantics is not equivalent to losing a thumbnail; introducing an ephemeral tier requires a separate failure/durability contract. |
| S9 | Reject proposed measurement | Ten-second [membership heartbeats](../../crates/plurx-core/src/cluster/membership.rs):72 cannot establish a two-second clock-offset violation. Clock-skew protection is a legitimate concern, but implement the bounded measurement described above and test clock steps, asymmetric delay and unknown state. |
| S10 | Amend with actual selector results | Executing [scope_for_paths](../../validation/ci_scope.py) with the current [catalogue](../../validation/points.toml) finds **3 of 16** `hiqlite_*.rs` and **7 of 24** SQLite modules without cluster_auth, not 7 and 20. Dependencies between points matter. Missing Hiqlite files: classification, DVR, library_channels. SQLite: classification, DVR, library, library_channels, outbox, trakt, watch. Repair actual selector gaps and test both SQL families. Main-fast-lane still does not execute the cluster suite even when selection is correct. |
| S11 | Amend | Four startup pragmas and poison-as-error are visible in [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs):1352/1583. They are not evidence that every proposed tuning is beneficial. A 64 MiB cache per connection plus a larger pool can be expensive. Reopening/validating a poisoned connection is safer than blindly continuing through potentially interrupted state. Measure prepared-statement reuse, quick-check startup cost, mapping behavior and row-layout effects; a side-table migration has compatibility costs. |

## §3.3 — C1 through C11, server core

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| C1 | Keep with §2.4 conditions | The four small-buffer sites exist; retain delivery acknowledgements and cancellation bounds while measuring larger buffers. |
| C2 | Amend per §2.5 | Missing header timer is supported; absence of all limits/timeouts is not. Existing route/body and publication deadlines remain meaningful. |
| C3 | Keep | [scan/mod.rs](../../crates/plurx-core/src/scan/mod.rs):358/494 uses synchronous metadata and WalkDir in an async path. Move blocking traversal/stat work to a bounded worker while preserving order, progress, error reporting and cancellation. Merely paginating results is insufficient if an unbounded producer queue is introduced. |
| C4 | Keep | [plex.rs](../../crates/plurxd/src/http/plex.rs):191/217 fetches up to 5,000 items then awaits per-item files/children. There is no container pagination input on that handler. Batch metadata and support Plex pagination; test stable totals, ordering and empty pages. Priority depends on actual façade use. |
| C5 | Keep missing deadlines; qualify cluster-wide outcome | [tmdb.rs](../../crates/plurx-core/src/metadata/tmdb.rs):140 and [anilist.rs](../../crates/plurx-core/src/metadata/anilist.rs):59 have no explicit HTTP deadlines. Bound connect, response/body and total per-item/retry work. A stuck request can occupy the active job, but indefinite blockage of all nodes also depends on lease renewal and cancellation. Verify that separately instead of deriving it from the client builder alone. |
| C6 | Amend | [images.rs](../../crates/plurxd/src/http/images.rs):255/838 hashes content-addressed files and shares an eight-permit gate; lack of validators is real. Cache verified identity/digest, retain corruption quarantine and cancellation accounting, and reserve bounded peer-fetch capacity. [metadata/mod.rs](../../crates/plurx-core/src/metadata/mod.rs):38 explicitly chose original backdrops after visible upscaling. Add smaller derivatives for grids rather than globally reverting the TV/detail quality fix. |
| C7 | Reject best-effort revocation remedy | [logout](../../crates/plurxd/src/http/auth.rs):100 brackets the claimed Store mutation with [peer revocation](../../crates/plurxd/src/http/internal_auth_revocation.rs):265. Contention and client-local logout on failure are real, but the fence protects cached authorization. Keep it and bound queueing; do not replace it with an ordinary DELETE. |
| C8 | Keep hardening direction; amend product contract | [auth.rs](../../crates/plurxd/src/http/auth.rs) has no account/IP login throttle; tokens lack expiry and [extract.rs](../../crates/plurxd/src/http/extract.rs) accepts query credentials. Use bounded throttle state, normalized keys, proxy trust rules and measured password-verification concurrency. Query auth exists for media consumers; narrowing it requires client migration. Token expiry/devices are product changes that must account for long-lived and offline clients, not unexplained 90-day defaults. |
| C9 | Measure | The one-second TTL and global map lock exist in [media_sessions.rs](../../crates/plurxd/src/media_sessions.rs):107/1485, alongside query sharding, route generations and release fences. A map lock is not proof of serial Store I/O. Measure hold/wait time and query counts; extending positive TTL changes stale-owner/termination behavior. The suggested `now+TTL/2` expression needs an unambiguous TTL definition. |
| C10 | Keep gaps; amend absolute claims | [http/mod.rs](../../crates/plurxd/src/http/mod.rs):629 provides a request span, but no general RED histogram/counter or propagated request-ID layer. [main.rs](../../crates/plurxd/src/main.rs):1542 lacks a JSON switch and custom panic hook. The appendix correctly identifies tracing-subscriber's default ANSI behavior without TTY detection; actual deployment environment overrides remain unknown. Keep metric labels bounded and logs credential-redacted, including panic hooks and third-party panic text. Framework panics still reach stderr; absence from the in-memory log buffer is the narrower concern. |
| C11 | Amend | [stream.rs](../../crates/plurxd/src/http/stream.rs):2828 intentionally ignores If-Range when no strong validator exists. That is a conservative full-response behavior, not an HTTP correctness defect. Adding strong validators is useful, but still authenticate and verify current representation identity before conditional reuse; an ETag match must not bypass authorization or source-change checks. |

## §3.4 — L1 through L9, Live TV and channels

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| L1 | Keep bug; amend exact impact | [dvr_expand](../../crates/plurxd/src/live_tv/dvr.rs):753 calls the locally clipped guide; [guide.rs](../../crates/plurxd/src/live_tv/guide.rs):222 removes furthest programmes to meet the response cap. The scheduler can miss late airings. How many days survive depends on data, not a fixed first-day rule. Give internal consumers an immutable full-guide snapshot, enforce output bounds at HTTP serialization, and test an oversized guide with a matching late airing and concurrent refresh. |
| L2 | Amend remedy | [ensure_session_fence](../../crates/plurxd/src/live_tv.rs):5554 reads config and validates owner, generation, enabled state and drain admission; serving authority is also independently watched. Share bounded validated observations if needed, but propagate remote changes and define freshness. A watch fed only by local settings writes misses changes on another node; unbounded continuation on read failure is not safe fencing. The six-second failure time is a scenario to measure. |
| L3 | Keep transport reuse; qualify peer cache | [http/live_tv.rs](../../crates/plurxd/src/http/live_tv.rs):1093/1121 constructs [PeerTransport](../../crates/plurxd/src/http/peer_transport.rs):50 per request and resolves membership. Reusing the client is well supported. An epoch-only peer cache must also expire reachability and address/security capability changes that do not necessarily alter topology; preserve per-request signing and response verification. |
| L4 | Keep opportunity; design separately | [live_tv.rs](../../crates/plurxd/src/live_tv.rs) owns one tuner input/process per viewer session and has a bounded session ceiling. Shared transport can reduce tuner usage. Define slow-consumer eviction, bounded per-viewer queues, cancellation/reference ownership, DVR retention and per-plan authorization. An existing DVR fan-out primitive does not establish all viewer-sharing contracts. |
| L5 | Keep CPU/copy concern; correct lock scope | [cached generation](../../crates/plurxd/src/http/library_channels.rs):1299 clones under its mutex; repeated guide calls then use [resolve_occurrence](../../crates/plurx-core/src/library_channels.rs):993, which validates entries each time. That validation is not inside the cache mutex. Arc sharing plus once-per-generation validation is sensible; retain digest/identity checks on insertion. The 720 KB example is workload-dependent. |
| L6 | Amend | [collect_live_prefix](../../crates/plurxd/src/live_tv.rs):5625 stops at 8 MiB **or** three seconds, so three seconds is a ceiling after the first chunk, not an unconditional fixed wait. The cached UI source format includes dimensions/scan/audio layout but not the complete codec/HDR/frame-rate facts used by the delivery planner. Extend and qualify a complete cache before skipping probes. Later stderr detection cannot undo an already published wrong delivery. |
| L7 | Same as Q9 | Preserve the distinction between output cadence sizing and caption transport/advertising. |
| L8 | Keep decomposition direction | [live_tv.rs](../../crates/plurxd/src/live_tv.rs) remains large despite existing guide/DVR modules. Playlist parsing is duplicated across [transcode](../../crates/plurxd/src/transcode.rs), [renditiondir](../../crates/plurxd/src/renditiondir.rs) and Live TV. First inventory each parser's accepted grammar and error policy; sharing a parser must not silently widen a trust boundary. Separate mechanical extraction from behavioral unification. |
| L9 | Amend | [cleanup](../../crates/plurxd/src/live_tv.rs):5071 returns before retirement on failure, and scratch observation polls filesystem state. Separate session capacity from bounded orphan-cleanup ownership; blindly retiring can discard the only retry/accounting owner. Playlist-mtime notification is an optimization with overflow/missed-event fallback. PROGRAM-DATE-TIME is a capability addition, not proof of the reported stall. Distinguish unknown capability after restart without accepting stale capabilities. |

## §3.5 — W1 through W9, web

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| W1 | Amend | [index.html](../../crates/plurxd/src/web/index.html) has 70 script tags; those scripts total 2,145,634 bytes, and top-level CSS another 446,737 bytes. The large raw payload is real. Synchronous execution does not imply 71 serial HTTP transactions: speculative fetch, browser parallelism and cached immutable assets matter. Measure a cold/warm waterfall and time to usable UI. Gzip is sensible; preload selectively from that evidence. |
| W2 | Keep | [web.rs](../../crates/plurxd/src/http/web.rs):228 serves unversioned hls.js with seven-day caching, while app code extends its loader behavior. Version it with the consuming assets, preserving the vendored sidecar/native/test dependencies described in [WEB-SHELL-LAYOUT](../clients/WEB-SHELL-LAYOUT.md). |
| W3 | Keep narrower scope | [transport.js](../../crates/plurxd/src/web/player/transport.js):733 seeks direct play **and** immutable VOD locally; the gap concerns rolling HLS/progressive remux. The heading's “every non-VOD” also includes direct play and is too broad. Add an in-buffer route only when offset, generation, server demand and publication-retention rules allow it. A published server range is not automatically a locally buffered range. Test both local seek and reopen fallback. |
| W4 | Same as Q10 | Worker support can be tested independently. Recovery must remain inside the shared attachment/control budget. |
| W5 | Keep payload/cache concern | [web.rs](../../crates/plurxd/src/http/web.rs):241 onward serves sidecars without validators, and [app.css](../../crates/plurxd/src/web/app.css) embeds fonts. Version or validate sidecars and measure font splitting. Native clients consume some sidecars as build resources; keep those paths and ordering contracts intact. Preload only the actual used fonts to avoid spending bandwidth earlier without benefit. |
| W6 | Keep hardening direction | [web.rs](../../crates/plurxd/src/http/web.rs):197 lacks the proposed response headers, while [core/app.js](../../crates/plurxd/src/web/core/app.js) persists the token and [api.js](../../crates/plurxd/src/web/core/api.js) builds query-auth media URLs. The restricted CSP subset can precede handler migration. A later script policy must account for inline bootstrap/handlers and hls.js workers; apply frame restrictions to the appropriate pages and preserve offline/native reader embedding. Escaping is a useful practice, not an exhaustive XSS proof. |
| W7 | Keep | Source search finds video-error handling but no global error/unhandled-rejection reporter. Install a boot sentinel early enough to catch later script failures. Redact tokens, URL queries and user/media data; avoid recursively reporting logger failures and bound rate/payload/storage. A logger loaded after the failing asset cannot diagnose its own missing initialization. |
| W8 | Keep incremental typing; amend “only node --check” | The global script architecture is accurate, and checkJs/JSDoc/no-undef can help. But [web layout documentation](../clients/WEB-SHELL-LAYOUT.md) names load/order/layout tests beyond syntax. A jsconfig file list alone does not enforce runtime order; retain those tests. Establish an actionable typing baseline before making all existing diagnostics merge-blocking. Ownership/generation races still need behavior tests. |
| W9 | Amend compound claim | [library-grids.js](../../crates/plurxd/src/web/layouts/library-grids.js):100 explicitly mounts the shell once and redraws regions, not the whole page per batch. Repeated visible-grid replacement and full-library filtering still cost work. [decode-margin.js](../../crates/plurxd/src/web/player/decode-margin.js):517 continues sampling when paused, but progress/control liveness semantics must be separated before suppressing it. MediaSession and TV-specific back codes are absent in the searched sources; Escape is supported. Some hover motion lacks reduced-motion protection while other layouts already have it. Treat these as separate small changes with separate evidence. |

## §3.6 — A1 through A7, Apple

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| A1 | Same as §2.8 | Missing display-manager integration is supported; use the actual AVAsset/window API and physical output-mode evidence. |
| A2 | Same as §2.10 | Add interruption/route-change ownership and test user/system pause interactions; the exact failure narrative is not yet observed. |
| A3 | Keep extraction direction; amend inference | [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift) is 9,548 lines with many separately fenced domains. An immutable Attempt context can reduce repeated conjunctions, but independent epochs represent different invalidation scopes. Do not collapse them into one counter or force unrelated work to cancel together. Extract coherent lifetimes and test late callbacks; historical fix frequency alone does not establish cause. |
| A4 | Keep observer gap; amend blanket wording | The main controller observes status/end and polls timeControlStatus; [LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift):188 already observes timeControlStatus. A common item-event adapter can help, but preserve the separate finite/live/channel policies. Using the latest error-log entry without matching the failed resource can misclassify errors; source presence does not prove every last entry is benign. Add correlated failure tests, not merely more event subscriptions. |
| A5 | Keep performance concern; retain global sort/filter semantics | [AppModel.swift](../../clients/apple/Sources/AppModel.swift):613 pages each library and sorts the merged collection, while [LibraryView.swift](../../clients/apple/Sources/LibraryView.swift):14 repeatedly filters it. Sorting is needed across multiple libraries even if each page is server-sorted. Lazy loading requires server/global search and watch-filter behavior so unloaded titles do not disappear. The exact per-keystroke operation count is an estimate; profile a representative library. |
| A6 | Amend; recovery regression risk | Polls exist in [PlaybackControlSession.swift](../../clients/apple/Sources/PlaybackControlSession.swift):568 and [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift):5011/6637/7990. Status polling drives starvation recovery and prepared-switch sampling. The item-ready poll was introduced because KVO-only waiting could hang. Preserve those deadlines and evidence paths before reducing polling; 480 wakeups is not a measured energy cost. |
| A7 | Keep several gaps; amend certainty | [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift):6823/8832 repeatedly updates Now Playing and makes it iOS-only; no route-picker/scrub-thumbnail implementation was found. [project.yml](../../clients/apple/project.yml) uses Swift 5.9, and [Session.swift](../../clients/apple/Sources/Session.swift):8 has unchecked Sendable with mutable origin/token. Strict concurrency diagnostics and state-change updates are useful. A setter is not proof of one XPC per call. Protect session state based on actual access ownership, and validate lock-screen/remote behavior on both platforms. |

## §3.7 — D1 through D7, Android

| ID | Verdict | Source evidence and required disposition |
|---|---|---|
| D1 | Same as §2.9 | Byte/time math is supported; universal starvation/judder and safety of the proposed larger heap remain unproved. |
| D2 | Keep lifecycle gap; amend wake proposal | [PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt):1089 marks presentation foreground state; [Controller.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt):2640 does not pause there. The manifest has no media-playback foreground service, and the player view sets keepScreenOn. Define video/background/PiP/audio policy explicitly. A CPU/network wake lock does not replace keeping the display awake while watching video. Background audio needs the service, session, notification and platform permission/lifecycle contract together. |
| D3 | Keep faults; refine classification | [LiveTvPlayer.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt):481 maps behind-live-window to fatal. A bounded live-edge reprepare is appropriate for real Live TV, not finite VOD. [PlaybackPolicy.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackPolicy.kt):101 excludes audio sink errors, and [Controller.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt):704/2280 does not implement the claimed HTTP-status filter. Filter actual response causes; do not blanket-classify every 5xxx resource/sink failure as codec incompatibility. Re-snapshot route caps and bound retries. |
| D4 | Keep divergence; qualify common builder | The Live TV, [LibraryChannelPlayer](../../clients/android/app/src/main/java/tv/plurx/app/librarychannels/LibraryChannelPlayer.kt):65 and [offline player](../../clients/android/app/src/main/java/tv/plurx/app/data/offline/OfflineDownloads.kt):391 skip the finite controller's focus/noisy/fallback configuration. Centralize safe defaults with explicit role overrides. Tunneling is not universally safe for every subtitle/overlay/device mode, and offline playback must remain cache-only with no account-bearing upstream. |
| D5 | Keep security finding; expand backup edit | [DetailScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/DetailScreen.kt):486 exports [Session.mediaUrl](../../clients/android/app/src/main/java/tv/plurx/app/data/Session.kt):118 containing the account token. [SettingsStore](../../clients/android/app/src/main/java/tv/plurx/app/data/SettingsStore.kt) stores it in preferences. Both [legacy backup rules](../../clients/android/app/src/main/res/xml/backup_rules.xml) and [modern extraction rules](../../clients/android/app/src/main/res/xml/data_extraction_rules.xml) exclude only offline files. Exclude the DataStore in cloud and device transfer rules, not one unspecified line. Use a bounded, revocable file grant that supports the external player's actual range/HLS needs. App-private storage is not equivalent to public plaintext storage. |
| D6 | Amend compound finding | [Net.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/Net.kt):23 shares a client; async image/API calls can contend. The claimed 24-poster queue requires a trace and distinction between async dispatcher limits and synchronous media requests. [AppViewModel](../../clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt):674 pages eagerly; UI sorting should preserve merged ordering. Tests target policy components; no direct Controller instantiation was found in test sources. [Makefile](../../Makefile):1739/1751 publishes a debug APK, while release R8 configuration alone does not protect that artifact. Separate API scheduling, collection work, controller integration tests and release signing into independent tasks. |
| D7 | Keep startup blocking and telemetry opportunity; correct feature name | [OfflineDownloads.initialize](../../clients/android/app/src/main/java/tv/plurx/app/data/offline/OfflineDownloads.kt):113 blocks the main looper with two runBlocking calls; it initializes offline **media**, not just books. Lazy initialization must preserve transfer recovery and required manager readiness. [Controller.startStatusPolling](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt):2225 updates diagnostics every two seconds and is a candidate for visibility/backoff. Unlike Apple, this inspected loop does not call the starvation detector, but audit other readers before suppressing its updates. |

## §4 — all seven structural recommendations

| Item | Verdict | Source evidence and required disposition |
|---|---|---|
| 4.1 Large modules and drift | Keep decomposition; amend claims | The large-file line counts reproduce. [vodserve.rs](../../crates/plurxd/src/vodserve.rs):6778 directly spawns without the common runtime setup or Windows Job helper, so the drift is concrete. Progress parsers differ in [stream.rs](../../crates/plurxd/src/http/stream.rs):3409 and [transcode.rs](../../crates/plurxd/src/transcode.rs):2392, though swallowing a bare `filter_units=…` does not prove swallowing every prefixed FFmpeg diagnostic. The two registries intentionally represent VOD versus retained rolling playback; unifying them is not a mechanical move. Extract first, preserve private visibility and lifetimes, then separately evaluate unification. It is not a proven prerequisite for fixing every other lifecycle bug. |
| 4.2 Test seams | Amend substantially | Test-only barriers and fields exist, but non-module `cfg(test)` counts also include imports, helper functions and entirely test-only types. For example [playback_control.rs](../../crates/plurxd/src/playback_control.rs):2511/2741/2784 includes these categories. The document's 681 count cannot be described as 681 shipping-struct mutations. Injected no-op hooks can improve design but do not make scheduling identical to an activated blocking failpoint. Retain deterministic race tests, add shipped-binary tests where needed, and classify seams before migrating them. |
| 4.3 Vendor ownership and dependency size | Keep ownership/dependency concern; correct fix | [Cargo.toml](../../Cargo.toml) patches maintained vendor forks; [PLURX-PATCH](../../vendor/hiqlite/PLURX-PATCH.md) documents changes. Whether upstream adoption is viable needs a compatibility/support decision; a rename is not required to own maintenance. [hiqlite Cargo.toml](../../vendor/hiqlite/Cargo.toml):190 explicitly enables cryptr `s3`. `default-features=false` alone cannot disable an explicitly requested feature; cryptr 0.10 already has empty defaults. The appendix explicitly removes the unconditional s3 edge, which is the relevant fix. Preserve optional backup/dashboard combinations before deleting the S3 patch. A blanket aws-lc-sys ban can reject the intended TLS implementation; target duplicates or unwanted feature paths. [plurxd dependencies](../../crates/plurxd/Cargo.toml):26 unconditionally compile Candle/tokenizers; feature-gating is reasonable, but regex backend/model compatibility and a separate-process design need tests and an API contract. |
| 4.4 Process cost | Amend; retain safeguards | [history.py](../../validation/history.py):27 uses the broad corrective regex described; SHA receipts create remapping work. A PR-level receipt may be simpler, but define durable exact-tree evidence and merge enforcement, especially without branch protection. Restricting detection to subject prefixes allows unchanged failure modes under other subjects. A Markdown link checker does not replace index completeness or [status/history consistency](../../tests/operations/test_status_pr_claims.py). Prune only tests with a demonstrated stronger replacement, not a Python-line target. Production counters supplement regression tests. |
| 4.5 Releases and rollback | Reject missing-artifact premise; keep version hygiene | v0.3.0 is the latest local semantic tag and the workspace remains 0.3.0. But [OPERATIONS](../OPERATIONS.md):308 explicitly documents immutable `sha-<12hex>` image rollback and ten retained tags. Missing semantic tags do not imply no rollback artifact. Schema compatibility limits code rollback independently of naming. A per-deploy semantic tag would also trigger the full tag CI currently being criticized; decide that release policy deliberately and distinguish deployment receipts from releases. |
| 4.6 Operational defaults | Keep audit; amend prescriptions | [plurxd.service](../../deploy/plurxd.service) lacks the listed explicit limits, and [Compose](../../deploy/docker-compose.yml) lacks explicit pids/NOFILE limits. Host defaults and actual open-file demand need observation. Negative OOMScoreAdjust is inherited by children until changed; verify every child path and write permissions rather than relying on one pre_exec. Validate PrivateTmp against existing paths and child priority against realtime delivery. The FFmpeg 6/8 validation mismatch is documented in [the action](../../.github/actions/ffmpeg/action.yml). [Dockerfile](../../Dockerfile) uses a floating base but copies the pinned toolchain file before invoking rustup-managed cargo, so this does not by itself prove an unpinned compiler. Release debug information improves native stacks; panic locations are not categorically absent without it. |
| 4.7 Architecture documentation | Keep stale-reference repair; reject index count | [ARCHITECTURE](../ARCHITECTURE.md) still contains outdated single-voter default, ephemeral KV, four-second segments, no-role and no-DVR narratives. Correct them against current constants and supported modes. At the cited commit, every maintained Markdown document satisfies the index test's inventory; 50 unlisted records are in explicitly exempted folders. The earlier checkout had two unrelated unindexed staged documents; those are untracked after the pull and outside the tracked-doc inventory. The claimed 51 missing documents ignores that policy. A PR being merged does not imply its enclosing effort is closed or physically accepted; verify each alleged status mismatch before changing it. |

## §1 — executive claims and numerical provenance

| Claim | Assessment |
|---|---|
| Product lines tripled; 70% of tree is tests; 315k production lines | The appendix supplies a baseline and heuristic, but its history report says 161,733 product lines while its build report says approximately 315k for the same tree. A cutoff at cfg(test) is not a syntax-aware partition: production definitions can follow test-only helpers. Publish one reproducible classifier and reconcile both totals. |
| Pure functions were stable because they had few fixes | A useful design hypothesis, not causal evidence. Older or less-used code can have fewer commits. Separate bug density, change volume and runtime exposure. Preserve pure policy layers while checking the effectful adapters. |
| None of the hot-path choices was a decision | False. VOD no-reorder and release-on-hold contracts, qualified rate-control fallback, bounded Android allocations, original-resolution backdrops, and the web seek policy all have explicit reasons in the cited code/docs. Those decisions can be revisited, but the review must carry their constraints forward. |
| Every authenticated request is a leader round trip | Too broad across standalone SQLite, cache-only recovery routes and capability-authorized media. Hiqlite consistent queries do use the leader WebSocket path, including loopback on the leader. Measure by route/backend/role; parallel poster requests cannot be summed into a serial page delay. |
| Every control beat causes an encoded restart | The code permits churn after an ahead hold, but cadence depends on production speed, demand, blocked GETs and retained output. Count launches on a specific long session; do not present the model as fleet evidence. |
| Every audio transcode is stereo AAC | False for copy-video audio conversion, which preserves six-channel audio, and for Live TV's independently selected audio plan. Keep the narrower full-video-transcode finding. |
| Fix-density table and 40% aggregate | Large files and substantial corrective churn are supported; exact counts differ with time boundaries, merge inclusion and the definition of corrective. The source contradicts itself on Apple/Android fix counts between §§1, 3 and 4 and uses different classifications across appendix reports. Publish one query and avoid presenting correlation as proof of module-boundary causation. |
| 681 seams inside shipping structs | Incorrect categorization. There are 861 literal cfg(test) attributes across tracked crate src files in this checkout; excluding test modules still leaves imports/functions/test-only types. A syntax-aware category census is needed before claiming a layout/await change at every site. |
| All incidents were discovered by humans; counters would have caught them | The cited incident commits support particular diagnoses, not the universal detection-history claim. A non-increasing counter is ambiguous without expected demand, an alert and an owner. The review should specify an executable observation/alert contract rather than assuming instrumentation detects itself. |

The reproducibility check used `a14143684` and an explicit inclusive history
window starting `2026-08-20T00:00:00Z`. It found 3,906 non-merge commits, rather
than 3,860. This is not evidence of fabricated history. The appendix names a midnight
boundary and two different corrective-subject rules, but supplies no executable
inventory and mixes incompatible totals. Its five weekly counts sum to 3,905;
its product/non-product partition sums to 3,849 against a 3,860 headline. There are 2,213 non-merge commits after
v0.3.0 (2,949 including merges), so that narrower stated count reproduces.
The 852 `.toml` regression files include 511 naming `rust-gate`; “853 files”
can include a non-TOML file. Counts need their denominator and file selection.

Under a stated `^fix(?:\(|:)` subject rule, the same window yields these
file-touch counts. They count commits touching a file, not unique root causes
or production-only changes:

| File | All non-merge touches | `fix`-prefix touches |
|---|---:|---:|
| transcode.rs | 295 | 156 |
| http/hls.rs | 203 | 108 |
| playback_control.rs | 188 | 82 |
| vodserve.rs | 109 | 66 |
| cluster/membership.rs | 106 | 74 |
| PlayerController.swift | 71 | 36 |
| Android Controller.kt | 75 | 29 |

The same selection has 1,037 commits touching the regression directory and
134 `fix`-prefix commits touching operations tests. These do not reproduce
the review's 1,240/136 without a different selection. The broad conclusion
that process files generate substantial churn is credible; exact percentages
and “mostly string assertions” need a classified diff inventory.

## §5 — sequence corrections

Every proposed week-one item has been considered:

| Step | Disposition |
|---|---|
| 1 — output helper | Proceed as an isolated cross-platform correctness repair. |
| 2 — tests and Clippy | Explicitly revise the accepted CI policy, measure codegen/link/test time, and implement correct schedule/job/concurrency conditions. |
| 3 — body buffers and acknowledgements | Separate a measured buffer-size change from the more sensitive completion-accounting redesign. |
| 4 — listener, gzip, asset hashes, headers | Split connection limits/deadlines from representation caching and CSP. Each has distinct compatibility tests. |
| 5 — font attestation | Fix blocking I/O independently; design immutable inputs or reliable invalidation before introducing cache time windows. |
| 6 — SIGSTOP/hysteresis | Require shared-pool fairness and late-arrival tests, not only pure scheduler cases. |
| 7 — outbox, metadata timeouts, replica reads | These are independent changes. Preserve replicated claims; retain bounded retrieval of updated configuration; treat replica reads as a consistency-policy decision. |
| 8 — Apple, Android memory, backups | Split by platform/behavior. Backup exclusions are concrete. Heap floors and output-mode behavior need measurements. |
| 9 — web workers, recovery, errors | Worker enabling and global reporting can be isolated; recovery must integrate with the established control budget. |
| 10 — service limits and release | Separate observed resource sizing, child priority, image deployment and semantic-version policy. Do not deploy just to satisfy a calendar item. |
| 11 — DVR guide and peer transport | Both are good early candidates with the full-guide and bounded-reuse contracts above. |
| 12 — architecture prose | Proceed with code-backed corrections and the index; do not close efforts solely because their PR merged. |

**Month:** keep backup/restore, output-aware manifests, measured database work,
client lifecycle repairs and incremental typing. Do not bundle CRF, B-frames,
tone mapping, deinterlacing and surround audio into one undifferentiated change.
They alter different contracts and have different oracles. Three VMAF scores
cannot prove audio, HDR signaling, restart continuity, client compatibility or
recovery. Benchmark the axes separately and integrate only verified pieces.

**Quarter:** mechanical module extraction, explicit state-machine boundaries and
vendor ownership are reasonable investments. Registry unification, replacing
test seams and introducing ephemeral state require their own evidence. Keep
focused regressions and qualification alongside measured fleet acceptance.
The phrase “not a green unit run and a receipt” must not become permission to
replace pre-deployment verification with production experimentation.

## §6 — every “already good” category

These positive assessments are useful preservation constraints, but broad
absence claims should not be promoted to security or race-freedom proofs.

| Category | Assessment and limits |
|---|---|
| unwrap discipline, pinned toolchain/actions, rustls | [Cargo.toml](../../Cargo.toml) warns on unwrap and the Clippy gate denies warnings; many test contexts explicitly allow it. Toolchain pinning and SHA action references are visible. This does not prove zero unwraps across all shipped vendor code (the vendor writer itself uses them), nor that a gate omitted from PR CI ran. Define first-party/production scope for the count. Preserve the intended TLS dependency policy. |
| Store discipline | [SqliteStore](../../crates/plurx-core/src/store/sqlite/mod.rs) sends connection closures to spawn_blocking, enables WAL/NORMAL/FK/busy timeout, and runs migrations. [Hiqlite](../../crates/plurx-core/src/store/hiqlite.rs) centralizes measured calls. Coalescing, batched renewals, narrow file columns and json_each are valid patterns. They do not establish safe migrations under every failure or that all callers obey every consistency class; retain the existing contract suites. |
| Locks, bounded maps, auth, file/range serving | Bounded route/control maps, dummy password verification, guarded file serving and overflow-aware range parsing are visible. However “no std guard across await anywhere,” “every map bounded,” “all 205 routes uniformly authorized” and universal browse batching need explicit inventories or automated checks. The same review identifies the Plex unpaged/N+1 exception. Do not label an entire security boundary proven from spot checks. |
| Process supervision | Owned jobs, kill-on-drop, supervised reaping, stderr rings and pacing exist in [process_control](../../crates/plurxd/src/process_control.rs), [prodexec](../../crates/plurxd/src/prodexec.rs) and transcode. The review itself finds VOD bypassing the Windows job helper, so “Windows job objects” must be qualified by call path. Preserve these mechanisms while unifying spawns. |
| Copy/remux and grid contracts | Tagging, explicit DV stripping, chapter removal, immutable frame-grid validation, typed output grade and 5.1 AAC layout are real and worth retaining. They directly constrain Q2/Q5/Q8. A valid argument string does not prove device acceptance or engine support; continue runtime and device qualification. |
| HLS authoring | HDR range/codec metadata, subtitle defaults and random-access distinctions exist in [hls.rs](../../crates/plurxd/src/http/hls.rs). “CODECS only when needed” is not an unconditional positive: Q7 correctly identifies missing SDR codec/output metadata. Preserve subtitle and open-GOP behavior while correcting variant declarations. |
| Live TV | Configured ownership, signed control, guide persistence, inventory publication, separate scratch and bounded child environment are implemented. “One-second uniform cadence” accurately states the target/encoded cadence; copy output cuts at source keyframes and is not guaranteed to be exactly one second. Retain both source-GOP and listed-segment progress tests. |
| Clients | Policy fixtures, image caching, capability snapshots, Keychain, Android SurfaceView/fallback and hashed web assets are implemented patterns. Android tunneling/focus/fallback are not used by all builders, and R8 in the release configuration does not protect the published debug APK. Content-hashed URLs use a query hash which the handler ignores; they are useful cache-busting URLs, not a server guarantee that an arbitrary old hash retrieves old bytes. Universal escaping/race-freedom claims remain unproven. |
| Process practices | RCA-quality commit bodies, vendored patch records, mobile counters and the documented byte-identity split are concrete records. “Tests added not deleted” and “every ignore has a reason” need a reproducible census and should not be universal conclusions about the whole month. Receipt presence still does not prove execution of its named check. |

## §7 — all six decisions

| Decision | Assessment |
|---|---|
| 1 — fast lane scope | A real policy decision. Compare mandatory focused tests, broader unit execution and scheduled sweeps with measured queue/runtime cost. Scheduling alone leaves an exposure window; the current ruling explicitly chose compile-only CI. |
| 2 — hiqlite upstream or own | A real maintenance decision, but the choices are not exclusive. Own the tested patch stack while upstreaming generic fixes. Preserve exact versions and compatibility evidence; a new crate name is optional. |
| 3 — encoder defaults | Reject the forced choice between one broad PR and one flag at a time behind UI. Use reviewable changes by invariant, feature-gating only where operational rollback warrants it. VMAF covers neither surround audio nor timestamp correctness. |
| 4 — receipt ledger | Reasonable to reconsider, with an enforceable replacement for commit/base identity, test evidence and final-tree freshness. Do not delete gates first and promise future PR prose will replace them. |
| 5 — semantic search and Windows | Build cost warrants measurement. Runtime-optional semantic inference has a clear compile feature boundary; Windows is a platform target, not an equivalent optional dependency. Dropping support is a product decision that must not be inferred from a slow CI job. |
| 6 — DVR non-goal reversal | Correct the stale non-goal. Retention, conflicts, ownership and recording behavior already have [implementation](../features/LIVE-TV-DVR-IMPLEMENTATION.md) and [decision](../features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md) documents; reconcile those rather than pretending the decisions are unwritten. |

## §8 — all ten fleet questions and what would answer them

None of these device/fleet experiments was executed in this review. They are
specific remaining evidence, not findings silently counted as confirmed.

| Question | Required observation |
|---|---|
| Apple TV output-mode matching | Record device/OS, matching settings, source and delivered format/cadence, and the TV/receiver's actual HDMI mode before/during/after playback and stream changes. A player badge is insufficient. |
| iPhone interruption/headphone removal | Test calls, Siri, route removal, user pause during interruption, background/PiP and teardown. Observe play intent, AVPlayer state and server-session ownership; require no unintended speaker playback. |
| Encoded VOD restart rate | Count per rendition/session over a known interval, with encoder, ahead target, source position, startup/reap cost and competing admissions. A global grep count includes unrelated starts and cannot establish 1,400 restarts for one film. |
| Snapshot time and DB size | Scrape histogram bucket rates per node/outcome and correlate apply latency, leases, write rate, DB/WAL sizes and free space. A histogram family alone is not a p99 sample. |
| Consistent read latency | Segment metrics by backend/role and endpoint, include call counts and end-to-end latency, and compare identical workloads. Store-call duration is not itself network RTT or page time. |
| guide.json size | A stored file exceeding 2 MiB does not prove the requested scheduler window is clipped. Compare the scheduler's actual filtered guide with full-source late matching airings and resulting DVR rows. |
| HDR metadata after hwdownload | Inspect MDCV/CLL, transfer, matrix, primaries and pixel format on the exact deployed graph/build, then compare output. Include absent and conflicting metadata. |
| Android memory classes | Record memoryClass, largeMemoryClass, actual process memory, decoder allocations, image cache, buffer occupancy and concurrent incumbent/successor behavior; class values alone cannot validate the proposed floor. |
| NVENC/VideoToolbox fleet presence | Inventory configured versus successfully selected encoders and actual sessions. Hardware presence alone does not prove that Q6 is affecting playback. |
| DV Profile 5 impact | Identify a known P5 input and exact attempted path; correlate renderer proof failure with the deployed build. Existence of P5 library titles does not imply every direct/copy playback path failed. |

The proposed new counters in §5.3 are largely illustrative names. Existing
metrics include `plurx_live_tv_starts_total{outcome=...}` and the snapshot/Store
histograms, not every spelling in the proposal. For each acceptance counter,
define the event, bounded labels, expected workload, failure denominator,
observation interval and alert owner. A nonzero success count does not prove
that the failing scenario was exercised.

## Appendix coverage — every one of the 128 findings

The following ledger uses the appendix's exact IDs. References such as Q2, S2
and §2.6 point to the detailed evidence above; they are not additional findings.
Keep, Amend, Reject and Measure have the meanings defined at the beginning.
Historical incidents are distinguished from defects still present in the tree.

### Change history — 13 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-hist-1 | Keep historical mechanism; amend prescription | Commit `120a3d29` records the refused renew/yield statements and the quoted 72-hour incident. This is a repaired incident, not proof that the current worker is broken. Instrument validation failures before I/O and both backend contracts. A numeric-placeholder census must account for SQLite's indexed binding semantics; dead-job alerts need workload and time-window definitions. |
| F-hist-2 | Keep historical mechanism; amend blanket logging | `cfba5ff6` records and fixes both requeue binding and forced-successor admission. The caller in [vodserve.rs](../../crates/plurxd/src/vodserve.rs):2540 still discards a requeue result, worth targeted observability. Do not turn every intentionally best-effort cleanup result into an unbounded error stream. Classify operation, cancellation, retry and failure severity; retain a regression through the actual no-holder path. |
| F-hist-3 | Keep residual defect | §2.1 verifies the unpiped helper and two remaining stdout consumers. The old `.output()` and repaired pipeprobe explain the regression. Dates and fleet-wide impact remain attributed to the incident record, rather than independently observed here. Test stdout, stderr, unsuccessful exit and cancellation with a portable child fixture. |
| F-hist-4 | Keep acceptance gap; amend status rule | The [continuity plan](../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md) documents the unreachable viewer path and measured fallback interruptions. Separate merged, deployed and physically accepted status. Production counters should verify the real viewer action and denominator; they cannot replace pre-merge integration tests or require production success before code can be merged. |
| F-hist-5 | Keep historical defects; correct universal wording | `57e03a6b` confirms NULL-role activation failure; `93beae93` confirms mismatched request-ID validators. The latter rejected most random IDs, not literally every start. Both were repaired. Keep real ingress-to-owner smoke coverage and canonical schema fixtures; a new role backfill is not automatically needed after the invariant fix. |
| F-hist-6 | Keep recurrence concern | `8df09290` and the current migration tests support preserving exhaustive predecessor admission. A generated migration catalogue is reasonable only if it carries special migration preconditions, repairs and import behavior. A downgrade-fixture generator needs independent upgrade fixtures to avoid generating both implementation and oracle from the same mistaken table. |
| F-hist-7 | Keep documented history; amend current-state claim | Historical red baselines are documented, but an older STATUS entry cannot establish today's six failing web tests. No full runtime suite was run for this review. Required status enforcement must use the actual forge/account capabilities and the accepted gate policy; no-merge-on-red and a changed feature matrix are policy changes, not already configured guarantees. |
| F-hist-8 | Amend numbers and inference | Weekly and product/non-product totals are internally inconsistent, and receipt subclasses sum to 630 rather than the stated 631. Commit volume is not elapsed engineering cost or proof of zero benefit. PR-level evidence must remain durable and bound to the merged tree; link checking and Conventional Commit prefixes alone do not replace existing guarantees. See §4.4. |
| F-hist-9 | Reject rollback premise; keep release hygiene | The 2,213 post-tag non-merge count reproduces, but [OPERATIONS](../OPERATIONS.md):308 provides rollback via immutable sha tags with ten retained images. Semantic releases, mobile build counters and deployment revisions answer different questions. Do not replace a working deployment receipt with weekly tags without accounting for tag-triggered qualification. |
| F-hist-10 | Keep specific documentation drift | DVR, standalone SQLite, segment constants, client shell structure and vendor ownership need reconciliation. The index explicitly exempts evidence/build/archive records, so their absence is not missing-document debt. Implementation merged is not equivalent to effort acceptance complete. Use authoritative constants and behavior tests rather than brittle prose-number greps alone. |
| F-hist-11 | Keep render evidence; amend universal gate | `4b109ebd` confirms the owner-selected restoration and render comparisons. This supports reviewable before/after renders for visual redesigns, not a finding that every layout change requires a new owner approval rule. Preserve accessibility and behavioral checks alongside renders. The cited calm-library test is regression evidence, not proof every prior UI change was wrong. |
| F-hist-12 | Keep maintenance ownership; amend size and options | Patch ledgers document real fork obligations. Gross patch insertions across history are not net divergence from upstream, and an OpenRaft fallback being unevaluated is not evidence of an available drop-in substitute. Ownership does not require renaming the crate or abandoning selective upstreaming. See §4.3. |
| F-hist-13 | Keep extraction; amend numerical certainty | Giant implementation modules and corrective churn are supported. The appendix's two product-line classifiers disagree by almost a factor of two; touches across overlapping files must be deduplicated before claiming 40% of all fixes. Mechanical extraction can proceed incrementally; it is not a prerequisite for independent correctness fixes. |

### Streaming pipeline — 16 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-stream-1 | Amend | Q1 applies. ABR is deliberate; saying it has no lookahead-informed allocation conflicts even with the report's own rc-lookahead example. Source average bitrate is not a safe cross-codec peak cap. Qualify quality mode and cache identity per output grade and hardware family before changing defaults. |
| F-stream-2 | Reject flag-only remedy | Q2 and correction 2 apply: nonzero CTO is refused by production validation. `negative_cts_offsets` does not satisfy that invariant, nor prove restart splices. Closed GOP alone is not the whole timeline contract. Claimed universal efficiency/support figures are not evidence from the deployed pipeline. |
| F-stream-3 | Amend | Q3 applies. Unlike the summary, the appendix acknowledges FFmpeg's side-data fallback and labels the effect LIKELY; retain that distinction. Explicit peak, dither and gamut changes need output comparisons across absent/conflicting metadata and hardware download paths. A default 1,000-nit peak is a policy assumption, not source truth. |
| F-stream-4 | Keep gap; amend qualification | Q4 applies. Field order can be unknown, mixed or incorrectly tagged; `field_order != progressive` is not sufficient evidence that every frame is interlaced. Validate conditional frame/field output and hardware fallback with interlaced and progressive fixtures, including resulting fps and bitrate. |
| F-stream-5 | Amend | Q5 applies; the appendix correctly distinguishes copy conversion but its no-sample-rate headline ignores VOD's 48 kHz. Add channel-layout and mux compatibility to negotiation. The illustrative numeric pan matrix is incomplete and depends on channel ordering; it must not be copied as a universal no-LFE downmix. Verify dialogue level and clipping rather than promising a fixed improvement. |
| F-stream-6 | Amend | §2.6 and correction 5 apply. Also reject reducing two seconds of audio preroll to two AAC frames without proof: [vod.rs](../../crates/plurx-core/src/transcode/vod.rs):125–165 explicitly covers demux, resampling, offsets and the film-global AAC lattice. Retain exact landing and cross-generation continuity tests. |
| F-stream-7 | Amend | §2.7 and correction 6 apply. Probes/stat calls exist; 100–500 ms and producer bottleneck are estimates. Timer-only attestation permits changed fonts or executable dependencies under an existing immutable recipe. Freeze inputs or preserve conservative invalidation, and measure runtime blocking separately from child-process cost. |
| F-stream-8 | Keep buffer opportunity; reject unsupported magnitude | §2.4 applies. The small-buffer sites are real. A 64× larger buffer does not prove a 100× throughput win or removal of over 90% of total cost. Keep acknowledgements, ownership and cancellation; measure concurrent memory and tail latency as well as throughput. |
| F-stream-9 | Amend; unsafe cache key | Correction 14 applies. The held probe runs on the inspected admission path, but the decode-facts probe can hit cache, so two child probes are not unconditional on every create. Persist all existing identity dimensions, attest the scan association and preserve reporter-drift rules before bypassing work. TTFF savings remain unmeasured. |
| F-stream-10 | Amend | Q6 applies. The code does not force NVENC Main, and a driver-version floor does not prove every GPU supports every B-reference/lookahead flag. CUDA/VideoToolbox zero-copy graphs need deployed-build probes, output parity and capacity tests; B-frames also require the independent VOD timeline change. |
| F-stream-11 | Keep cost; reject accounting shortcut | Correction 13 applies. Directory inventory covers unpublished bytes omitted by the segment index. FFmpeg progress does not itself prove atomic playlist publication. Retain bounded reconciliation for missed notifications, producer silence, rename races and temporary-file growth. The stated stat rate depends on actual retained files. |
| F-stream-12 | Amend; reject frame-rate example | Correction 15 applies: the cited HLS frame-rate parser is test-only. The progressive progress classifier is broader than the closed-key classifier, but a bare filter_units line is not every FFmpeg diagnostic. Share parsing only after preserving the distinct accepted inputs, error handling and attached-picture semantics of production callers. |
| F-stream-13 | Keep drift; amend scope | §4.1 verifies the direct VOD spawn and missing common environment/Windows Job helper. Consolidate these guarantees while preserving inherited descriptors, cancellation, reaping and diagnostics ownership. The Docker startup failure and stranded-child frequency require reproduction; do not assume one diagnostics grammar fits every pipe. |
| F-stream-14 | Keep metadata defect; correct literal recipe | Q7 applies. Do not hard-code avc1.640028 for every rolling rendition: output profile/level vary. Derive exact output codec and geometry; configured maxrate plus audio is not automatically a measured HLS peak, especially over short segments. Preserve the documented SDR compatibility history while fixing declarations. |
| F-stream-15 | Keep decomposition; separate redesign | §4.1 applies. The proposed module ownership map is useful; copying private state across modules or exposing everything pub(crate) can weaken boundaries. Keep behavior-preserving moves separate from registry unification and test extraction. File-touch counts do not establish that every fix had one structural cause. |
| F-stream-16 | Amend all six subitems | Forced extra IDRs do not prove grid drift; HEVC fallback packaging and tone-map algorithms require their own qualification. Direct validators must preserve auth and file identity. Source-fence checks cannot be blindly sampled once per second. Whole-segment buffers are real, but streaming them must retain complete-fragment validation, atomic publication, cancellation cleanup and bounded concurrent writes. |

### Server core — 13 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-core-1 | Keep buffers; reject removing the delivery pump | C1 and §2.4 apply. Replacing the pump with Body::from_stream loses its completion/cancellation accounting unless that contract is implemented elsewhere. Read size and queue capacity are separate tuning decisions; preserve bounded per-viewer memory. |
| F-core-2 | Amend | C2 applies. Header timeout is missing, while route-specific limits and capacity gates exist. A global limit does not make those subsystem limits unnecessary. Header-read deadlines do not alone bound idle keep-alive sockets; specify each connection/handler/body deadline and streaming exception separately. |
| F-core-3 | Keep | C3 applies to traversal and synchronous sidecar/metadata reads. A bounded blocking producer must retain cancellation and memory bounds; spawn_blocking work is not automatically cancellable once started. Network-filesystem delay is a measurement target rather than a fixed per-entry cost. |
| F-core-4 | Amend; correct earlier assessment | Consistent Hiqlite reads do take the WebSocket path, including leader loopback; [query.rs](../../vendor/hiqlite/src/client/query.rs):21/286 and [query execution](../../vendor/hiqlite/src/query/mod.rs):19 establish it. Parallel poster fetches are not 70 serial RTTs. The 60-second last-seen write window and 15-second exclusion lease are not accepted credential-revocation staleness. Preserve acknowledged revocation. |
| F-core-5 | Keep | C4 applies: sequential per-item metadata and missing Plex container paging are concrete. Reuse batched metadata without dropping file/version details, and specify totalSize, offset/limit clamping, stable ordering and child pagination. Measured Kodi refresh latency remains outstanding. |
| F-core-6 | Keep deadlines; amend outcome | C5 applies. Include response-body consumption, retry budget and lease cancellation, not merely connection timeout. The appendix also identifies post_join_request: bound that request within the join protocol while preserving reconciliation of an ambiguous accepted join. A timeout is not proof the operation did not happen. |
| F-core-7 | Amend | C6 applies. Local hits share the eight-permit gate and content verification; separating network work is sensible, unbounded local reads are not. Keep corruption detection and cancellation accounting. Add image derivatives without removing the deliberately chosen high-resolution originals. Request concurrency and blank-image rates need observation. |
| F-core-8 | Reject best-effort revocation; keep contention finding | C7 and correction 1 apply. The cache lasts five minutes, not a 15-second tolerated stale interval. A bounded queue may improve concurrent logout behavior, but successful revocation must still exclude stale peer proofs. Preserve ambiguous-write and node-membership handling. |
| F-core-9 | Keep throttle gap; amend policy | C8 applies. A map keyed only by username/IP pairs is not enough without bounded storage, trusted-proxy handling and protection against distributed guessing or intentional account lockout. Token expiry affects long-lived clients and recovery proofs; define the policy and revoke cached proofs consistently. The quoted guesses/second are unmeasured. |
| F-core-10 | Keep extraction; amend causality | A typed transition function can help, but rejection ordering, duplicate-sequence semantics and effects are contractual. A 250 ms rate floor is not proof concurrent requests cannot reorder. Different rejection text for corrupted replay is an API decision. Preserve behavior first, then redesign phases under existing lifecycle tests. |
| F-core-11 | Amend | C9 applies. Lease TTL does not bound ownership changes to once per 6–12 seconds: release, replacement and fencing can change authority immediately. Extending positive-cache lifetime requires proof against those events. Lock sharding and pruning are separate measured changes; the cache mutex is not held across the Store query. |
| F-core-12 | Keep instrumentation; amend completeness | C10 applies. Use matched routes and bounded method/status labels, distinguish response-header latency from streaming-body completion, and sanitize request IDs. The cached tracing-subscriber source confirms ANSI defaults on unless NO_COLOR disables it. A JSON option and panic reporting are useful; preserve redaction and avoid recursive logging. |
| F-core-13 | Keep validators; reject bypassing authority | C11 applies. Returning a full response when If-Range cannot be validated is conservative behavior. An id/size/mtime tuple is not automatically a strong byte validator under in-place replacement. A matching validator cannot replace current authorization or the held-file identity check. |

### Store and cluster — 14 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-sc-1 | Amend | S1 and F-core-4 apply. Keep distinctions between authority, bounded catalogue and node-local reads. The appendix itself proposes incompatible auth TTLs in different reports; resolve the revocation contract before caching. Replacing settings polling with notifications needs remote-apply coverage and bounded recovery after missed events. A justification-comment census is not a latency test. |
| F-sc-2 | Amend; preserve snapshot consistency | S2 and correction 4 apply. Snapshot work blocks the writer, but the time until every session fences depends on when its last renewal committed. Preserve a single database/last-applied/membership cut. Moving probe_json to another table in the same DB does not shrink VACUUM output; compression is a separate measured storage change. |
| F-sc-3 | Keep; preserve atomic claims | S3 applies. Local due-work checks are hints, not replacements for replicated claims. Singleton scheduling needs failover and bounded pickup; disabled Monarr configuration must still be refreshed. Proposal counts must not be presented as one physical fsync per proposal. |
| F-sc-4 | Keep recovery gap; expand restore contract | S4 and §2.3 apply. The proposed vendor environment variable is not an accepted product restore procedure by itself. Include schema/version checks, encrypted data/key handling, cluster identity reset/fencing, off-node retention and recovery of users/settings/watch state in a restore drill. |
| F-sc-5 | Keep capacity concern; reject invented retryability | S5 applies. Returning a StorageError from the builder is fatal in the inspected OpenRaft path; owning the builder does not create a retryable-error variant. Defer safely before snapshot admission or change the integration explicitly. Account for database/WAL/copy retention and test low-space refusal without falsely reporting a successful snapshot. |
| F-sc-6 | Keep targeted reader migration | S6 applies. user_for_token is not read-only; it performs a conditional UPDATE. Splitting it must preserve credential deletion races. A no-DML text grep is not proof a multi-query closure can change transaction/connection semantics. Measure the pool and total cache memory rather than selecting eight readers by default. |
| F-sc-7 | Keep query work; amend replacements | S7 applies. A newest-8×limit candidate window is correct only if widening continues until the original distinct-group result is complete, including ties and filters. Some proposed indexes lead with kind although a query filters only library; inspect actual plans. Preserve pagination and cross-page ordering before changing offset semantics. |
| F-sc-8 | Reject proposed query; keep cost concern | Correction 11 reproduces a real regression: metadata changes delete classification_fts rows but retain media_classifications, so the proposed NOT EXISTS hides updated titles. A rowid point lookup on current FTS membership or an equivalent maintained current-index relation is a candidate, subject to EXPLAIN and stale-classification tests on both backends. |
| F-sc-9 | Keep documentation correction | S8 applies. Durable ownership/session rows are not made safely ephemeral by enabling a cache feature. Retention, restart recovery, quorum loss and publication fences need a separate design before any tier migration. |
| F-sc-10 | Reject both proposed shortcuts | S9 and correction 12 apply. Heartbeat age cannot isolate clock skew, and replay-time SQL clocks break deterministic replication. A time service or timestamp exchange needs explicit uncertainty and unknown-state handling; leader-assigned values must be bound before replication. |
| F-sc-11 | Keep fork ownership; amend absolutes | §4.3 applies. Security fixes can be backported; they are not categorically unavailable because the dependency is forked. The repository already has a synthesized-provenance vendor audit. Gross history insertions are not net patch size; upstream version/support claims require a dated source rather than the July observation. |
| F-sc-12 | Keep lifecycle extraction; amend model | membership.rs mixes several state machines. Joining, role, maintenance, removal attempts and tombstones have overlapping dimensions; the illustrative flat enum is not a proven exhaustive product state. Preserve joint-consensus reconciliation, ambiguity and replicated exclusion before adopting a simplified transition table. |
| F-sc-13 | Amend selector and SQL generation | S10's executed selector finds 3/16 Hiqlite extension gaps and 7/24 SQLite gaps. Current main compile policy is a separate execution gap. Mechanical ?N-to-$N replacement is unsafe because first-occurrence binding differs; a shared SQL representation must generate both placeholders and the matching parameter order. |
| F-sc-14 | Amend | S11 applies. Tuning cache/mmap/temp memory multiplies across connections and needs workload sizing. Blind PoisonError::into_inner does not restore a possibly interrupted invariant. A separate probe table may improve row access but does not shrink the same database's snapshot. Boot integrity checks need bounded operational behavior and explicit refusal/recovery policy. |

### Live TV and library channels — 14 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-ltv-1 | Keep scheduler defect | L1 applies: scheduling and reconciliation consume the response-clipped guide. The size of the entire persisted guide alone does not prove the filtered scheduling window is clipped. Test a late matching airing and already-scheduled late rows against an oversized window; retain full scheduler input and bounded public responses. |
| F-ltv-2 | Amend fence replacement | L2 and correction 9 apply. Store failure can terminate a stream, but config checks carry independent generation/owner/drain authority. Watch channels need every replicated change, not just local saves. Neither indefinitely stale local reads nor unknown-and-continue is automatically equivalent to the existing continuation proof. |
| F-ltv-3 | Keep transport reuse; amend peer cache | L3 applies. Reuse the HTTP client without caching stale credentials or addresses indefinitely. A membership epoch is not shown to change for every reachability/address/capability update; define invalidation, TTL and request-time owner checks. Cold handshake frequency is measurable, not established for every request by constructor count alone. |
| F-ltv-4 | Keep capacity opportunity; expand lifecycle | L4 applies. Transport sharing needs slow-consumer isolation, bounded fan-out, per-viewer cancellation and exact tuner reference counting, including DVR consumers. Sharing FFmpeg additionally requires equivalent delivery plans and independent authorization. This is a feature/capacity redesign, not a small deduplication patch. |
| F-ltv-5 | Keep repeated work; correct lock claim | L5 applies. The generation clone occurs under the cache lock; resolve_occurrence validation occurs afterward. Use an immutable validated generation type or private resolver, not a generally accessible unchecked API. Advance guide occurrences with checked cycle arithmetic and preserve activation boundaries. The 720 MB estimate assumes maximum-size generations on every hit. |
| F-ltv-6 | Amend warm-start optimization | L6 applies. Three seconds is a ceiling, not an unconditional wait. Existing source_format is insufficient to rebuild the delivery plan. Cached full facts need freshness and mux-change handling before bytes are published; an SPS/audio-frame heuristic must handle delayed PMT, multiple programmes, discontinuities and encrypted/unsupported tracks. |
| F-ltv-7 | Keep output-rate mismatch | Q9 applies. Carry deinterlaced output cadence in the plan, bitrate selection and metadata, using rational arithmetic. Do not silently halve temporal resolution merely because output is 720p or software encoding was admitted; make that quality policy explicit and verify capacity. |
| F-ltv-8 | Keep missing proof; reject flags-only confidence | The appendix correctly distinguishes A53 side data from subtitle streams, unlike the stronger consolidated wording. VideoToolbox explicitly disables A53. Caption services, language, preservation through filters and client exposure need real fixtures. Do not invent CC1/SERVICE1 tracks or undo the documented VideoToolbox workaround without proof. |
| F-ltv-9 | Keep mechanical split | L8 applies. Retain the finite/live namespace boundary. Shared HLS parsing must distinguish complete immutable media from moving windows, unknown tags, deletion lag and partial publication; accepting different tag sets does not alone prove accidental drift. |
| F-ltv-10 | Keep architecture reconciliation | §4.7 applies. Record the accepted DVR writer exception, retention/conflict constraints and actual window constant. The shipped DVR decision should be reconciled with old non-goals, not presented as an undecided feature that must be removed. |
| F-ltv-11 | Keep retention defect; amend cleanup handoff | L9 applies. cleanup_session can fail because child exit was not confirmed as well as because directory deletion failed. Always retiring without preserving process ownership can abandon a live child. Separate failed-cleanup tracking from viewer/tombstone capacity and retain retry ownership for both reaping and scratch removal. |
| F-ltv-12 | Keep batching; reject playlist-only gate | Correction 13 applies. One bounded blocking inventory may reduce dispatches, but playlist mtime cannot detect growing temporary files, unexpected entries or delayed deletion. Retain checks for the 128 MiB and inventory limits even when no playlist update arrives. |
| F-ltv-13 | Amend; invalid option and latency origin | Correction 16 applies. No upstream hls_start_time_offset option was found. Program date-time based on mux start is not broadcast capture time, and an EXT-X-START preference does not guarantee identical effective buffering on three clients. Qualify clocks, discontinuities, copy GOPs and actual client offsets. |
| F-ltv-14 | Keep message problem; reject asserted diagnosis | [session lookup](../../crates/plurxd/src/live_tv.rs):4769 cannot distinguish a restart from an expired/evicted or never-valid capability after its tombstone disappears. Say the session is unavailable unless a persisted incarnation proves restart. BeforeEpoch can also mean a legitimately future activation; a fixed one-second retry cannot resolve arbitrary skew and needs a bounded retry contract. |

### Web client — 15 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-web-1 | Keep compression; amend negotiation and counts | W1 applies. Classic scripts execute in order but may be prefetched concurrently, and warm immutable caches change transfer cost. A substring test for gzip accepts gzip;q=0 incorrectly; negotiate weights/wildcards and representation validators. Compression can be scoped to asset routes. Recomputed byte counts differ from the report's snapshot. |
| F-web-2 | Keep cache-version defect | W2 applies. Hash the exact hls.js bytes while preserving its public route and load order. The tests requiring a local file do not themselves require an HTTP route. Registration changes need the asset-table/shell/layout contracts updated together. |
| F-web-3 | Keep missing validators; clarify navigation | Sidecar no-cache responses lack validators. This affects document reloads/fetches, not every hash-router navigation. Preserve native-client fixture/resource dependencies. Either immutable version URLs or correct conditional responses are viable; adding ETag alone without evaluating If-None-Match does not produce 304 responses. |
| F-web-4 | Keep externalization option; correct gzip assertion | The three embedded base64 payloads total 195,868 bytes; gzip reduces them to 148,374, near the decoded WOFF2 total of 146,900. Thus base64 does benefit from gzip despite already-compressed font content. Separate fonts improve discovery/cache granularity; preload only required faces and update the embedded asset/CSS URL contract. |
| F-web-5 | Keep rolling-seek gap; correct title | W3 applies: direct and VOD already seek locally. A buffered-range check needs film/stream offset, retained-publication availability, current attachment and subsequent fetch safety. Local-seek policy belongs in the existing control/seek tests; a blanket 1.5-second holdback is not established for all finite rolling streams. |
| F-web-6 | Keep worker opportunity; qualify fallback | Q10 applies. Worker support/fallback should be exercised in the exact vendored build with CSP and custom loader behavior. Moving TS work is useful but does not prove the stated device jank magnitude. fMP4 conversion is a separate server contract change. |
| F-web-7 | Amend recovery integration | Q10 applies. The existing handler reports fatal failure before fallback, so inserting recoverMediaError afterward without changing state/evidence can recover an already-terminal attempt. Bound and classify media recovery within the shared attempt budget; a reset-per-attach flag alone permits renewed attempts on every reopen. Do not swap audio codec for arbitrary video failures. |
| F-web-8 | Keep defense in depth; amend zero-risk claim | W6 applies. Framing/base/object restrictions can affect supported embedding and asset behavior; cookie auth introduces CSRF/session-origin requirements. Include blob workers if retained, redact token-bearing URLs, and do not treat an esc scan as a proof against all HTML/URL/CSS injection contexts. |
| F-web-9 | Keep global reporting; refine bootstrap | W7 applies. Install a minimal reporter early enough to observe failures before core/api.js loads. Redact bearer URLs, bound/deduplicate events and prevent reporter recursion. A five-second boot sentinel can mislabel a slow connection as a crash; distinguish progress, script failure and a timed-out bootstrap. |
| F-web-10 | Keep incremental types; reject overpromise | W8 applies. The existing ordering/load tests already do more than syntax checking. JSDoc can expose shape errors, not prove generation/lifetime correctness. A no-source-edit proposal conflicts with required annotations/nocheck exceptions, and no-implicit-globals conflicts with the intentional script model unless configured. Preserve load-order gates. |
| F-web-11 | Keep All-grid cost; amend append-only fix | W9 applies. The stable outer shell is not repainted wholesale, but the item region is. Appending new cards is correct only when current sorting/filtering/page ownership preserves order; later items may belong before existing cards. Retain focus, generation guards and deduplication while adding image variants. |
| F-web-12 | Amend heartbeat reduction | W9 applies. A repeated position is not proof the request carries no liveness/activity semantics. Trace consumers, send explicit pause/seek/end transitions and preserve visibility/lease contracts before suppressing heartbeats. The overnight request count and radio impact are estimates, not production measurements. |
| F-web-13 | Keep platform integration gap; qualify behavior | W9 applies. MediaSession and TV input adapters are useful with feature detection and capability-specific actions. Unsupported Remote Playback implementations do not become Chromecast support merely by calling prompt(). Preserve focus/back routing and distinguish play from togglePlay for idempotent media commands. |
| F-web-14 | Keep capability gap; expand probe matrix | Unconditional H.264/container claims deserve evidence. One 4K24 profile/level probe cannot establish every H.264 profile, frame rate or container/codec combination; bare canPlayType(video/webm) is likewise incomplete. Report supported tuples or conservative per-codec limits and preserve rescue behavior on unknown results. |
| F-web-15 | Keep reduced-motion defect; amend deletion | [app.css](../../crates/plurxd/src/web/app.css):147 animates base poster transforms. The catalog/theater overrides also remove the hover transform, not just its transition. Moving only transition into no-preference and deleting both overrides reintroduces motion there. Provide a shared reduce-motion rule covering transform and transition before removing duplicates. |

### Apple client — 12 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-apple-1 | Keep gap; correct API | A1 and §2.8 apply. The proposed AVDisplayCriteria initializer is not the inspected SDK API. Derive valid criteria from the actual asset/format description and clear them on ownership changes. Verify frame-rate and dynamic-range switching on hardware; a source probe is not necessarily the delivered format. |
| F-apple-2 | Keep interruption defect | A2 and §2.10 apply. Observe interruptions and route changes across all three players, preserving user intent separately from system suspension. Resume only when the system permits and the user still wants playback. Background handling must respect audio/PiP policy. The exact six/twelve-second failure stories are plausible paths, not observed device reproductions. |
| F-apple-3 | Keep extraction; reject collapsing independent epochs | A3 applies. Lifecycle, viewer action, attachment, seek and prepared replacement have different invalidation scopes. A value snapshot can make required checks explicit; comparing every field on every continuation can incorrectly cancel valid work. Preserve those scopes before splitting components. File churn does not prove every fix had the same cause. |
| F-apple-4 | Keep observation gap; amend event handling | A4 applies. Live TV already observes timeControlStatus. Extract common observation with item identity, cancellation and observer removal; error-log entries can be diagnostic rather than fatal. Prefer the error accompanying a fatal event and do not turn every PlaybackStalled notification into a terminal transition or duplicate recovery. |
| F-apple-5 | Keep main-actor work; amend pagination | A5 applies. Per-page server ordering is insufficient proof that concatenated pages have the same merged collection order. Preserve multi-library sorting, filters, stable identities and refresh behavior. Moving filtering off actor requires immutable snapshots and result generations; debounce alone does not prevent stale results. Device lag remains unmeasured. |
| F-apple-6 | Keep targeted event conversion; preserve fences | A6 applies. Events do not eliminate stale continuations, missed-registration races or cancellation. A media-data-will-change callback is not proof the sought frame has been presented. Install observation before checking state, resume waiters exactly once and retain deadlines. Backing off status must preserve recovery and prepared-switch semantics, not depend only on panel visibility. |
| F-apple-7 | Keep integration opportunity; qualify effects | A7 applies. The periodic dictionary rewrite and tvOS conditional exclusions exist. An assignment is not evidence of exactly one XPC per tick, and missing these calls does not prove every listed Siri/remote surface is broken. Use event-driven elapsed-time/rate metadata, clear it on teardown and coordinate remote ownership across all player stacks. |
| F-apple-8 | Keep capability investigation; reject unqualified passthrough | Q11 applies. Validate codec/container/profile combinations and exact wire codec names before broadening claims. Apple's [Apple TV audio specifications](https://www.apple.com/apple-tv-4k/specs/) do not establish native DTS-HD/DTS:X passthrough; remove that asserted premise pending device/receiver evidence. Native FLAC or PCM decoding does not prove every proposed HLS packaging path works. |
| F-apple-9 | Keep race; amend actor proposal | Session's node lock does not protect mutable origin/token, which AuthImage reads from a non-isolated context. Protect a coherent snapshot with a lock or use actor-isolated asynchronous access; an actor cannot safely expose a synchronous nonisolated read of mutable state merely by naming it snapshot(). Stage strict-concurrency checking with real diagnostics rather than assuming hundreds of warnings are benign. |
| F-apple-10 | Keep profiling target | Published clock updates invalidate observing views, but full focus-tree rebuild, layer churn and visible jank are unproved. PlayerSurface already checks player identity and has idempotent updates. Narrow view dependencies first; adopting Observation helps only where views stop reading unrelated state. Use Instruments before assigning a performance benefit or claiming focus loss. |
| F-apple-11 | Keep manifest defect; amend replacement | Q7 applies. Source bitrate misrepresents delivered transcodes. Encoder target bitrate alone is not HLS peak BANDWIDTH; include audio and measured/qualified overhead, and distinguish average bitrate. Keep copied streams, prepared replacements and actual output resolution/frame rate correct. A better manifest does not itself measure current network throughput. |
| F-apple-12 | Keep product gaps; require scoped designs | An AirPlay route picker, thumbnail previews and remote scrubbing are separate additions. A documented trickplay architecture does not prove a sprite endpoint is implemented. Preserve seek ownership, preview cancellation, availability checks and remote focus routing; do not introduce a second gesture owner beside the existing input path. Hardware usability and AirPlay delivery need qualification. |

### Android client — 16 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-android-1 | Amend buffer diagnosis | D1 and §2.9 apply. prioritizeTimeOverSizeThresholds=false can permit playback at the byte threshold; it does not by itself prove an eight-second startup deadlock. The small byte cap is a credible high-bitrate resilience constraint. Size against heap class and allocator overhead, preserving startup/rebuffer behavior and process memory headroom. |
| F-android-2 | Keep matching gap; correct API and scope | The cited Media3 ALWAYS strategy is not the inspected C API; Surface's similarly named constant is a different API. Use supported strategy/configuration and test seamless/non-seamless behavior. Excluding all Live TV is unjustified: progressive 24/25 fps and deinterlaced output cadence need appropriate matching too. |
| F-android-3 | Keep lifecycle gap; separate policies | D2 applies. Video background pause, audio foreground-service ownership, audio focus and wake mode are distinct. CPU/network wake mode does not keep the display on. Preserve explicit user pause through lifecycle transitions, PiP policy and service teardown; do not resume unconditionally on foreground entry. |
| F-android-4 | Keep live-window recovery | D3 applies. A behind-live-window error can warrant seeking to the current default position and preparing again. Bound the retry and verify session/window availability; restarting a stopped or expired session under the old capability is not equivalent. Preserve observer ownership and avoid duplicate players. |
| F-android-5 | Keep classification gap; amend rescue | D3 applies. Audio sink/track failures deserve explicit classification, but may arise from route/device state rather than unsupported media. Capture sink diagnostics, distinguish initialization/write/dead-object cases and spend the existing compatibility budget. Do not transcode or reopen indefinitely for a disconnected output device. |
| F-android-6 | Keep bearer disclosure | Handing a reusable account token to an arbitrary external player exposes account authority beyond playback. Use a narrowly scoped, bounded playback capability with explicit revocation/expiry and range/reconnect semantics. A chooser warning is not a replacement for reducing the credential's privileges. |
| F-android-7 | Keep backup exposure; amend storage remedy | D5 applies. Fix both legacy full-backup and modern data-extraction rules. Encrypted-at-rest storage still needs key lifecycle, logout and migration handling; a package-name backup rule is not a valid file exclusion. Excluding only cloud backup leaves device-to-device transfer policy unresolved. |
| F-android-8 | Keep scheduling concern; reject h2c shortcut | OkHttp's five-per-host dispatcher limit applies to asynchronous requests, not all network connections. H2_PRIOR_KNOWLEDGE does not bypass that limit and assumes compatible cleartext HTTP/2. Separate latency-sensitive API scheduling or measure a bounded increase; raising image concurrency above the server's eight-permit gate can increase rejection. |
| F-android-9 | Keep common builder; preserve role differences | D4 applies. Centralize shared focus/noisy/decoder policy with explicit role parameters. Tunneling, cache-only sources, Live TV and background audio do not have identical lifecycle or track requirements. Count and test all construction paths without automatically enabling every option everywhere. |
| F-android-10 | Keep paging cost; correct limit and ordering | The server clamps requests to 200, so asking for 500 does not produce 500-row pages. Advance from actual returned rows/cursors and preserve termination. Demand paging changes whole-library search/filter behavior; define that behavior and maintain global sort order before removing the client merge. |
| F-android-11 | Keep testability gap; amend extraction | D6 applies. Pure helper tests do not instantiate the controller's lifecycle and effects. Introduce a player/control boundary and focused controller tests while preserving distinct epochs, cancellation and main-thread affinity. File size and fix density justify investigation, not proof that a reducer rewrite will eliminate races. |
| F-android-12 | Keep release gap; qualify priorities | D6 applies. Debug deployment, signing and baseline profiles are separate issues. Make the supported distribution path reproducible, retain diagnostics, and protect signing material. Measure release startup/jank before attributing gains to a profile; a profile does not replace tests of the deployed build variant. |
| F-android-13 | Keep blocking initialization; reject unsupported feature premise | D7 applies. OfflineDownloads initialization performs synchronous startup work, but the claim that TV never uses it is not established by an offline-books restriction. Move recovery off the main thread while maintaining process-wide readiness for services and background work. Initializing only when a downloads screen opens can strand pending downloads. |
| F-android-14 | Amend polling reduction | HLS status uses a session capability; calling every poll an account-authenticated request is inaccurate. The status path participates in failure/recovery behavior beyond the info panel. Back off only with a replacement detection bound. The stated radio-cost reduction needs measurement and cannot be inferred from request count alone. |
| F-android-15 | Keep overbroad failover | D3 applies. Inspect the response code carried by the HTTP exception before choosing node failover. Authentication, missing resources and retired capabilities need their own handling; trying peers can amplify load and obscure the original cause. Keep transport failures and genuinely retryable server responses inside a bounded owner-aware policy. |
| F-android-16 | Keep incomplete capability matrix; amend defaults | A 30 fps probe and omitted profiles do not describe all supported tuples. Test rate/profile/level/bit depth and negotiated output, including 4K24 versus 4K60. Do not impose blanket 1080p limits or claim audio/container support solely from decoder-name enumeration. Preserve conservative rescue for unknown devices and distinguish native decoding from passthrough. |

### Build, operations and code health — 15 findings

| ID | Verdict | Assessment and required disposition |
|---|---|---|
| F-build-ops-codehealth-1 | Keep execution gap; correct Clippy claim | §2.2 and §4.4 apply. Main's fast lane omits the full Rust unit suite, but does run compilation and vendor Clippy; the tracked hook runs workspace Clippy. The repository explicitly assigns focused local regression duties. Restoring automatic full-suite execution is a policy change, and the suggested two cargo commands are not equivalent to make ci-rust-gate's feature/backend coverage. |
| F-build-ops-codehealth-2 | Keep product recovery gap | S4 and §2.3 apply. The activated Hiqlite deployment lacks the documented portable backup/restore workflow. Existing storage snapshots are not a substitute. Define encryption/key retention, schema/version compatibility, off-node copies, restore identity/fencing and a tested recovery objective before treating the proposed environment variables as a finished solution. |
| F-build-ops-codehealth-3 | Keep seam audit; reject 681 count | §4.2 applies. Subtracting top-level modules from all cfg(test) occurrences does not count shipping functions with alternate implementations. Categorize test modules, helper types, fields and actual lifecycle branches separately. A production-shaped integration harness must retain process, admission and ownership semantics without removing deliberate test isolation indiscriminately. |
| F-build-ops-codehealth-4 | Keep runtime mismatch | §4.6 applies. A distribution ffmpeg 6 installation cannot qualify the shipped patched ffmpeg 8 capabilities. Use the exact supported runtime for behavior tests and explicitly retain any older-runtime compatibility lane. Binary version alone is insufficient where filters/decoders and patched behavior matter. |
| F-build-ops-codehealth-5 | Keep fork governance; correct metrics | S11 and §4.3 apply. Added-line totals are gross history churn, not the current fork delta. Existing vendor audit restores registry provenance for advisory checking; it does not prove patches are secure or upstreamed. Assign ownership, maintain a reproducible delta and test upstream rebases without claiming security fixes cannot be backported. |
| F-build-ops-codehealth-6 | Keep feature pruning; credit full proposal | §4.3 applies. Unlike the summary, the appendix explicitly removes the unconditional cryptr s3 feature, which is necessary. Verify the whole resolved graph and optional backup/S3 combinations after changing defaults. Choosing ring and banning aws-lc is a provider decision requiring a supported feature matrix, not merely deleting duplicate package names. |
| F-build-ops-codehealth-7 | Keep optional build boundary; preserve shipped compilation | Correction 17 applies. Turning semantic search off in the fast lane while retaining it in Docker creates a shipped-feature compile gap unless another blocking check covers it. Bound inference concurrency without unexpectedly reconfiguring a shared global Rayon pool. Package size and idle RSS gains require build/runtime measurements. |
| F-build-ops-codehealth-8 | Keep resource policy; reject universal settings | §4.6 applies. Missing explicit unit/Compose limits does not establish the fleet's inherited limits or measured exhaustion threshold. CPU priority, OOM selection and cgroup group-kill behavior differ. Validate helper permissions, platform support and pre_exec async-signal safety; do not allocate or perform arbitrary file/logging work after fork. Qualify each hardening directive with hardware and discovery access. |
| F-build-ops-codehealth-9 | Keep gate simplification review; reject blanket deletion | §4.4 and §4.7 apply. Ledger-touch counts overlap product changes and are not a measured third of engineering time. Some text contracts encode security, policy and cross-file consistency absent from runtime tests. A Markdown link checker cannot enforce document inventory or status claims. Replace each gate only after mapping the invariant to an equivalent check; opt-in regression metadata changes repository policy. |
| F-build-ops-codehealth-10 | Keep typed-FD migration; amend scope | Raw unsafe counts include tests and do not measure undocumented safety obligations one-for-one. A transitive rustix dependency still needs a direct dependency/feature declaration. Preserve macOS/Windows support and symlink/mount constraints; Linux openat2 is not a portable drop-in. Retain async-signal-safe child setup and test the actual filesystem threat model. |
| F-build-ops-codehealth-11 | Amend invalid configuration and claimed wins | Correction 18 applies. The TOML snippet's semicolon-separated assignments do not parse; stripping debuginfo removes the requested line tables. Fat LTO/CGU=1 and overflow checks change build/runtime tradeoffs and require measurement/testing. A custom ci profile is unused without selecting it. Pin the image consistently and verify linker availability on each supported target. |
| F-build-ops-codehealth-12 | Keep logging gap | F-core-12 applies. tracing-subscriber defaults confirm ANSI without TTY detection. Add a selectable structured format and panic capture with bounded backtraces/redaction, recursion protection and deliberate default-hook chaining. A panic hook reports failure; it does not ensure a detached task's lifecycle effects are repaired. |
| F-build-ops-codehealth-13 | Keep parser fuzzing expansion | The fuzz manifest contains only the PGS target. Add bounded callable parser seams, representative valid/invalid seeds, allocation/output/time limits and regression promotion. Filesystem/container inputs need safe harnesses. A scheduled campaign is a CI policy addition; ten minutes per target is a budget proposal, not proof of meaningful coverage. |
| F-build-ops-codehealth-14 | Keep staged decomposition; amend mechanical label | §4.1 and §4.2 apply. Extracting inline tests can reduce navigation cost without changing product behavior, but moving subsections is not a whole-file git mv. Migration steps include repairs and transaction semantics that a static SQL table must preserve. CGU/cache gains and safety from a 300-line lint are unproved; isolate moves from semantic redesign. |
| F-build-ops-codehealth-15 | Keep release provenance work; reject no-rollback claim | §4.5 applies. Fleet publishing already retains the ten newest immutable sha-<12hex> image tags, so rollback artifacts do exist. Semantic versions/changelogs improve communication but do not replace current-candidate qualification. Restricting the deployment alias to release publishing and imposing weekly releases are policy changes; retain a qualified immutable rollback target. |

## Appendix narrative, positive assessments and open questions

The introductory architecture maps, timelines, hot-spot tables, command examples
and conclusions were included in the review. They do not add another independent
set of defects to the 128 findings. In particular:

- **History and twelve incident narratives:** all 84 distinct eight-character
  commit references resolve and are ancestors of the reviewed tree. The incident
  bodies support the cited placeholder, migration, probe-pipe, membership and
  rendering mechanisms. They do not prove the universal claims about detection
  by humans, every startup failing, or every fix having one architectural cause.
  For example, the UUID mismatch affected 15/16 generated version-4 IDs, not
  every possible ID; an incident bundle also includes fixture defects alongside
  runtime defects. The weekly/product totals and test/production partition need
  the corrections in §1. The receipt-category counts total 630, not 631.
- **Streaming argument recipes and latency model:** these are useful path maps,
  not qualified replacement commands or measured latency budgets. Carry forward
  exact selected streams, held-file identity, encoder/build facts, timestamp
  origins, HDR metadata, caption services and output-cadence constraints. The
  per-family recommendations inherit the Q-series and F-stream dispositions;
  replacing a flag is not sufficient proof of output compatibility.
- **Server route/auth inventory:** the listed extractor examples support the
  architecture, but the table is not an exhaustive audit of all nested handlers,
  capability-authorized paths, setup and internal routes. Do not infer “no auth
  gaps anywhere” or “every mutation uses the same extractor” from that table.
  The positive bounded-state and lock claims are scoped observations, not proofs
  covering all runtime paths. BoundedReplica still needs route eligibility and
  proof-loss handling; enabling it globally is not the complete change.
- **Performance and cross-product comparisons:** request counts, static buffer
  budgets and source sizes are distinguishable from measured latency, jank,
  battery use and outage frequency. References to Plex/Jellyfin/Infuse/Swiftfin
  are design comparisons, not evidence that their policies fit this ownership
  model. Proposed “low risk” sizing omits several lifecycle and migration
  obligations recorded in the ledger.

All **95 numbered “Already good” items** were considered. Retain the named
mechanisms, with the following qualifications; this is not permission to remove
the invariants as part of an optimization.

| Appendix area | Items covered | Disposition |
|---|---|---|
| History | 1–10 | Retain forensic commit bodies, migration/placeholder censuses, qualified splits, patch records, mobile version checks and documentation organization. An ignore reason does not prove a test was never silenced improperly; growing test counts do not prove execution or effectiveness. Historical fleet measurements remain measurements of their recorded runs, not current qualification. |
| Streaming | 1–11 | Retain encoder bounds/forced IDRs, metadata typing, copy hygiene, multichannel AAC, rational VOD timing, process ownership, wait-pool admission, pacing and executable-keyed facts. Narrow universal supervision/output claims to the inspected paths: the missing-pipe helper defect is itself an exception. Physical validation and no-reorder invariants constrain the proposed B-frame and caching changes. |
| Server core | 1–10 | Retain blocking-store isolation, short scoped locks, bounded maps, explicit extractors, secure paths, batched browse, shutdown bounds, body limits, single-flight jobs and consistency metrics. The source review is not a proof of “no locks across await anywhere” or complete authorization coverage. Request-span metrics and route/body limits also do not supply the missing header timeout. |
| Store/cluster | 1–11 | Retain migration guards, progress coalescing, batched renewals, fenced publication, bounded list parameters, local telemetry, quorum-watermarked catalogue reads, TimedClient, backend contracts and replay-safe activation. These mechanisms do not prove that every selector executes their tests, every transaction is deterministic, or snapshots are safely offloaded; the respective findings remain. |
| Live TV/channels | 1–12 | Retain owner proofs, atomic inventory publication, evidence-based startup budgets, uniform cadence, guide persistence, typed delivery plans, process/URL hygiene, bounded relays, encoder constraints, namespace separation, capacity accounting and staged channel publication. “Exhaustive” planner confidence is limited to represented inputs and fixtures. These rules are constraints on sharing, inventory and fence optimizations. |
| Web | 1–10 | Retain immutable assets where actually versioned, load-order gates, native-HLS detection, conservative capability proofs, teardown, buffer budgets, typed refusals, render generations, escaping/redaction and accessibility/pure-policy tests. hls.js and sidecars have the stated versioning exceptions. Escaping conventions do not prove all injection contexts safe; passing pure tests does not prove effectful player recovery. |
| Apple | 1–10 | Retain image bounds, decision snapshots, qualified HLS authoring, limited recovery, explicit track selection, Keychain/logout rules, synchronized overlays, reopen serialization, foreground-aware view polling and release fixtures. “Address+token written together” does not resolve Session's unprotected cross-context reads. Overlay seek safety depends on the existing attachment/selection fences, not animation type alone. |
| Android | 1–10 | Retain shared pure fixtures, surface choice, route-aware capabilities, finite recovery budgets, media-origin epochs, cancellation-aware teardown, credential hygiene, explicit subtitles, keyed UI and TV-focus layout. The main player builder's good defaults do not cover the other builders. Launching cleanup in viewModelScope does not guarantee remote DELETE delivery across process death. |
| Build/ops | 1–11 | Retain lint/toolchain/action pins, feature discipline, runtime image assertions, bounded caches, targeted dev profiles, vendor provenance audit, readiness semantics, fixed-label metrics and deployment rationale. The universal “every child/no unbounded output” statement is too broad; helper wait_with_output paths need their own bounds and pipe review. Production LOC and release artifact claims need the numerical corrections above. |

All **53 numbered open questions** also have a disposition. “Measure” below
means a concrete remaining fleet/CI/device investigation, not an unanswered
source-code review item or a claim that an experiment was performed.

| Appendix area | Disposition of every question |
|---|---|
| History (1–6) | **1:** measure DV-P5 exposure and build refusal logs. **2:** obtain installation receipts; build counters are not installations. **3:** reconcile supported SQLite/recovery modes in product policy; code supports a standalone path. **4:** inspect sustained campaign receipts, not a single PR result. **5:** consumer existence and receipt usefulness differ; the category count itself needs correction. **6:** distinguish implemented finite VOD and retained rolling paths, then measure actual path selection; stale M9 prose is not reachability evidence. |
| Streaming (1–6) | **1:** count producer generations under a defined demand/hold trace; “every beat” is not established. **2:** inspect actual hardware-download side data. **3:** test delivered BANDWIDTH effects on constrained Apple playback. **4:** no-reorder is an explicit parser/timeline invariant, so determine the work to support composition offsets before attributing it to an undocumented client bug. **5:** instrument probe/attest/spawn/first-publication TTFF separately. **6:** inventory deployed encoder families before assigning fleet severity. |
| Server core (1–6) | **1:** measure authority-read percentiles by backend and role. **2:** inspect deployed bounded-read configuration. **3:** establish proxy/TLS/compression topology. **4:** measure scan runtime contention on actual hardware; one blocked worker does not imply a fixed 25% throughput loss. **5:** original backdrops are already a documented deliberate quality policy. **6:** establish actual Plex client use and pagination requirements without assuming discovery-only service. |
| Store/cluster (1–6) | **1:** measure DB size and snapshot duration during activity. **2:** inspect deployed configuration, not defaults. **3:** preserve distributed atomic claiming while deciding scheduler placement. **4:** upstream version/patch disposition needs a separate current upstream check; the July note cannot establish it. **5:** measure comparable authority/local read distributions before choosing a cache. **6:** add explicit concurrent-playback, ENOSPC, skew and large-database drills; existing cluster tests do not imply this coverage. |
| Live TV/channels (1–6) | **1:** measure authority loss and renewal recovery; a single request budget is not the whole fence timeline. **2:** inspect the filtered scheduler window as well as full guide bytes; a large persisted file alone does not prove clipping. **3:** capture field order on real muxes, including unknown/change cases. **4:** measure post-fix latency/stalls on each client and network. **5:** shared viewing is a capacity/product decision. **6:** encoder help shows options, but preserving actual captions requires an end-to-end fixture beyond help output. |
| Web (1–5) | **1:** inspect deployed proxy/HTTP/compression behavior. **2:** a current hls.js release comparison is separate dependency maintenance; preserve loader/subtitle internals and versioned assets during any bump. **3:** measure actual SourceBuffer pressure on target mobile devices. **4:** define supported TV browsers before input/capability guarantees. **5:** choose an image authorization contract and preserve logout/cache invalidation; signed URLs and cookies impose different expiry/origin requirements. |
| Apple (1–6) | **1:** test match settings on hardware. **2:** preserve the recorded SDR-master compatibility ruling until exact strings and devices are requalified. **3:** the premise is incomplete: synthesized finite VOD and blocked segment production already exist; this is not an entirely unevaluated server design. Determine remaining rolling-route eligibility before promising deletion of client lifecycle code. **4:** reproduce interruptions and route changes. **5:** scoped direct-play capabilities are a separate acknowledged credential-boundary improvement. **6:** do not assume DTS-HD passthrough from tvOS version; qualify the actual decoding/container/output chain. |
| Android (1–6) | **1:** obtain installed build provenance. **2:** record actual heap classes and memory behavior. **3:** qualify supported Media3/display APIs; the proposed ALWAYS constant is not valid in the cited API. **4:** test sustained background audio and service/lifecycle behavior. **5:** reproduce the documented tunneled prepared-replacement case with fallback timing and attempt ownership. **6:** h2c availability does not remove OkHttp's asynchronous dispatcher limit; correct that premise before transport experimentation. |
| Build/ops (1–6) | **1:** inspect full-gate run receipts on the exact tree; workflow configuration alone cannot answer execution history. **2:** inspect deployed service/container overrides. **3:** measure release size and representative RSS. **4:** check upstream patch submissions separately; absence of links is not proof of no submissions. **5:** measure hook use if useful, but lack of hook execution would not erase vendor Clippy or other independently run checks. **6:** Windows support is an owner/product decision; runtime/cross-build cost is evidence for that decision, not proof of accidental scope. |

## Verification record

- Read all 699 lines of the consolidated document and all 3,513 appendix lines,
  including all nine reports' narrative, positive items and open questions.
  Checked each finding against its cited implementation or relevant policy.
  Verified that the document pull did not change the reviewed implementation.
- Inspected the cached axum/hyper-util/Tokio/cryptr/OpenRaft dependency source
  and the installed tvOS 27 SDK header where API/default claims mattered.
- Consulted primary Apple, Android/Media3 and FFmpeg references for platform
  claims; device effects remain explicitly unmeasured.
- Executed the actual CI path selector for all 16 Hiqlite extension modules
  and all 24 SQLite modules, rather than counting literal catalogue paths.
- Recomputed maintained-document indexing against the cited Git tree,
  selected history counts, source-file lengths, test-attribute categories and
  web payload sizes. Definitions and differences are recorded above.
- Reproduced F-sc-8's proposed search regression in an in-memory SQLite database
  using the actual classification schema/triggers: after a title change, the
  current query returns the item while the proposed media_classifications
  exclusion drops it. Inspected the replicated-SQL deterministic-clock rule
  for F-sc-10 rather than accepting a replay-time unixepoch() substitution.
- Resolved all **84 historical commit references** and checked ancestry against
  the reviewed tree. Inspected the principal incident bodies, not only titles.
- Recomputed embedded-font base64/gzip sizes, rejected the release-profile
  snippet with Python's TOML parser, and checked upstream FFmpeg 8 HLS source
  plus installed FFmpeg 9.0.1 muxer help for the proposed nonexistent option.
  The deployed Jellyfin build was not available for a fleet-runtime check.
- The coverage census matched **82 consolidated entries and 128 appendix
  findings exactly once** in their respective ledgers, with no missing or
  duplicate IDs. Dedicated assessments cover the remaining consolidated
  sections, all **95 positive items**, and all **53 open questions**.
- All **174 local Markdown links** resolve, and this assessment has a row in
  the documentation index. `git diff --check` passed for the changed index.
- `python3 -m unittest tests.operations.test_docs_index`: **4/4 passed**.
  Because the new assessment remains untracked, the same suite was also run
  with its path included in tracked_files via unittest.mock.patch: **4/4
  passed**, checking its inventory entry and references without staging it.
  Unrelated files were not altered.
- No production/Rust suites or fleet experiments were run; those are not
  implied by the source-level verdicts in this assessment.
