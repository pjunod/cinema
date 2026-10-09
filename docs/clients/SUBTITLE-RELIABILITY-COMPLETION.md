# Subtitle reliability — completion ledger

**Status:** WebVTT/offline implementation integrated; UI snapshots complete; A/53 file-caption repair linked to its active video-processing effort; the sole adversarial review is complete, all three corrections are integrated, and final validation is running.
**Updated:** 2026-10-09 · **Initial base:** `1088d7529` · **Current main integrated:** `0c4ae0aa5` · **Integration:** `codex/subtitle-timeline-and-window` · **Batched PR:** [#961](http://forge.lan:3000/noirr/plurx/pulls/961)

Cinema subtitles fail across clients: selection may produce nothing, cues can
arrive late, and a selected track can stop displaying. WebVTT is already
implemented; the remaining defects concern when its bytes become available,
what playback engines retain, and which subtitle timeline they follow.

This ledger owns the current repair and reconciles the older subtitle plans.
It supersedes their stale build/PR status, not their unearned acceptance.
The user authorized testing on available hardware except the physical Apple TV
and iPhone 18. Neither excluded device is used for this effort.

## 1. Confirmed defects and repairs

| Defect | Evidence | Repair |
|---|---|---|
| All three readiness reporters require seeing warming before ready | Actual macOS AVPlayer buffers five empty VTT segments; old source observes ready first and renders zero cues for 22 seconds while video advances | Retry the first ready for the current selection/seek intent, then suppress repeated identical ready observations |
| Ready window and ready complete track are indistinguishable | Focused server test publishes a window, a different window, then the complete track without a warming edge | Optional opaque `delivery.subtitle_revision` identifies the source/track/window representation; a new revision authorizes one retry |
| Media3 off/on retry can be coalesced before the playback thread consumes it | Physical Android: old retry fails 2 of 5 runs, five empty reads, zero ready reads, text still selected and video advancing | Wait for `onTracksChanged` to acknowledge text deselection; then restore the still-current selection. Cancellation/timeout cannot strand the current intent disabled |
| Web disables native captions while waiting for a whole-track sidecar even when only a window is ready | Recovery path inspection and browser regression | Retain the native rendition until usable script cues arrive; deduplicate pending requests and fence completion by intent |
| Subtitle playlists discard video discontinuity tags | New takeover/sliding-playlist regression fails against the old generator | Preserve discontinuity sequence and every segment boundary in the VTT rendition |
| Partial browser takeover stops the HLS requests that prepare subsequent windows | Bundled hls.js track ownership and the partial-only regression identify the stopped requester | Keep the native rendition selected but hide its display while script windows render; retire it only for a complete track |
| Failed subtitle HTTP 503 blocks otherwise healthy video | Production setting was enabled; actual AVPlayer stays at zero through 18 seconds of permanent 503, while the empty-200 control advances; Media3 survives | Retire the unsafe experimental transport option and report unavailable captions once per current intent through playback control |
| Nested subtitle extraction futures overflow the debug worker stack | Actual embedded-caption app run aborts; three measured poll frames total 2,584,560 bytes against a 2,129,920-byte worker stack | Heap-own the existing bounded 64 KiB decoder-copy buffer; preserve byte limits, cancellation and child reaping |
| A session can join the wrong extraction after a language/source change at the same playhead | New regression holds the old window flight, changes track or source stamp, and observes the new publication | Include complete cache identity in the session window owner; displace obsolete work |

The first-ready mechanism is demonstrated against the old and new Apple
source; Media3's additional race is demonstrated on a physical Pixel Fold.
These findings explain concrete failure modes across clients. Retained fleet
logs do not identify every selection from the reported evening, so this is
not a claim that every reported failure had only one cause.

`subtitle_revision` is an optional, opaque string. Clients compare equality
only. Absent and unknown readiness values never imply readiness. Old peers
may omit the revision; first-ready recovery still works. The server observes
cache state without starting extraction from a control request. A full-track
revision stays stable as the playhead crosses windows. Off, newer selection,
newer seek, replacement and teardown continue to fence delayed work.

## 2. Local evidence

| Check | Result |
|---|---|
| Rust 1.97.1 baseline and modified `cargo check -p plurxd --all-targets --offline` | pass |
| Pinned `cargo clippy -p plurxd --all-targets --offline -- -D warnings` | pass |
| `cargo test -p plurxd --bin plurxd --offline subtitle` | 178 passed |
| Apple playback-control reporter/session XCTest on iPad simulator | 87 passed |
| Android playback-control, subtitle-policy and acknowledgment JVM tests | 177 passed |
| `node tests/playback/native-hls-startup.test.js` | 39 passed |
| `scripts/subtitle-readiness-browser-check` with real Chrome/hls.js | zero cues before readiness; visible cue after; same video element and session |
| `scripts/subtitle-readiness-native-check` with actual macOS AVPlayer | old source fails with zero cues; fixed source refetches and continues through cue 21 without replacing the item |
| `SubtitleReadinessRenderingTest` on physical Pixel Fold, isolated application | old retry fails 2/5; acknowledged retry passes 10/10 |
| Native WebKit HLS, selected empty rendition followed by publication | refetches and presents cues 6–19 through 20 seconds, same video element and no media error |
| Physical iPhone 17 Pro Max, isolated AVPlayer probe | empty rendition → first-ready → continuing visible cues; Off hides cues; seek/reselect restores cue 20; same item and no error |
| Physical Pixel Fold PGS Compose renderer | corrected isolated-test manifest; actual overlay rendering check passes |
| Permanent subtitle 503, actual engines | AVPlayer stays at zero for 18 seconds; hls.js escalates to fatal `fragLoadError`; Media3 continues. Empty-200 controls continue without media errors |
| Mac E2 analytic rendering corpus | 96 raw frames each in SDR, HDR10 and HLG; ASS anchor/fade/motion/gap and PGS color/alpha/palette/clear/EOF controls pass; nine active/blank seeks and bounded text/bitmap cancellation pass; [retained receipt](SUBTITLE-MAC-COMPOSITION-EVIDENCE.md) |
| Actual whisper.cpp 1.9.2 with local tiny.en model | held source/model descriptors and cleared environment produce valid timed WebVTT from synthetic speech on CPU |

Reproduce the native fixture with `python3 scripts/subtitle-readiness-native-check`.
For Android, start that command with `--serve`, reverse its printed localhost
port with ADB, and pass `-e subtitleFixture http://127.0.0.1:PORT` to the
instrumentation runner. Build with `-PplurxIsolatedBudgetProbe=true` and install
only the `capabilityProbe` application and test packages. The fixture creates
synthetic media, uses no library credentials and does not replace the viewer's
installed Cinema application. The test is skipped without an explicit fixture.

The synthetic harnesses prove decoder/renderer recovery. They do not replace
an end-to-end test of a deployed server, UI selection, seeks and quality
handoffs against actual titles. AVPlayer testing also showed that answering
503 while captions warm can stall video; a blanket 503 change is not a fix.

## 3. Project reconciliation and remaining work

| Work | Current state | Completion requirement |
|---|---|---|
| [Original reliability handoff](SUBTITLE-RELIABILITY-HANDOFF.md) | Earlier implementation is on main; residual defects above were missed | Integrate repairs; repeat selection/off/seek/quality cases on available clients |
| [Cluster extraction](SUBTITLE-CLUSTER-EXTRACTION-STATUS.md) | M0–M5 built in #507 | Fleet cold/warm/off/slow, peer hydration and backfill observations |
| [Parallel ranges](PARALLEL-SUBTITLE-RANGES-STATUS.md) | #517 landed as `38f61dfe6`; its pending-PR status was stale | Current/next ranges and complete-track takeover stay visible without a video restart |
| [Online downloads](../features/SUBTITLE-DOWNLOADS-IMPLEMENTATION.md) | #498 landed as `0e2c3fd47`, implementation `843ed9bb5`; not an unbuilt feature | Provider configuration/quota smoke where configured; durable caption playback on clients |
| [PGS startup and overlay](PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) | Two-device startup bar passed September 24; broader corpus still open | Color/geometry/alpha, intervals, active-cue seeks and late data across clients |
| [VideoToolbox file/DVR A/53 captions](../streaming/VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md) | Owned by the active `effort/video-processing-followups` integration: SEI parser repair `622c1ee42` plus source-clock repair; retained normal-API caption cases pass there | Preserve the existing effort ownership and passing evidence; final qualification/landing there remains a dependency, not a completed claim for this PR |
| [Mac E2 subtitle processing](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md) | CPU composition after native processing is integrated; HLG text/bitmap checks exist | Reconcile E2 acceptance; GPU composition is conditional on measured benefit, not a required second renderer |
| [Offline transcription](../performance/PERF2-PLAN.md#102-subtitles-on-demand--the-whisper-queue) | Implemented in `6fae0a350`, `8df243175` and `718e35a47`; separate from online downloads | Bounded offline command job, Stop/lease ownership, durable caption provenance, settings and tests |
| [Physical verification](SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) | Historical cases remain useful; build numbers and PGS refusal expectations are stale | Record each current case separately; excluded hardware stays untested |

### Offline transcription implementation boundary

The optional worker uses the existing durable queue and acquired-caption
store. A single transaction validates the active claim, source revision and
demand, then publishes WebVTT/provenance and settles the job/waiters. Stop,
source replacement or a lost lease cannot publish stale captions. No schema
migration or library-side SRT file is needed.

CPU execution uses two threads and one processor; GPU acceleration is a
future optimization. The installed local model is at most 2 GiB. Probed
sources and inference are bounded to four hours, audio conversion to 30
minutes, temporary mono audio to approximately 460 MiB and published captions
to 256 KiB. Queue admission allows at most 32 active transcription jobs;
automatic discovery checks eight files per page. The existing eight-caption
limit per source revision still applies. No model download or translation
runs inside the daemon. Matching-language captions suppress automatic work;
manual generation remains available for a better alternative. The worker selects
matching spoken-language audio (default first, then lowest audio ordinal),
falling back only to untagged audio. Known language mismatches are refused.
The chosen ordinal is bound into job identity, provenance and publication.


## 4. Current coordination and next gate

The October 9 owner instruction supersedes the older per-task effort gate
and full-suite promotion workflow for this batch: keep proper commits in one
main-bound PR, obtain one adversarial agent review only when the whole batch
is ready, address it, then run the fast lane. Preserve passing evidence for
unchanged code and rerun only failures. Another process owns broad unit-suite
failure batches. The earlier effort dispatch was cancelled; no task gate is
needed for subsequent build commits.

All work now uses independent agent clones in temporary storage. The user's
checkout is not a work directory. The prior managed worktree archive was
refused by the app because the task/workspace is pinned; its code is preserved
in the independent clone, and it is no longer used. Cleanup remains tracked.

| Owner | Assignment | State |
|---|---|---|
| Coordinator | Initial root-cause repair, integration, status, Developer UI, final review/gate | Initial repair committed as `46a15c280`; PR is draft |
| Sol 6.1 · window delivery | Serve already-ready bounded WebVTT to browser recovery without waiting for whole extraction; local/shared parity | integrated as `25cbac3c7` (agent commit `979621d26`); final batched validation pending |
| Sol 6.1 · offline transcription | Bounded command job, queue/Stop ownership and durable caption provenance | integrated through `718e35a47`; matching audio ordinal bound to job/publication; normal pinned hooks passed |
| Sol 6.1 · acceptance | Reconcile PGS/Mac/download/cluster evidence, complete available-hardware cases and repair concrete gaps | unsafe-503 retirement/native notices integrated as `4f6a1d7e2`; physical and bounded Mac composition checks passed; remaining replicated-store/app acceptance prepared |

UI generation is complete: 78 captures, 10,849 structural facts and no
console/page errors. It also exposed a canonical decimal file-ID mismatch in
client logging; `20ef21267` accepts the existing web spelling without losing
64-bit precision. The future-window browser fixture is prepared. The sole adversarial agent
review completed on candidate `f717f7c08`; all three findings below are corrected. Named cluster and new worker regressions run after
that review alongside the fast lane. The live online provider is unconfigured;
no successful provider request is claimed.
No production deployment or completion of the whole backlog is claimed yet.

### Passing-test preservation

Passing named unit tests are retained and must not be run again. The earlier
39-pass web receipt is the native-HLS startup suite, not the complete control
suite. Remaining checks exclude those names even when another suite imports
them. New worker/window/failure-notice tests were deferred until the sole review completed; their outcomes are recorded below.

The current main fast lane unconditionally runs `make web-unit-check` in its
web compile job, which conflicts with this batch's explicit no-repeat
instruction. The PR remains draft while the validation route is resolved;
no duplicate full web suite has been launched and no green gate is claimed.

### Existing caption-processing effort

P06 is not silently closed. Read-only inspection of the active video-processing
effort found its already committed caption-preserving SEI parser repair and
normal-API evidence, including a separately repaired file-source clock.
Those changes remain owned by that effort; rebuilding or copying a partial
subset here would duplicate work and omit coupled timestamp fixes. The
subtitle ledger tracks its final qualification and landing as an explicit
dependency. The current batch repairs client WebVTT delivery and implements
the previously missing offline generation path.

### Final review dispositions

One independent adversarial agent reviewed the complete candidate
`f717f7c08` without running tests. No second review is planned.

| Finding | Correction owner | Current disposition |
|---|---|---|
| P1: control readiness comes from the session owner, but the cached VTT file route reads only ingress-local cache | Window-delivery builder | Implemented session-owned cache delivery and shared receiver/Source mapping; real signed remote-owner regression written, integrated as `de7c724fa`; pinned compiler and normal hook passed; named owner-route regressions running |
| P2: PCM conversion discards initial audio offsets and timestamp gaps, shifting generated captions | Transcription builder | Implemented source-clock-normalized PCM with bounded leading/gap silence and overlap handling; real offset/gap diagnosis and pinned compilation passed; integrated as `0816ed580` |
| P2: FFmpeg can emit decode errors yet exit zero, allowing partial captions to publish as complete | Transcription builder | Implemented strict decoder errors plus error-level stderr veto before inference/publication; integrated as `0816ed580`; new regressions await final validation |

All three corrections are integrated. Remaining unit execution is now running against `de7c724fa`, preserving every earlier passing case.
The retained pass ledger contains 503 named results; retries must exclude
every passing case. The existing CI web job cannot consume these local
results, so local qualification must remain explicitly identified and never
be represented as an actual green Main promotion gate.


### Final validation progress · October 9

- Apple: all five new failure-notice ownership cases pass; Android: both new
  cases pass. The earlier 87 Apple and 177 Android passes were not repeated.
  tvOS simulator compilation also passes. The retained native ledger now has
  510 total named passes across its previously recorded groups.
- Actual Chrome/hls.js future-window fixture passes: cached bytes follow the
  attached session namespace; a seek to 72 seconds requests segments 24–27,
  visible script captions continue while native display is hidden, and only
  complete-track takeover retires the HLS subtitle requester. The same video
  and session remain attached, with no media error.
- The remaining Node and Python fast-lane cases and new server/storage
  regressions are being qualified. The first Node exclusion filter matched
  bare names, while Node supplied names with a file prefix, so 43 prior cases and 19 newly passing cases were accidentally repeated
  (62 duplicate executions in total). This violated the owner
  instruction; raw journals are retained, repeated cases are excluded from
  new coverage counts, and the corrected runner guard was verified with registration-only synthetic cases before further use.
- Node qualification is complete across 63 entry scripts: 1,193 unique new
  named cases plus 21 file-wrapper outcomes. Python has 388 new validation
  passes and 853 new operations passes; one Linux-only method remains a
  platform skip. Python journals contain no repeated successful IDs.
- Seventeen of 18 new daemon regressions pass, including real audio clock
  preservation and decoder-error rejection. The remaining remote-owner
  fixture needed to reuse the startup server's retained listener; it is
  corrected and awaiting its failure-only retry. All four core/storage
  checks pass, including the real three-voter publication and Stop-fencing
  contracts; their passing executions will not be repeated.
- The real embedded-caption app check failed: the debug daemon aborted with
  a Tokio worker stack overflow at `ensure_window_owned` while starting a
  window extraction. Its crash report and failed receipt are retained;
  nested poll frames exceed the worker stack because they embed the decoder
  copy buffer. The buffer now has fixed-size heap ownership; the failed app
  scenario awaits the rebuilt daemon. No stack increase or retry watchdog
  is used.
- The independent generated-caption app check passes end to end: actual
  queue completion, durable WebVTT/provenance, restart persistence, and a
  visible caption in Chrome. That passing scenario will not be repeated.
  It exposed a separate selected-track count error; the corrected count
  passes its one new focused regression after a fixture-only repair.
- Local evidence does not claim a completed CI gate or a production deployment.
